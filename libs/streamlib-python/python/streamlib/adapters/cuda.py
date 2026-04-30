# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""CUDA surface adapter — Python customer-facing API.

Mirrors the Rust crate ``streamlib-adapter-cuda`` (#587 / #588). The
Python subprocess delegates to ``streamlib-python-native``'s
``slpn_cuda_*`` FFI surface, which itself wraps
``CudaSurfaceAdapter<ConsumerVulkanDevice>`` plus
``cudaImportExternalMemory`` / ``cudaImportExternalSemaphore`` against
the host-allocated OPAQUE_FD ``VkBuffer`` + timeline semaphore.
Per-acquire control flow:

1. The Python SDK looks the host's pre-registered cuda surface up via
   surface-share once (``slpn_cuda_register_surface``). The OPAQUE_FD
   memory + timeline FDs enter the cdylib's address space, get
   imported via ``ConsumerVulkanPixelBuffer::from_opaque_fd`` /
   ``ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd`` AND
   re-imported into CUDA via ``cudaImportExternalMemory`` /
   ``cudaImportExternalSemaphore``. The CUDA device pointer
   (``cudaExternalMemoryGetMappedBuffer``) is cached for the surface's
   lifetime.
2. Every ``acquire_read`` / ``acquire_write`` waits on the imported
   timeline (Vulkan-side via the adapter; CUDA-side via
   ``cudaWaitExternalSemaphoresAsync_v2`` so CUDA driver state is in
   sync with Vulkan's view of the kernel timeline) and hands back a
   DLPack PyCapsule named ``"dl_managed_tensor"`` consumable by
   ``torch.from_dlpack`` zero-copy.

There is no per-acquire IPC — the host's pipeline is expected to write
into the OPAQUE_FD buffer and signal the shared timeline ambiently.
Customers who need an explicit host-side trigger (e.g. an explicit
``vkCmdCopyImageToBuffer`` per acquire) should use
``streamlib.adapters.cpu_readback`` instead.

Customer-facing shapes:

  * ``CudaReadView`` / ``CudaWriteView`` — surface-level metadata plus
    a ``dlpack`` PyCapsule the customer hands to ``torch.from_dlpack``
    (or any other ``__dlpack__``-protocol consumer).
  * ``CudaContext`` — concrete subprocess runtime. One per subprocess;
    obtain via :meth:`CudaContext.from_runtime`.
"""

from __future__ import annotations

import ctypes
import itertools
from contextlib import contextmanager
from dataclasses import dataclass
from typing import Iterator, Optional

from streamlib.surface_adapter import (
    STREAMLIB_ADAPTER_ABI_VERSION,
    SurfaceFormat,
)

__all__ = [
    "STREAMLIB_ADAPTER_ABI_VERSION",
    "CudaReadView",
    "CudaWriteView",
    "CudaContext",
]


# ``slpn_cuda_*`` return values — must match the cdylib's
# ``SLPN_CUDA_OK`` / ``_ERR`` / ``_CONTENDED`` constants.
_RC_OK = 0
_RC_ERR = -1
_RC_CONTENDED = 1

# DLPack ``DLDeviceType`` discriminants the cdylib hands back via
# ``SlpnCudaView.device_type``. Mirrors the spec (and the cdylib's
# ``SLPN_CUDA_DEVICE_TYPE_*`` constants).
_DEVICE_TYPE_CUDA = 2
_DEVICE_TYPE_CUDA_HOST = 3

# Surface-id namespace inside this subprocess. The host's pool_id (a
# string) is mapped to a u64 the cdylib uses internally; customers
# never see the u64.
_CUDA_SURFACE_ID_COUNTER = itertools.count(start=1)


class _SlpnCudaView(ctypes.Structure):
    """C struct matching ``streamlib_python_native::cuda::SlpnCudaView``.

    Layout pinned by ``slpn_cuda_view_layout_matches_spec_64bit`` in
    the cdylib's tests — fields, offsets, and sizes are part of the
    ABI between this wrapper and the native library.
    """

    _fields_ = [
        ("size", ctypes.c_uint64),
        ("device_ptr", ctypes.c_uint64),
        ("device_type", ctypes.c_int32),
        ("device_id", ctypes.c_int32),
        ("dlpack_managed_tensor", ctypes.c_void_p),
    ]


# DLPack capsule machinery
# ---------------------------------------------------------------------------
#
# The DLPack v0.8 spec defines a producer/consumer handoff via PyCapsule:
#
# 1. Producer creates a heap-allocated ``DLManagedTensor*``, wraps it in
#    ``PyCapsule_New(mt, "dl_managed_tensor", destructor)``.
# 2. Consumer (e.g. PyTorch) calls ``PyCapsule_GetPointer(capsule,
#    "dl_managed_tensor")`` to take ownership, then renames the capsule
#    to ``"used_dl_managed_tensor"``. The consumer is now responsible
#    for eventually calling ``mt->deleter(mt)``.
# 3. If the consumer never takes the capsule (e.g. an exception fires
#    before ``torch.from_dlpack`` runs), the capsule's destructor must
#    call ``mt->deleter(mt)`` to free the producer-side state.
#
# The destructor inspects the capsule name to decide whether it owns
# the cleanup. ``ctypes.pythonapi`` exposes the relevant CPython API.

# The PyCapsule destructor is a C-level callback — `void(*)(PyObject*)`.
# Parameter type is ``c_void_p`` (raw pointer) rather than ``py_object``
# because the destructor fires during the capsule's tp_dealloc — going
# through ``py_object`` would trigger ctypes' refcount manipulation
# against an object that is in the process of being freed, which
# segfaults under CPython 3.12+. The PyCapsule_* helpers below accept
# the raw pointer via the same ``c_void_p`` argtype, so the destructor
# threads it back without ever materializing a Python reference.
_PyCapsule_Destructor = ctypes.CFUNCTYPE(None, ctypes.c_void_p)

# ``DLManagedTensor`` layout (matches ``streamlib-adapter-cuda::dlpack``
# tests pinning the v0.8 ABI on 64-bit):
#   dl_tensor: 48 bytes @ 0
#   manager_ctx: 8 bytes @ 48
#   deleter: 8 bytes @ 56  ← function pointer ``void(*)(DLManagedTensor*)``
_DLPACK_DELETER_OFFSET = 56
_DLPACK_DELETER_TYPE = ctypes.CFUNCTYPE(None, ctypes.c_void_p)

# Capsule name shipped to consumers. Must be byte-string for the C API.
_DLPACK_CAPSULE_NAME = b"dl_managed_tensor"
_DLPACK_CAPSULE_NAME_USED = b"used_dl_managed_tensor"


def _wire_pythonapi():
    """Wire ``ctypes.pythonapi`` argtypes / restypes for the
    PyCapsule_* helpers. Idempotent — re-wiring is a no-op.

    The destructor path passes a raw ``c_void_p`` PyObject* pointer
    (not a ``py_object``) to avoid ctypes refcount manipulation
    during ``tp_dealloc``; the consumer-facing ``PyCapsule_New``
    output is a real ``py_object`` because that's what consumers see.
    """
    api = ctypes.pythonapi

    api.PyCapsule_New.argtypes = [
        ctypes.c_void_p,
        ctypes.c_char_p,
        ctypes.c_void_p,
    ]
    api.PyCapsule_New.restype = ctypes.py_object

    # Destructor-path overloads — accept raw ``PyObject*`` as
    # ``c_void_p``. Outside the destructor, callers route through
    # explicit ``ctypes.cast(capsule, c_void_p)`` so the same wrappers
    # work both pre- and post-tp_dealloc.
    api.PyCapsule_GetPointer.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    api.PyCapsule_GetPointer.restype = ctypes.c_void_p

    api.PyCapsule_GetName.argtypes = [ctypes.c_void_p]
    api.PyCapsule_GetName.restype = ctypes.c_char_p


_wire_pythonapi()


def _make_dlpack_capsule(mt_ptr: int) -> object:
    """Wrap a ``*mut DLManagedTensor`` (raw integer address, returned
    by the cdylib's ``slpn_cuda_acquire_*``) as a PyCapsule consumable
    by ``torch.from_dlpack``.

    Capsule destructor: if the consumer didn't claim the capsule (name
    still ``"dl_managed_tensor"``), reach into the ``DLManagedTensor``
    layout and call the producer-supplied deleter so resources don't
    leak. If the consumer claimed it (name ``"used_dl_managed_tensor"``),
    the consumer is responsible — do nothing.
    """
    if not mt_ptr:
        raise RuntimeError(
            "CudaContext: cdylib returned null DLManagedTensor pointer — "
            "the cuda runtime is in a bad state"
        )

    # The destructor must keep a reference to itself for as long as the
    # capsule could call it. Holding it on the capsule via
    # PyCapsule_SetContext is one option; simpler: capture in a closure
    # and stash on the returned capsule's __dict__-equivalent
    # (PyCapsule_SetContext) — but PyCapsules don't have __dict__, so
    # we take the standard approach: keep a global registry keyed on
    # the capsule id so the destructor reference lives long enough.
    #
    # Even simpler: ctypes-bound CFUNCTYPE objects can outlive their
    # containing scope as long as something refers to them. We cast
    # the destructor to a plain c_void_p for PyCapsule_New (which only
    # stores the address), and pin the CFUNCTYPE wrapper to a module-
    # level `_capsule_destructors` set so it isn't GC'd until we
    # explicitly remove it.

    destructor_key = next(_destructor_id_counter)

    @_PyCapsule_Destructor
    def destructor(capsule):
        # Read the capsule name. If the consumer claimed it, the name
        # was renamed; we don't own the cleanup.
        name_ptr = ctypes.pythonapi.PyCapsule_GetName(capsule)
        if name_ptr is None:
            # The capsule was invalidated; nothing to do.
            _capsule_destructors.pop(destructor_key, None)
            return
        if name_ptr == _DLPACK_CAPSULE_NAME_USED:
            # Consumer (PyTorch / etc.) took the capsule and is
            # responsible for calling the deleter.
            _capsule_destructors.pop(destructor_key, None)
            return
        # Producer-owned cleanup path: call the DLManagedTensor's
        # deleter to free shape / strides / manager_ctx / the
        # ManagedTensor itself.
        raw_ptr = ctypes.pythonapi.PyCapsule_GetPointer(
            capsule, _DLPACK_CAPSULE_NAME
        )
        if not raw_ptr:
            _capsule_destructors.pop(destructor_key, None)
            return
        # Read the deleter function pointer at offset 56 in
        # DLManagedTensor.
        deleter_addr = ctypes.cast(
            raw_ptr + _DLPACK_DELETER_OFFSET,
            ctypes.POINTER(ctypes.c_void_p),
        )[0]
        if deleter_addr:
            deleter_fn = _DLPACK_DELETER_TYPE(deleter_addr)
            try:
                deleter_fn(ctypes.c_void_p(raw_ptr))
            except Exception:  # pragma: no cover - logged on the cdylib
                pass
        _capsule_destructors.pop(destructor_key, None)

    _capsule_destructors[destructor_key] = destructor

    capsule = ctypes.pythonapi.PyCapsule_New(
        ctypes.c_void_p(mt_ptr),
        _DLPACK_CAPSULE_NAME,
        ctypes.cast(destructor, ctypes.c_void_p),
    )
    return capsule


# Pin ctypes destructor closures so they outlive the capsules that
# reference them. ``CFUNCTYPE``-bound objects aren't hashable, so use
# a counter-keyed dict instead of a set. The destructor itself pops
# its entry on call (consumed or not).
_destructor_id_counter = itertools.count(start=1)
_capsule_destructors: "dict[int, object]" = {}


@dataclass(frozen=True)
class CudaReadView:
    """View handed back inside an ``acquire_read`` scope.

    ``dlpack`` is a PyCapsule named ``"dl_managed_tensor"`` consumable
    by ``torch.from_dlpack`` zero-copy. The capsule references the
    cdylib-imported CUDA memory; the underlying memory stays alive
    until the consumer (or the capsule's destructor) invokes the
    DLPack deleter.

    The view is valid only inside the ``with`` block. After the block
    exits, the adapter releases its read guard — the host pipeline is
    free to overwrite the buffer. If you need to retain the tensor
    beyond the block, call ``.clone()`` on the PyTorch tensor (or the
    framework equivalent) inside the block.
    """

    width: int
    height: int
    format: SurfaceFormat
    size: int
    device_id: int
    device_type: int
    dlpack: object  # PyCapsule


@dataclass(frozen=True)
class CudaWriteView:
    """View handed back inside an ``acquire_write`` scope.

    Same shape as [`CudaReadView`]. The DLPack capsule's underlying
    memory is writable from CUDA — kernels that produce new frames
    (e.g. compositing inference outputs) can dispatch against
    ``torch.from_dlpack(view.dlpack)`` and the writes land in the
    host-shared OPAQUE_FD buffer.
    """

    width: int
    height: int
    format: SurfaceFormat
    size: int
    device_id: int
    device_type: int
    dlpack: object  # PyCapsule


def _surface_pool_id(surface) -> str:
    """Extract the surface-share pool id (string) from either a
    ``StreamlibSurface``-shaped object or a bare string / int."""
    if isinstance(surface, str):
        return surface
    if isinstance(surface, int):
        return str(surface)
    sid = getattr(surface, "id", None)
    if sid is None:
        raise TypeError(
            f"CudaContext: expected StreamlibSurface or pool_id, got {surface!r}"
        )
    return str(sid)


def _surface_format_from(surface) -> SurfaceFormat:
    """Best-effort SurfaceFormat extraction; falls back to BGRA8."""
    fmt = getattr(surface, "format", None)
    if fmt is None:
        return SurfaceFormat.BGRA8
    if isinstance(fmt, SurfaceFormat):
        return fmt
    return SurfaceFormat(int(fmt))


class CudaContext:
    """Customer-facing handle bound to the subprocess's cuda cdylib
    runtime.

    Use as a context manager::

        with ctx.acquire_read(surface) as view:
            tensor = torch.from_dlpack(view.dlpack)
            # tensor lives on CUDA device `view.device_id`, no copy.
            outputs = model(tensor)
            # If you need outputs beyond the block, .clone() them now.
    """

    _shared_instance: Optional["CudaContext"] = None

    def __init__(self, gpu_limited_access) -> None:
        self._gpu = gpu_limited_access
        self._lib = gpu_limited_access.native_lib
        self._wire_signatures()
        rt = self._lib.slpn_cuda_runtime_new()
        if not rt:
            raise RuntimeError(
                "CudaContext: slpn_cuda_runtime_new returned NULL — the "
                "subprocess could not bring up a Vulkan device + CUDA "
                "context. Check that libvulkan.so.1, libcuda.so.1, and "
                "libcudart.so are installed and that the driver supports "
                "VK_KHR_external_memory_fd, VK_EXT_external_memory_dma_buf, "
                "and VK_KHR_external_semaphore_fd. See the subprocess log "
                "for the underlying error."
            )
        self._rt = ctypes.c_void_p(rt)

        # pool_id (host-side string) → local u64 surface_id.
        self._surface_ids: dict[str, int] = {}
        # Pin resolved SurfaceHandle objects so the OPAQUE_FD plane and
        # sync FDs stay alive for the surface's lifetime. The cdylib
        # already dups before each Vulkan / CUDA import, but the
        # SurfaceHandle drop closes the originals — keeping it alive
        # is defense in depth.
        self._resolved_handles: dict[str, object] = {}

    @classmethod
    def from_runtime(cls, runtime_context) -> "CudaContext":
        """Build (or fetch the cached) :class:`CudaContext` for this
        subprocess. The subprocess hosts at most one cuda runtime —
        calling this twice with the same runtime returns the same
        instance.
        """
        if cls._shared_instance is None:
            cls._shared_instance = cls(runtime_context.gpu_limited_access)
        return cls._shared_instance

    def _wire_signatures(self) -> None:
        lib = self._lib

        lib.slpn_cuda_runtime_new.restype = ctypes.c_void_p
        lib.slpn_cuda_runtime_new.argtypes = []

        lib.slpn_cuda_runtime_free.restype = None
        lib.slpn_cuda_runtime_free.argtypes = [ctypes.c_void_p]

        lib.slpn_cuda_register_surface.restype = ctypes.c_int32
        lib.slpn_cuda_register_surface.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint64,
            ctypes.c_void_p,
        ]

        lib.slpn_cuda_unregister_surface.restype = ctypes.c_int32
        lib.slpn_cuda_unregister_surface.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint64,
        ]

        for name in (
            "slpn_cuda_acquire_read",
            "slpn_cuda_acquire_write",
            "slpn_cuda_try_acquire_read",
            "slpn_cuda_try_acquire_write",
        ):
            fn = getattr(lib, name)
            fn.restype = ctypes.c_int32
            fn.argtypes = [
                ctypes.c_void_p,
                ctypes.c_uint64,
                ctypes.POINTER(_SlpnCudaView),
            ]

        for name in (
            "slpn_cuda_release_read",
            "slpn_cuda_release_write",
        ):
            fn = getattr(lib, name)
            fn.restype = ctypes.c_int32
            fn.argtypes = [ctypes.c_void_p, ctypes.c_uint64]

    def _resolve_and_register(self, pool_id: str) -> int:
        """Resolve `pool_id` via surface-share, register with the cuda
        adapter, and return the local u64 surface_id. Idempotent —
        repeat calls return the cached id."""
        cached = self._surface_ids.get(pool_id)
        if cached is not None:
            return cached
        handle = self._gpu.resolve_surface(pool_id)
        handle_ptr = handle.native_handle_ptr
        if not handle_ptr:
            raise RuntimeError(
                f"CudaContext: resolve_surface('{pool_id}') returned a "
                "handle with a null native pointer"
            )
        surface_id = next(_CUDA_SURFACE_ID_COUNTER)
        rc = self._lib.slpn_cuda_register_surface(
            self._rt,
            ctypes.c_uint64(surface_id),
            ctypes.c_void_p(handle_ptr),
        )
        if rc != _RC_OK:
            raise RuntimeError(
                f"CudaContext: register_surface failed for pool_id "
                f"'{pool_id}' (rc={rc}). Common causes: host registered "
                f"the surface as DMA-BUF rather than OPAQUE_FD (cuda "
                "requires `handle_type=opaque_fd`); host did not attach "
                "an exportable timeline semaphore (cuda requires sync_fd); "
                "or libcudart / libcuda.so missing. See the subprocess "
                "log for specifics."
            )
        self._surface_ids[pool_id] = surface_id
        self._resolved_handles[pool_id] = handle
        return surface_id

    @contextmanager
    def acquire_read(self, surface) -> "Iterator[CudaReadView]":
        """Block until the host has signaled the timeline; hand back a
        DLPack-bearing read view. On exit, release the adapter guard
        so the timeline can advance."""
        with self._acquire(surface, write=False, blocking=True) as view:
            yield view  # type: ignore[misc]

    @contextmanager
    def acquire_write(self, surface) -> "Iterator[CudaWriteView]":
        """Block until the host has signaled the timeline; hand back a
        DLPack-bearing write view. CUDA writes land in the OPAQUE_FD
        buffer; on exit, release the adapter guard so the host can
        observe the new contents."""
        with self._acquire(surface, write=True, blocking=True) as view:
            yield view  # type: ignore[misc]

    @contextmanager
    def try_acquire_read(self, surface) -> "Iterator[Optional[CudaReadView]]":
        """Non-blocking read acquire. Yields a [`CudaReadView`] on
        success or ``None`` on contention."""
        with self._acquire(surface, write=False, blocking=False) as view:
            yield view  # type: ignore[misc]

    @contextmanager
    def try_acquire_write(
        self, surface
    ) -> "Iterator[Optional[CudaWriteView]]":
        """Non-blocking write acquire. Yields a [`CudaWriteView`] on
        success or ``None`` on contention."""
        with self._acquire(surface, write=True, blocking=False) as view:
            yield view  # type: ignore[misc]

    @contextmanager
    def _acquire(self, surface, write: bool, blocking: bool) -> "Iterator[object]":
        pool_id = _surface_pool_id(surface)
        format_ = _surface_format_from(surface)
        # Surface format is informational at the cuda layer (the
        # buffer is flat bytes from CUDA's perspective); the host
        # adapter doesn't read it. Recorded on the view so customers
        # can interpret the bytes correctly.
        surface_id = self._resolve_and_register(pool_id)
        view_struct = _SlpnCudaView()
        if blocking:
            fn = (
                self._lib.slpn_cuda_acquire_write
                if write
                else self._lib.slpn_cuda_acquire_read
            )
        else:
            fn = (
                self._lib.slpn_cuda_try_acquire_write
                if write
                else self._lib.slpn_cuda_try_acquire_read
            )
        rc = fn(self._rt, ctypes.c_uint64(surface_id), ctypes.byref(view_struct))
        if rc == _RC_CONTENDED:
            yield None
            return
        if rc != _RC_OK:
            raise RuntimeError(
                f"CudaContext.{'try_' if not blocking else ''}"
                f"acquire_{'write' if write else 'read'}: rc={rc} for "
                f"surface '{pool_id}'"
            )
        try:
            yield self._build_view(view_struct, format_, write)
        finally:
            release_fn = (
                self._lib.slpn_cuda_release_write
                if write
                else self._lib.slpn_cuda_release_read
            )
            release_fn(self._rt, ctypes.c_uint64(surface_id))

    def _build_view(
        self,
        view_struct: _SlpnCudaView,
        format_: SurfaceFormat,
        writable: bool,
    ):
        mt_ptr = view_struct.dlpack_managed_tensor
        if not mt_ptr:
            raise RuntimeError(
                "CudaContext: cdylib returned null DLPack managed tensor "
                "pointer — the cuda runtime is in a bad state"
            )
        capsule = _make_dlpack_capsule(mt_ptr)
        # Width / height are recorded on the original host surface but
        # not threaded through the cuda FFI today (the buffer is flat
        # bytes from CUDA's perspective, and DLPack consumers reshape
        # via tensor.view(...) anyway). Customers needing dimension
        # info can read it from the StreamlibSurface descriptor they
        # passed in.
        klass = CudaWriteView if writable else CudaReadView
        return klass(
            width=0,
            height=0,
            format=format_,
            size=int(view_struct.size),
            device_id=int(view_struct.device_id),
            device_type=int(view_struct.device_type),
            dlpack=capsule,
        )

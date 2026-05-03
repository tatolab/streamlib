# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Capability-typed Python runtime context views for subprocess processors.

Two concrete classes mirror the Rust capability split:

- :class:`NativeRuntimeContextLimitedAccess` — passed to ``process`` /
  ``on_pause`` / ``on_resume``. Carries no ``gpu_full_access`` attribute, so
  reaching privileged ops from the hot path raises ``AttributeError`` at
  runtime (and is flagged by type checkers).
- :class:`NativeRuntimeContextFullAccess` — passed to ``setup`` /
  ``teardown`` / Manual-mode ``start`` / ``stop``. Exposes both
  ``gpu_limited_access`` and ``gpu_full_access``.

Data I/O uses direct FFI to ``libstreamlib_python_native`` via ctypes.
Lifecycle commands use length-prefixed JSON over stdin/stdout pipes.

Protocol (lifecycle only):
  All messages are length-prefixed: [4 bytes u32 BE length][JSON bytes]
"""

from __future__ import annotations

import json
import struct
import threading
from typing import Any, Dict, Optional, Sequence, Tuple, TYPE_CHECKING

import msgpack

if TYPE_CHECKING:
    from .escalate import EscalateChannel


# ============================================================================
# Lifecycle protocol I/O (length-prefixed JSON over stdin/stdout)
# ============================================================================


# Serializes frame writes across the main thread (lifecycle + escalate
# request/response) and the log writer daemon (fire-and-forget escalate log
# ops). Without this, two threads can interleave halves of a length-prefixed
# frame and desynchronize the bridge reader on the host side.
_bridge_send_lock = threading.Lock()


def bridge_read_message(stdin):
    """Read a length-prefixed JSON message from stdin."""
    len_buf = stdin.read(4)
    if len(len_buf) < 4:
        raise EOFError("stdin closed")
    length = struct.unpack(">I", len_buf)[0]
    msg_buf = stdin.read(length)
    if len(msg_buf) < length:
        raise EOFError("stdin closed mid-message")
    return json.loads(msg_buf)


def bridge_send_message(stdout, msg):
    """Send a length-prefixed JSON message to stdout."""
    json_bytes = json.dumps(msg, separators=(",", ":")).encode("utf-8")
    with _bridge_send_lock:
        stdout.write(struct.pack(">I", len(json_bytes)))
        stdout.write(json_bytes)
        stdout.flush()


# ============================================================================
# Native FFI library loading
# ============================================================================


#: Default read-buffer capacity when the host sends no per-input
#: ``max_payload_bytes``. Matches Rust's ``streamlib_ipc_types::MAX_PAYLOAD_SIZE``.
DEFAULT_READ_BUF_BYTES = 65536


def compute_read_buf_bytes(inputs) -> int:
    """Size the input read buffer to the largest per-port ``max_payload_bytes``
    the host declared, floored at :data:`DEFAULT_READ_BUF_BYTES`.

    A fixed smaller buffer silently truncates payloads larger than it —
    including encoded video frames, which can be arbitrarily large depending
    on how the schema is configured.
    """
    declared = [
        inp.get("max_payload_bytes") or 0
        for inp in inputs
    ]
    return max(DEFAULT_READ_BUF_BYTES, *declared) if declared else DEFAULT_READ_BUF_BYTES


def decode_read_result(read_buf, read_buf_bytes: int, data_len: int, timestamp_ns: int, port_name: str):
    """Build the return value of a single FFI read given the output state
    ``slpn_input_read`` populated.

    Returns ``(bytes, timestamp_ns)`` for a valid read, ``(None, None)`` when
    the read produced no data or when the native side reported more bytes than
    the read buffer can hold (truncation). Extracted for testing so the empty
    / happy / truncated branches can be exercised without spinning up iceoryx2.
    """
    if data_len == 0:
        return None, None
    if data_len > read_buf_bytes:
        from . import log

        log.warn(
            "payload truncated on input port",
            port=port_name,
            reported_bytes=data_len,
            read_buf_bytes=read_buf_bytes,
        )
        return None, None
    return bytes(read_buf[:data_len]), timestamp_ns


def load_native_lib(lib_path):
    """Load the streamlib-python-native cdylib and configure ctypes signatures."""
    import ctypes

    lib = ctypes.CDLL(lib_path)

    # Context lifecycle
    lib.slpn_context_create.argtypes = [ctypes.c_char_p]
    lib.slpn_context_create.restype = ctypes.c_void_p
    lib.slpn_context_destroy.argtypes = [ctypes.c_void_p]
    lib.slpn_context_destroy.restype = None
    lib.slpn_monotonic_now_ns.argtypes = []
    lib.slpn_monotonic_now_ns.restype = ctypes.c_uint64

    # Input
    lib.slpn_input_subscribe.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    lib.slpn_input_subscribe.restype = ctypes.c_int32
    lib.slpn_input_poll.argtypes = [ctypes.c_void_p]
    lib.slpn_input_poll.restype = ctypes.c_int32
    lib.slpn_input_read.argtypes = [
        ctypes.c_void_p, ctypes.c_char_p,
        ctypes.c_void_p, ctypes.c_uint32,
        ctypes.POINTER(ctypes.c_uint32), ctypes.POINTER(ctypes.c_int64),
    ]
    lib.slpn_input_read.restype = ctypes.c_int32
    lib.slpn_input_set_read_mode.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int32]
    lib.slpn_input_set_read_mode.restype = ctypes.c_int32

    # Output
    lib.slpn_output_publish.argtypes = [
        ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p,
        ctypes.c_char_p, ctypes.c_char_p, ctypes.c_size_t,
        ctypes.c_char_p,  # notify_service_name (may be empty/null)
    ]
    lib.slpn_output_publish.restype = ctypes.c_int32
    lib.slpn_output_write.argtypes = [
        ctypes.c_void_p, ctypes.c_char_p,
        ctypes.c_void_p, ctypes.c_uint32, ctypes.c_int64,
    ]
    lib.slpn_output_write.restype = ctypes.c_int32

    # Event service (fd-multiplexed wakeups)
    lib.slpn_event_subscribe.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    lib.slpn_event_subscribe.restype = ctypes.c_int32
    lib.slpn_event_listener_fd.argtypes = [ctypes.c_void_p]
    lib.slpn_event_listener_fd.restype = ctypes.c_int32
    lib.slpn_event_drain.argtypes = [ctypes.c_void_p]
    lib.slpn_event_drain.restype = ctypes.c_int32

    # GPU Surface
    lib.slpn_gpu_surface_lookup.argtypes = [ctypes.c_uint32]
    lib.slpn_gpu_surface_lookup.restype = ctypes.c_void_p
    lib.slpn_gpu_surface_lock.argtypes = [ctypes.c_void_p, ctypes.c_int32]
    lib.slpn_gpu_surface_lock.restype = ctypes.c_int32
    lib.slpn_gpu_surface_unlock.argtypes = [ctypes.c_void_p, ctypes.c_int32]
    lib.slpn_gpu_surface_unlock.restype = ctypes.c_int32
    lib.slpn_gpu_surface_base_address.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_base_address.restype = ctypes.c_void_p
    lib.slpn_gpu_surface_width.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_width.restype = ctypes.c_uint32
    lib.slpn_gpu_surface_height.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_height.restype = ctypes.c_uint32
    lib.slpn_gpu_surface_bytes_per_row.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_bytes_per_row.restype = ctypes.c_uint32
    lib.slpn_gpu_surface_create.argtypes = [ctypes.c_uint32, ctypes.c_uint32, ctypes.c_uint32]
    lib.slpn_gpu_surface_create.restype = ctypes.c_void_p
    lib.slpn_gpu_surface_get_id.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_get_id.restype = ctypes.c_uint32
    lib.slpn_gpu_surface_release.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_release.restype = None
    lib.slpn_gpu_surface_iosurface_ref.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_iosurface_ref.restype = ctypes.c_void_p

    # Surface-share service FFI.
    lib.slpn_surface_connect.argtypes = [ctypes.c_char_p, ctypes.c_char_p]
    lib.slpn_surface_connect.restype = ctypes.c_void_p
    lib.slpn_surface_disconnect.argtypes = [ctypes.c_void_p]
    lib.slpn_surface_disconnect.restype = None
    lib.slpn_surface_resolve_surface.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    lib.slpn_surface_resolve_surface.restype = ctypes.c_void_p
    lib.slpn_surface_acquire_surface.argtypes = [
        ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32,
        ctypes.c_uint32, ctypes.c_char_p, ctypes.c_uint32,
    ]
    lib.slpn_surface_acquire_surface.restype = ctypes.c_void_p
    lib.slpn_surface_unregister_surface.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    lib.slpn_surface_unregister_surface.restype = None

    # OpenGL adapter runtime (#530, Linux). Uses the host adapter crate's
    # EglRuntime + OpenGlSurfaceAdapter for EGL bring-up + DMA-BUF→GL
    # import; this binding only exposes scoped acquire/release returning
    # a `GL_TEXTURE_2D` id the customer's GL library renders into.
    lib.slpn_opengl_runtime_new.argtypes = []
    lib.slpn_opengl_runtime_new.restype = ctypes.c_void_p
    lib.slpn_opengl_runtime_free.argtypes = [ctypes.c_void_p]
    lib.slpn_opengl_runtime_free.restype = None
    lib.slpn_opengl_register_surface.argtypes = [
        ctypes.c_void_p, ctypes.c_uint64, ctypes.c_void_p,
    ]
    lib.slpn_opengl_register_surface.restype = ctypes.c_int32
    lib.slpn_opengl_register_external_oes_surface.argtypes = [
        ctypes.c_void_p, ctypes.c_uint64, ctypes.c_void_p,
    ]
    lib.slpn_opengl_register_external_oes_surface.restype = ctypes.c_int32
    lib.slpn_opengl_unregister_surface.argtypes = [
        ctypes.c_void_p, ctypes.c_uint64,
    ]
    lib.slpn_opengl_unregister_surface.restype = ctypes.c_int32
    for _op in ("acquire_write", "acquire_read"):
        _fn = getattr(lib, f"slpn_opengl_{_op}")
        _fn.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
        _fn.restype = ctypes.c_uint32
    for _op in ("release_write", "release_read"):
        _fn = getattr(lib, f"slpn_opengl_{_op}")
        _fn.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
        _fn.restype = ctypes.c_int32

    # Per-plane surface-share accessors (#530). Required by the OpenGL
    # adapter for `EGL_DMA_BUF_PLANE{N}_PITCH_EXT` import.
    lib.slpn_gpu_surface_plane_stride.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
    lib.slpn_gpu_surface_plane_stride.restype = ctypes.c_uint64
    lib.slpn_gpu_surface_plane_offset.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
    lib.slpn_gpu_surface_plane_offset.restype = ctypes.c_uint64
    lib.slpn_gpu_surface_plane_fd.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
    lib.slpn_gpu_surface_plane_fd.restype = ctypes.c_int32
    lib.slpn_gpu_surface_drm_format_modifier.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_drm_format_modifier.restype = ctypes.c_uint64
    # Producer-declared `VkImageLayout` from the surface-share lookup
    # response (#633). Adapter `register_host_surface` paths read it
    # from the SurfaceHandle and pass it into
    # `HostSurfaceRegistration::initial_layout` so the consumer-side
    # `current_layout` matches the producer's claim.
    lib.slpn_gpu_surface_initial_image_layout.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_initial_image_layout.restype = ctypes.c_int32

    return lib


# ============================================================================
# Native FFI context classes
# ============================================================================


class NativeInputs:
    """Input ports backed by iceoryx2 subscribers via FFI."""

    def __init__(self, lib, ctx_ptr, read_buf_bytes: int = DEFAULT_READ_BUF_BYTES):
        import ctypes

        self._lib = lib
        self._ctx_ptr = ctx_ptr
        self._read_buf_bytes = read_buf_bytes
        self._read_buf = (ctypes.c_uint8 * read_buf_bytes)()
        self._out_len = ctypes.c_uint32(0)
        self._out_ts = ctypes.c_int64(0)

    def _read_raw(self, port_name):
        """Call into FFI and return ``(data_bytes, timestamp_ns)`` or
        ``(None, None)``. Logs on truncation."""
        import ctypes

        result = self._lib.slpn_input_read(
            self._ctx_ptr,
            port_name.encode("utf-8"),
            ctypes.cast(self._read_buf, ctypes.c_void_p),
            self._read_buf_bytes,
            ctypes.byref(self._out_len),
            ctypes.byref(self._out_ts),
        )
        if result != 0:
            return None, None
        return decode_read_result(
            self._read_buf,
            self._read_buf_bytes,
            self._out_len.value,
            self._out_ts.value,
            port_name,
        )

    def read(self, port_name):
        """Read latest data from a port. Returns deserialized msgpack data or None."""
        raw, _ = self._read_raw(port_name)
        if raw is None:
            return None
        return msgpack.unpackb(raw, raw=False)

    def read_with_timestamp(self, port_name):
        """Read latest data and timestamp. Returns (data, timestamp_ns) or (None, None)."""
        raw, ts = self._read_raw(port_name)
        if raw is None:
            return None, None
        return msgpack.unpackb(raw, raw=False), ts


class NativeOutputs:
    """Output ports backed by iceoryx2 publishers via FFI."""

    def __init__(self, lib, ctx_ptr):
        self._lib = lib
        self._ctx_ptr = ctx_ptr

    def write(self, port_name, data, timestamp_ns=None):
        """Write data to a port. Serializes via msgpack and sends via FFI."""
        import ctypes

        if timestamp_ns is None:
            timestamp_ns = self._lib.slpn_monotonic_now_ns()

        packed = msgpack.packb(data, use_bin_type=True)
        data_buf = (ctypes.c_uint8 * len(packed))(*packed)

        result = self._lib.slpn_output_write(
            self._ctx_ptr,
            port_name.encode("utf-8"),
            ctypes.cast(data_buf, ctypes.c_void_p),
            len(packed),
            timestamp_ns,
        )
        if result != 0:
            raise RuntimeError(f"Failed to write to port '{port_name}'")


class NativeGpuSurfaceHandle:
    """GPU surface handle backed by the C-side SurfaceHandle* via FFI.

    Delegates all operations to the cdylib. Does NOT use Python-side
    IOSurface ctypes — the cdylib owns the surface lifecycle.
    """

    def __init__(self, lib, handle_ptr, pooled=False):
        self._lib = lib
        self._handle_ptr = handle_ptr
        self._pooled = pooled
        self.width = lib.slpn_gpu_surface_width(handle_ptr)
        self.height = lib.slpn_gpu_surface_height(handle_ptr)
        self.bytes_per_row = lib.slpn_gpu_surface_bytes_per_row(handle_ptr)

    def lock(self, read_only=True):
        """Lock surface for CPU access."""
        result = self._lib.slpn_gpu_surface_lock(
            self._handle_ptr, 1 if read_only else 0
        )
        if result != 0:
            raise RuntimeError("Failed to lock IOSurface")

    def unlock(self, read_only=True):
        """Unlock surface after CPU access."""
        self._lib.slpn_gpu_surface_unlock(
            self._handle_ptr, 1 if read_only else 0
        )

    @property
    def base_address(self):
        """Raw base address of the locked surface memory (untyped int).

        Returns ``0`` when the surface is not locked or the native side
        reported a null pointer. Callers construct their own typed views
        (numpy, ``ctypes.c_uint8.from_address``, etc.) on top of this so
        no-numpy consumers don't pay the numpy dep.
        """
        return int(self._lib.slpn_gpu_surface_base_address(self._handle_ptr) or 0)

    def as_numpy(self):
        """Create numpy array VIEW into locked surface memory (zero-copy)."""
        import ctypes
        import numpy as np

        base = self.base_address
        if not base:
            raise RuntimeError("IOSurface base address is null (not locked?)")
        buf = (ctypes.c_uint8 * (self.bytes_per_row * self.height)).from_address(base)
        return np.ndarray(
            shape=(self.height, self.width, 4),
            dtype=np.uint8,
            buffer=buf,
            strides=(self.bytes_per_row, 4, 1),
        )

    @property
    def iosurface_id(self):
        """IOSurface ID for this surface."""
        return self._lib.slpn_gpu_surface_get_id(self._handle_ptr)

    @property
    def iosurface_ref(self):
        """Raw IOSurfaceRef pointer for CGL texture binding."""
        import ctypes
        ref = self._lib.slpn_gpu_surface_iosurface_ref(self._handle_ptr)
        return ctypes.c_void_p(ref)

    @property
    def native_handle_ptr(self) -> int:
        """Raw `*mut SurfaceHandle` pointer (untyped int) for adapter
        crates that integrate with `streamlib-python-native`.

        Reserved for in-tree adapter SDKs (e.g. `streamlib.adapters.opengl`)
        that need to hand the underlying surface-share handle to a
        cross-language adapter FFI op (e.g. `slpn_opengl_register_surface`).
        Customer processors should use `lock` / `base_address` / numpy
        accessors above instead.
        """
        return int(self._handle_ptr or 0)

    @property
    def native_lib(self):
        """The cdylib handle this surface was resolved against. Used by
        in-tree adapter SDKs to call additional `slpn_*` FFI ops without
        re-loading the cdylib."""
        return self._lib

    def release(self):
        """Release the C-side surface handle."""
        if self._pooled:
            return
        if self._handle_ptr:
            self._lib.slpn_gpu_surface_release(self._handle_ptr)
            self._handle_ptr = None

    def __del__(self):
        self.release()


# ============================================================================
# GPU capability views
# ============================================================================


class NativeGpuContextLimitedAccess:
    """Non-allocating GPU capability — resolve existing surfaces only.

    Mirrors the Rust [`GpuContextLimitedAccess`] surface. Available in
    every lifecycle phase, including ``process`` / ``on_pause`` /
    ``on_resume``.
    """

    def __init__(self, lib, handle_ptr):
        self._lib = lib
        self._handle_ptr = handle_ptr

    @property
    def native_lib(self):
        """The cdylib handle this view's surfaces resolve against. Used
        by in-tree adapter SDKs (e.g. ``streamlib.adapters.opengl``) to
        call additional ``slpn_*`` FFI ops without re-loading the cdylib.

        Customer processors should not need this — the view's
        :meth:`resolve_surface` covers the common case.
        """
        return self._lib

    def resolve_surface(self, surface_id):
        """Resolve a surface-share pool UUID to a GPU surface handle."""
        if self._handle_ptr:
            handle_ptr = self._lib.slpn_surface_resolve_surface(
                self._handle_ptr,
                surface_id.encode("utf-8"),
            )
            if not handle_ptr:
                raise RuntimeError(f"Surface-share service failed to resolve surface: {surface_id}")
            return NativeGpuSurfaceHandle(self._lib, handle_ptr)

        # Fallback: treat surface_id as numeric IOSurface ID
        iosurface_id = int(surface_id)
        handle_ptr = self._lib.slpn_gpu_surface_lookup(iosurface_id)
        if not handle_ptr:
            raise RuntimeError(f"IOSurface not found: {surface_id}")
        return NativeGpuSurfaceHandle(self._lib, handle_ptr)


class NativeGpuContextFullAccess(NativeGpuContextLimitedAccess):
    """Privileged GPU capability — limited ops plus IOSurface allocation.

    Mirrors the Rust [`GpuContextFullAccess`] surface. Only available in
    ``setup`` / ``teardown`` / Manual-mode ``start`` / ``stop``.
    """

    SURFACE_POOL_SIZE = 3

    def __init__(self, lib, handle_ptr):
        super().__init__(lib, handle_ptr)
        self._output_pool = []  # list of (pool_id, handle_ptr)
        self._output_pool_index = 0
        self._output_pool_width = 0
        self._output_pool_height = 0

    def acquire_surface(self, width, height, format="bgra"):
        """Acquire a pixel buffer from the pool (triple-buffered, round-robin)."""
        import ctypes

        bytes_per_element = 4  # BGRA

        # If dimensions changed, release old pool and create new one
        if (self._output_pool
                and (width != self._output_pool_width or height != self._output_pool_height)):
            self.release_pool()

        # First call (or after dimension change): pre-allocate pool
        if not self._output_pool:
            if self._handle_ptr:
                for _ in range(self.SURFACE_POOL_SIZE):
                    pool_id_buf = (ctypes.c_char * 256)()
                    handle_ptr = self._lib.slpn_surface_acquire_surface(
                        self._handle_ptr,
                        width, height, bytes_per_element,
                        pool_id_buf, 256,
                    )
                    if not handle_ptr:
                        raise RuntimeError(
                            f"Surface-share service failed to acquire surface: {width}x{height}"
                        )
                    pool_id = pool_id_buf.value.decode("utf-8")
                    self._output_pool.append((pool_id, handle_ptr))
            else:
                for _ in range(self.SURFACE_POOL_SIZE):
                    handle_ptr = self._lib.slpn_gpu_surface_create(
                        width, height, bytes_per_element
                    )
                    if not handle_ptr:
                        raise RuntimeError(
                            f"Failed to create IOSurface: {width}x{height}"
                        )
                    surface_id = self._lib.slpn_gpu_surface_get_id(handle_ptr)
                    self._output_pool.append((str(surface_id), handle_ptr))

            self._output_pool_width = width
            self._output_pool_height = height
            self._output_pool_index = 0

        # Round-robin: return next surface from pool
        pool_id, handle_ptr = self._output_pool[self._output_pool_index]
        self._output_pool_index = (
            (self._output_pool_index + 1) % len(self._output_pool)
        )
        return pool_id, NativeGpuSurfaceHandle(self._lib, handle_ptr, pooled=True)

    def release_pool(self):
        """Release all pooled surfaces."""
        for pool_id, handle_ptr in self._output_pool:
            if handle_ptr:
                self._lib.slpn_gpu_surface_release(handle_ptr)
            if self._handle_ptr:
                self._lib.slpn_surface_unregister_surface(
                    self._handle_ptr,
                    pool_id.encode("utf-8"),
                )
        self._output_pool = []
        self._output_pool_index = 0
        self._output_pool_width = 0
        self._output_pool_height = 0

    def __del__(self):
        self.release_pool()


# ============================================================================
# Capability-typed runtime context views
# ============================================================================


class NativeProcessorState:
    """Shared FFI-backed state reused by both capability views for a single
    processor lifecycle.

    Construction is internal — ``subprocess_runner`` builds one of these per
    ``setup`` and wraps it in the appropriate view per lifecycle method.
    """

    def __init__(
        self,
        lib,
        ctx_ptr,
        config: Optional[Dict[str, Any]],
        handle_ptr=None,
        escalate_channel: "Optional[EscalateChannel]" = None,
        read_buf_bytes: int = DEFAULT_READ_BUF_BYTES,
    ) -> None:
        self._lib = lib
        self._ctx_ptr = ctx_ptr
        self._config = config or {}
        self._handle_ptr = handle_ptr
        self._escalate_channel = escalate_channel
        self.inputs = NativeInputs(lib, ctx_ptr, read_buf_bytes=read_buf_bytes)
        self.outputs = NativeOutputs(lib, ctx_ptr)
        # One instance of each GPU view shared across per-call context
        # wrappers so pool state and handle lifetimes are stable across
        # lifecycle phases.
        self._gpu_limited = NativeGpuContextLimitedAccess(lib, handle_ptr)
        self._gpu_full = NativeGpuContextFullAccess(lib, handle_ptr)

    @property
    def config(self) -> Dict[str, Any]:
        """Processor configuration dictionary."""
        return self._config

    @property
    def time(self) -> int:
        """Current monotonic time in nanoseconds via `clock_gettime(CLOCK_MONOTONIC)`.

        Comparable across processes — to host Rust `Instant` reads and to
        the Deno SDK's `monotonicNowNs()`. Equivalent to the module-level
        `streamlib.monotonic_now_ns()`.
        """
        return self._lib.slpn_monotonic_now_ns()

    def gpu_limited_access(self) -> NativeGpuContextLimitedAccess:
        """Return the limited-access GPU view (resolution only)."""
        return self._gpu_limited

    def gpu_full_access(self) -> NativeGpuContextFullAccess:
        """Return the full-access GPU view (allocations + resolution)."""
        return self._gpu_full

    def escalate_acquire_pixel_buffer(
        self, width: int, height: int, format: str = "bgra"
    ) -> Dict[str, Any]:
        """Ask the host to allocate a new-shape pixel buffer on our behalf."""
        channel = self._require_channel()
        return channel.acquire_pixel_buffer(width, height, format)

    def escalate_acquire_texture(
        self,
        width: int,
        height: int,
        format: str,
        usage: "Sequence[str]",
    ) -> Dict[str, Any]:
        """Ask the host to allocate a pooled GPU texture on our behalf."""
        channel = self._require_channel()
        return channel.acquire_texture(width, height, format, usage)

    def escalate_release_handle(self, handle_id: str) -> Dict[str, Any]:
        """Drop the host's strong reference to a previously-escalated handle."""
        channel = self._require_channel()
        return channel.release_handle(handle_id)

    def _require_channel(self) -> "EscalateChannel":
        if self._escalate_channel is None:
            # Fall back to the process-wide singleton so helpers that didn't
            # receive ctx still work inside the subprocess lifecycle.
            from .escalate import channel as _singleton
            return _singleton()
        return self._escalate_channel

    def release_pool(self) -> None:
        """Release the full-access pool (called during teardown)."""
        self._gpu_full.release_pool()


class NativeRuntimeContextLimitedAccess:
    """Restricted-capability runtime context passed to ``process`` /
    ``on_pause`` / ``on_resume``.

    Exposes :class:`NativeGpuContextLimitedAccess` only — attempting to
    reach ``gpu_full_access`` raises :class:`AttributeError` at runtime
    and is flagged by type checkers.

    Mirrors the Rust [`RuntimeContextLimitedAccess`] view.
    """

    __slots__ = (
        "_state",
        "config",
        "inputs",
        "outputs",
        "gpu_limited_access",
    )

    def __init__(self, state: NativeProcessorState) -> None:
        self._state = state
        self.config = state.config
        self.inputs = state.inputs
        self.outputs = state.outputs
        self.gpu_limited_access = state.gpu_limited_access()

    @property
    def time(self) -> int:
        """Current monotonic time in nanoseconds."""
        return self._state.time

    def escalate_acquire_pixel_buffer(
        self, width: int, height: int, format: str = "bgra"
    ) -> Dict[str, Any]:
        """Ask the host to allocate a new-shape pixel buffer on our behalf.

        The escalate channel is the capability-preserving path for
        allocation in the hot loop — it routes through the Rust host's
        [`GpuContextLimitedAccess::escalate`], which serializes against
        every other escalation in the runtime.
        """
        return self._state.escalate_acquire_pixel_buffer(width, height, format)

    def escalate_acquire_texture(
        self,
        width: int,
        height: int,
        format: str,
        usage: "Sequence[str]",
    ) -> Dict[str, Any]:
        """Ask the host to allocate a pooled GPU texture on our behalf."""
        return self._state.escalate_acquire_texture(width, height, format, usage)

    def escalate_release_handle(self, handle_id: str) -> Dict[str, Any]:
        """Drop the host's strong reference to a previously-escalated handle."""
        return self._state.escalate_release_handle(handle_id)


class NativeRuntimeContextFullAccess:
    """Privileged runtime context passed to ``setup`` / ``teardown`` and
    Manual-mode ``start`` / ``stop``.

    Exposes both :class:`NativeGpuContextFullAccess` (for allocations) and
    :class:`NativeGpuContextLimitedAccess` (so privileged methods can hand a
    stashable limited handle to downstream workers).

    Mirrors the Rust [`RuntimeContextFullAccess`] view.
    """

    __slots__ = (
        "_state",
        "config",
        "inputs",
        "outputs",
        "gpu_limited_access",
        "gpu_full_access",
    )

    def __init__(self, state: NativeProcessorState) -> None:
        self._state = state
        self.config = state.config
        self.inputs = state.inputs
        self.outputs = state.outputs
        self.gpu_limited_access = state.gpu_limited_access()
        self.gpu_full_access = state.gpu_full_access()

    @property
    def time(self) -> int:
        """Current monotonic time in nanoseconds."""
        return self._state.time

    def escalate_acquire_pixel_buffer(
        self, width: int, height: int, format: str = "bgra"
    ) -> Dict[str, Any]:
        """Ask the host to allocate a new-shape pixel buffer on our behalf."""
        return self._state.escalate_acquire_pixel_buffer(width, height, format)

    def escalate_acquire_texture(
        self,
        width: int,
        height: int,
        format: str,
        usage: "Sequence[str]",
    ) -> Dict[str, Any]:
        """Ask the host to allocate a pooled GPU texture on our behalf."""
        return self._state.escalate_acquire_texture(width, height, format, usage)

    def escalate_release_handle(self, handle_id: str) -> Dict[str, Any]:
        """Drop the host's strong reference to a previously-escalated handle."""
        return self._state.escalate_release_handle(handle_id)

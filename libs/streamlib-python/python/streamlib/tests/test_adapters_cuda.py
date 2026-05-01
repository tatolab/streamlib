# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Smoke test for the Python CUDA adapter wrapper module (#589).

Confirms the module imports, the view dataclasses round-trip
construction, the FFI struct layout matches what the cdylib emits,
and the DLPack PyCapsule machinery is wired to ``ctypes.pythonapi``.

A real subprocess test (host registers a host-allocated OPAQUE_FD
``VkBuffer``, Python opens the cdylib, ``slpn_cuda_acquire_read`` →
``torch.from_dlpack`` → byte-equal assertion against the host pattern)
requires a polyglot test harness that doesn't yet exist in tree —
filed as #596. This file exercises the Python module's contract
against the cdylib's documented FFI ABI.
"""

from __future__ import annotations

import ctypes

from streamlib.adapters import cuda as c
from streamlib.surface_adapter import STREAMLIB_ADAPTER_ABI_VERSION, SurfaceFormat


def test_module_re_exports_abi_version_constant():
    assert c.STREAMLIB_ADAPTER_ABI_VERSION == STREAMLIB_ADAPTER_ABI_VERSION


def test_slpn_cuda_view_layout_matches_cdylib():
    # Mirrors `slpn_cuda_view_layout_matches_spec_64bit` in the cdylib's
    # tests — these offsets / sizes are part of the wire ABI between
    # the cdylib's `SlpnCudaView` and Python's `_SlpnCudaView`.
    assert ctypes.sizeof(c._SlpnCudaView) == 32
    fields = {name: c._SlpnCudaView.__dict__[name].offset for name, _ in c._SlpnCudaView._fields_}
    assert fields == {
        "size": 0,
        "device_ptr": 8,
        "device_type": 16,
        "device_id": 20,
        "dlpack_managed_tensor": 24,
    }


def test_constants_match_dlpack_spec():
    # DLPack v0.8 `DLDeviceType` discriminants — wire ABI.
    assert c._DEVICE_TYPE_CUDA == 2
    assert c._DEVICE_TYPE_CUDA_HOST == 3
    # `slpn_cuda_*` return values — must match the cdylib's
    # SLPN_CUDA_OK / _ERR / _CONTENDED constants.
    assert c._RC_OK == 0
    assert c._RC_ERR == -1
    assert c._RC_CONTENDED == 1
    # DLManagedTensor deleter offset — pinned by adapter-cuda's dlpack
    # layout regression test. dl_tensor (48) + manager_ctx (8) = 56.
    assert c._DLPACK_DELETER_OFFSET == 56


def test_dlpack_capsule_name_matches_pytorch_consumer():
    # PyTorch's `torch.from_dlpack` checks the PyCapsule's name via
    # `PyCapsule_IsValid(capsule, "dltensor")`. An earlier draft of
    # this module used `"dl_managed_tensor"`, which torch silently
    # rejects with `from_dlpack received an invalid capsule. Note that
    # DLTensor capsules can be consumed only once`. The name is part
    # of the DLPack v0.8 spec — pin it here so a regression turns into
    # a unit-test failure rather than an E2E-only hang. See #591.
    assert c._DLPACK_CAPSULE_NAME == b"dltensor"
    assert c._DLPACK_CAPSULE_NAME_USED == b"used_dltensor"
    # And the produced capsule must report the right name to a
    # consumer probing it via `PyCapsule_IsValid`.
    fake_mt = (ctypes.c_uint8 * 100)()
    capsule = c._make_dlpack_capsule(ctypes.addressof(fake_mt))
    ctypes.pythonapi.PyCapsule_IsValid.argtypes = [ctypes.py_object, ctypes.c_char_p]
    ctypes.pythonapi.PyCapsule_IsValid.restype = ctypes.c_int
    assert ctypes.pythonapi.PyCapsule_IsValid(capsule, b"dltensor") == 1
    assert ctypes.pythonapi.PyCapsule_IsValid(capsule, b"dl_managed_tensor") == 0


def test_cuda_views_round_trip_dataclass_construction():
    # Use a `c_uint8 * 0` as a stand-in PyCapsule; the dataclass just
    # holds the reference — it doesn't dereference.
    fake_capsule = object()
    rv = c.CudaReadView(
        width=0,
        height=0,
        format=SurfaceFormat.BGRA8,
        size=1024 * 1024,
        device_id=0,
        device_type=c._DEVICE_TYPE_CUDA,
        dlpack=fake_capsule,
    )
    wv = c.CudaWriteView(
        width=0,
        height=0,
        format=SurfaceFormat.BGRA8,
        size=2048,
        device_id=1,
        device_type=c._DEVICE_TYPE_CUDA_HOST,
        dlpack=fake_capsule,
    )
    assert rv.size == 1024 * 1024
    assert rv.device_type == 2
    assert rv.device_id == 0
    assert wv.size == 2048
    assert wv.device_type == 3
    assert wv.device_id == 1
    # Both views must hold the capsule reference; the producer relies
    # on this to keep the underlying CUDA memory alive across the
    # acquire scope.
    assert rv.dlpack is fake_capsule
    assert wv.dlpack is fake_capsule


def test_pythonapi_pycapsule_helpers_are_wired():
    # `_wire_pythonapi` runs at module import time; this asserts the
    # argtypes / restypes survived intact for the helpers the capsule
    # destructor reaches into. Argtypes for `PyCapsule_GetName` /
    # `PyCapsule_GetPointer` use `c_void_p` (raw `PyObject*`) rather
    # than `py_object` so the destructor path doesn't trigger ctypes
    # refcount manipulation against an object in `tp_dealloc`.
    assert ctypes.pythonapi.PyCapsule_New.restype == ctypes.py_object
    assert ctypes.pythonapi.PyCapsule_GetPointer.argtypes == [
        ctypes.c_void_p,
        ctypes.c_char_p,
    ]
    assert ctypes.pythonapi.PyCapsule_GetPointer.restype == ctypes.c_void_p
    assert ctypes.pythonapi.PyCapsule_GetName.argtypes == [ctypes.c_void_p]
    assert ctypes.pythonapi.PyCapsule_GetName.restype == ctypes.c_char_p


def test_capsule_lifecycle_releases_destructor_pin():
    """Confirms the PyCapsule destructor wiring is in place: the
    destructor closure is pinned in `_capsule_destructors` for the
    capsule's lifetime, then popped when the capsule is dropped.

    Uses a fake `DLManagedTensor` with a NULL deleter at offset 56 —
    the destructor's null-check skips the deleter call, so this test
    exercises wire-up + cleanup without crossing the Python-callable-
    during-GC boundary (the real deleter is exercised end-to-end by
    `cuda_carve_out.rs` once the cdylib is loaded against a real
    consumer).
    """
    # Build a fake DLManagedTensor: 64 bytes total, deleter slot at
    # offset 56 is implicitly zero (calloc-style ctypes zeroing). The
    # destructor reads the deleter pointer there and skips the call
    # because it's NULL — no cross-runtime callback risk.
    fake_mt = (ctypes.c_uint8 * 64)()
    initial_pin_count = len(c._capsule_destructors)

    capsule = c._make_dlpack_capsule(ctypes.addressof(fake_mt))
    assert capsule is not None

    # Pinning happened: one new entry in the destructor table while
    # the capsule is live. The wrapper holds the CFUNCTYPE alive so
    # PyCapsule_New's stored pointer remains valid.
    assert len(c._capsule_destructors) == initial_pin_count + 1, (
        "PyCapsule destructor must be pinned in _capsule_destructors "
        "for the capsule's lifetime"
    )

    # Drop the capsule — destructor fires; null deleter slot means
    # the destructor's deleter-call branch is skipped, but the
    # cleanup branch (popping its own entry from the dict) runs.
    del capsule
    # Force a GC cycle so the destructor definitely runs.
    import gc

    gc.collect()

    assert len(c._capsule_destructors) == initial_pin_count, (
        "PyCapsule destructor must pop its own entry from "
        f"_capsule_destructors on drop; expected {initial_pin_count} "
        f"entries, found {len(c._capsule_destructors)}"
    )


def test_cuda_context_class_exposes_expected_method_set():
    # Surface-adapter Protocol shape — any future Protocol type-check
    # against `CudaContext` should structurally match these.
    assert hasattr(c.CudaContext, "acquire_read")
    assert hasattr(c.CudaContext, "acquire_write")
    assert hasattr(c.CudaContext, "try_acquire_read")
    assert hasattr(c.CudaContext, "try_acquire_write")
    assert hasattr(c.CudaContext, "from_runtime")


def test_capsule_destructor_registry_survives_concurrent_create_drop():
    """Stress the lock-guarded registry across many threads creating
    and dropping capsules concurrently.

    On standard (GIL-protected) CPython this test passes whether the
    `threading.Lock` is present or not — the GIL serializes dict ops
    at the bytecode level, so the test alone can't catch a
    missing-lock regression on a typical CI runner.

    Where the test earns its keep is **PEP 703 free-threaded Python
    (3.13t+)**: there, `_retain_destructor` and `_release_destructor`
    can interleave on the dict and the explicit lock is what keeps
    the registry coherent. Treating this as a no-GIL-readiness gate
    means free-threaded CI (when we add it) catches a regression that
    standard CI can't see.

    The test still has standalone value on GIL-CPython: it confirms
    the registry drains end-to-end across a many-threads churn
    pattern (no leaked entries from a thread that didn't get cleaned
    up; no double-pop crashes from a destructor running while
    another thread is creating a new capsule).
    """
    import threading
    import gc

    initial_size = len(c._capsule_destructors)
    n_threads = 8
    n_capsules_per_thread = 16

    barrier = threading.Barrier(n_threads)

    def churn():
        # Synchronize start so the threads fight for the lock.
        barrier.wait(timeout=5.0)
        # Each thread allocates its own fake DLManagedTensors and creates
        # / drops capsules over them. The deleter slot is NULL, so the
        # destructor's "consumed" path is never exercised — we're only
        # gating registry pin/release atomicity.
        capsules = []
        backing = []
        for _ in range(n_capsules_per_thread):
            mt = (ctypes.c_uint8 * 64)()
            backing.append(mt)
            capsules.append(c._make_dlpack_capsule(ctypes.addressof(mt)))
        # Drop refs in reverse so the destructor calls don't all race
        # with the thread's local frame teardown.
        capsules.clear()
        backing.clear()

    threads = [threading.Thread(target=churn) for _ in range(n_threads)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=10.0)

    # Pump GC so any straggling capsules are finalized.
    for _ in range(3):
        gc.collect()

    assert len(c._capsule_destructors) == initial_size, (
        f"Registry must drain back to {initial_size} entries after concurrent "
        f"churn; found {len(c._capsule_destructors)}. Either the lock isn't "
        f"actually serializing dict mutations or the destructor's release "
        f"path raced against the create path."
    )

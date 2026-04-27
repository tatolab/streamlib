# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Unit tests for the Python cpu-readback subprocess runtime (#529).

These tests exercise the wire-protocol glue and view assembly without
spinning up a real subprocess or GPU. The escalate channel and
``gpu_limited_access`` are stubbed so the test asserts:

* the request shape matches the JTD schema (op / surface_id / mode);
* per-plane staging surfaces are resolved + locked + unlocked +
  released in the right order;
* views expose ``memoryview`` and numpy-array aliases over the same
  staging memory;
* failures during plane resolve unwind cleanly (no leaked locks);
* ``release_handle`` always fires on context exit.

A real subprocess+GPU end-to-end test ships with the polyglot E2E
harness in ``examples/python-cpu-readback-cv2-blur/``.
"""

from __future__ import annotations

import ctypes
from dataclasses import dataclass
from typing import Any, Dict, List, Optional, Tuple

import numpy as np
import pytest

from streamlib.adapters.cpu_readback import (
    CpuReadbackContext,
    CpuReadbackPlaneView,
    CpuReadbackPlaneViewMut,
    CpuReadbackReadView,
    CpuReadbackWriteView,
)
from streamlib.surface_adapter import SurfaceFormat


# ---------------------------------------------------------------------------
# Test doubles
# ---------------------------------------------------------------------------


class _FakeStagingHandle:
    """Stand-in for ``NativeGpuSurfaceHandle`` backed by a Python bytearray.

    Tracks lock/unlock/release calls so tests can assert the correct
    teardown order.
    """

    def __init__(self, width: int, height: int, bytes_per_pixel: int):
        self.width = width
        self.height = height
        self.bytes_per_pixel = bytes_per_pixel
        self.bytes_per_row = width * bytes_per_pixel
        self._buffer = (
            ctypes.c_uint8 * (self.bytes_per_row * height)
        )()
        self.locks: List[bool] = []
        self.unlocks: List[bool] = []
        self.released = False

    @property
    def base_address(self) -> int:
        return ctypes.addressof(self._buffer)

    def lock(self, read_only: bool = True) -> None:
        self.locks.append(read_only)

    def unlock(self, read_only: bool = True) -> None:
        self.unlocks.append(read_only)

    def release(self) -> None:
        self.released = True


class _FakeGpuLimitedAccess:
    """Stand-in for ``NativeGpuContextLimitedAccess.resolve_surface``."""

    def __init__(self, plane_handles: Dict[str, _FakeStagingHandle]):
        self._handles = plane_handles
        self.resolved: List[str] = []

    def resolve_surface(self, staging_id: str) -> _FakeStagingHandle:
        self.resolved.append(staging_id)
        if staging_id not in self._handles:
            raise RuntimeError(f"fake host: unknown staging surface {staging_id!r}")
        return self._handles[staging_id]


@dataclass
class _RecordedRequest:
    op: str
    surface_id: Optional[str]
    mode: Optional[str]
    handle_id: Optional[str]


class _FakeEscalateChannel:
    """Stand-in for ``EscalateChannel``. Records every request and
    returns whichever responses the test queued for ``acquire_cpu_readback``
    and ``release_handle`` ops."""

    def __init__(
        self,
        acquire_response: Dict[str, Any],
        release_response: Optional[Dict[str, Any]] = None,
    ):
        self._acquire_response = acquire_response
        self._release_response = release_response or {
            "result": "ok",
            "request_id": "release",
            "handle_id": "fake-handle",
        }
        self.requests: List[_RecordedRequest] = []

    def acquire_cpu_readback(
        self, surface_id: int, mode: str
    ) -> Dict[str, Any]:
        self.requests.append(
            _RecordedRequest(
                op="acquire_cpu_readback",
                surface_id=str(int(surface_id)),
                mode=mode,
                handle_id=None,
            )
        )
        return self._acquire_response

    def release_handle(self, handle_id: str) -> Dict[str, Any]:
        self.requests.append(
            _RecordedRequest(
                op="release_handle",
                surface_id=None,
                mode=None,
                handle_id=handle_id,
            )
        )
        return self._release_response


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


def _bgra_acquire_response(handle_id: str = "host-handle-1") -> Dict[str, Any]:
    return {
        "result": "ok",
        "request_id": "req-1",
        "handle_id": handle_id,
        "width": 4,
        "height": 2,
        "format": "bgra8",
        "cpu_readback_planes": [
            {
                "staging_surface_id": "stg-bgra-0",
                "width": 4,
                "height": 2,
                "bytes_per_pixel": 4,
            }
        ],
    }


def _nv12_acquire_response(handle_id: str = "host-handle-nv12") -> Dict[str, Any]:
    # NV12 8x4: Y plane 8x4 (1bpp), UV plane 4x2 (2bpp).
    return {
        "result": "ok",
        "request_id": "req-nv12",
        "handle_id": handle_id,
        "width": 8,
        "height": 4,
        "format": "nv12",
        "cpu_readback_planes": [
            {
                "staging_surface_id": "stg-nv12-y",
                "width": 8,
                "height": 4,
                "bytes_per_pixel": 1,
            },
            {
                "staging_surface_id": "stg-nv12-uv",
                "width": 4,
                "height": 2,
                "bytes_per_pixel": 2,
            },
        ],
    }


def test_acquire_write_round_trip_bgra_aliases_staging_buffer():
    """Customer can mutate the numpy array; the bytes/memoryview view
    sees the same memory; release_handle fires on exit."""
    handle = _FakeStagingHandle(width=4, height=2, bytes_per_pixel=4)
    gpu = _FakeGpuLimitedAccess({"stg-bgra-0": handle})
    escalate = _FakeEscalateChannel(_bgra_acquire_response())
    ctx = CpuReadbackContext(gpu, escalate)

    surface_id = 42
    with ctx.acquire_write(surface_id) as view:
        assert isinstance(view, CpuReadbackWriteView)
        assert view.format == SurfaceFormat.BGRA8
        assert view.width == 4 and view.height == 2
        assert view.plane_count == 1
        plane = view.plane(0)
        assert isinstance(plane, CpuReadbackPlaneViewMut)
        # Mutate via numpy: paint the entire plane red (BGRA = 00 00 FF FF).
        plane.numpy[..., :] = (0, 0, 255, 255)
        # The bytes view sees the same memory (alias, not a copy).
        assert bytes(plane.bytes[:4]) == b"\x00\x00\xff\xff"

    # Order of requests: acquire then release.
    assert [r.op for r in escalate.requests] == [
        "acquire_cpu_readback",
        "release_handle",
    ]
    # Wire format: surface_id is decimal string (JTD has no native u64).
    acquire = escalate.requests[0]
    assert acquire.surface_id == str(surface_id)
    assert acquire.mode == "write"
    # release_handle echoes the host's handle_id.
    assert escalate.requests[1].handle_id == "host-handle-1"
    # Lifecycle: locked-for-write, unlocked-for-write, released.
    assert handle.locks == [False]
    assert handle.unlocks == [False]
    assert handle.released is True


def test_acquire_read_uses_read_only_lock_and_returns_immutable_view():
    handle = _FakeStagingHandle(width=4, height=2, bytes_per_pixel=4)
    gpu = _FakeGpuLimitedAccess({"stg-bgra-0": handle})
    escalate = _FakeEscalateChannel(_bgra_acquire_response("read-handle"))
    ctx = CpuReadbackContext(gpu, escalate)

    with ctx.acquire_read(surface=7) as view:
        assert isinstance(view, CpuReadbackReadView)
        assert view.plane_count == 1
        assert isinstance(view.plane(0), CpuReadbackPlaneView)
        # The read view's numpy still aliases — we just rely on the
        # type system + customer convention to not write through it.
        assert view.plane(0).numpy.shape == (2, 4, 4)

    assert escalate.requests[0].mode == "read"
    # Read locks the staging surface read-only.
    assert handle.locks == [True]
    assert handle.unlocks == [True]
    assert handle.released is True


def test_acquire_write_nv12_exposes_two_planes_with_correct_geometry():
    y_handle = _FakeStagingHandle(width=8, height=4, bytes_per_pixel=1)
    uv_handle = _FakeStagingHandle(width=4, height=2, bytes_per_pixel=2)
    gpu = _FakeGpuLimitedAccess(
        {"stg-nv12-y": y_handle, "stg-nv12-uv": uv_handle}
    )
    escalate = _FakeEscalateChannel(_nv12_acquire_response())
    ctx = CpuReadbackContext(gpu, escalate)

    with ctx.acquire_write(surface=99) as view:
        assert view.format == SurfaceFormat.NV12
        assert view.plane_count == 2
        y, uv = view.plane(0), view.plane(1)
        # NV12 plane 0: full-resolution Y, 1 bpp.
        assert (y.width, y.height, y.bytes_per_pixel) == (8, 4, 1)
        assert y.numpy.shape == (4, 8, 1)
        # NV12 plane 1: half-resolution interleaved UV, 2 bpp.
        assert (uv.width, uv.height, uv.bytes_per_pixel) == (4, 2, 2)
        assert uv.numpy.shape == (2, 4, 2)
        # Independent backing — writing to Y must not touch UV.
        y.numpy[...] = 200
        assert int(np.asarray(uv.numpy).sum()) == 0

    assert gpu.resolved == ["stg-nv12-y", "stg-nv12-uv"]
    assert y_handle.released and uv_handle.released


def test_release_handle_fires_even_when_view_assembly_fails():
    """If a plane fails to resolve mid-assembly, the prior planes get
    unlocked + released and the host-side handle still gets dropped."""
    y_handle = _FakeStagingHandle(width=8, height=4, bytes_per_pixel=1)
    # Deliberately omit the UV plane so resolve_surface raises.
    gpu = _FakeGpuLimitedAccess({"stg-nv12-y": y_handle})
    escalate = _FakeEscalateChannel(_nv12_acquire_response("nv12-h"))
    ctx = CpuReadbackContext(gpu, escalate)

    with pytest.raises(RuntimeError, match="unknown staging surface"):
        with ctx.acquire_write(surface=99) as _view:
            pytest.fail("should not reach the body — UV resolve failed")

    # Y plane was locked then unlocked + released on the unwind path.
    assert y_handle.locks == [False]
    assert y_handle.unlocks == [False]
    assert y_handle.released is True
    # release_handle still fired even though acquire raised — the host
    # would otherwise leak the adapter guard.
    release_calls = [r for r in escalate.requests if r.op == "release_handle"]
    assert len(release_calls) == 1
    assert release_calls[0].handle_id == "nv12-h"


def test_acquire_write_propagates_escalate_error():
    """When the host returns Err, the context manager surfaces it as
    EscalateError and never enters the body."""
    from streamlib.escalate import EscalateError

    class _ErrChannel:
        def acquire_cpu_readback(self, surface_id, mode):
            raise EscalateError(
                "host returned err: surface 42 not registered with adapter"
            )

        def release_handle(self, handle_id):
            pytest.fail("release_handle must not fire when acquire raised")

    ctx = CpuReadbackContext(
        _FakeGpuLimitedAccess({}),
        _ErrChannel(),
    )
    with pytest.raises(EscalateError, match="not registered"):
        with ctx.acquire_write(surface=42) as _:
            pytest.fail("body must not run")


def test_acquire_write_accepts_streamlib_surface_protocol_object():
    """`CpuReadbackContext` accepts either a bare int or any object
    with an `id` attribute matching the StreamlibSurface Protocol."""

    @dataclass
    class _SurfaceLike:
        id: int

    handle = _FakeStagingHandle(width=4, height=2, bytes_per_pixel=4)
    gpu = _FakeGpuLimitedAccess({"stg-bgra-0": handle})
    escalate = _FakeEscalateChannel(_bgra_acquire_response())
    ctx = CpuReadbackContext(gpu, escalate)

    with ctx.acquire_write(_SurfaceLike(id=12345)) as _:
        pass

    assert escalate.requests[0].surface_id == "12345"


def test_acquire_cpu_readback_rejects_invalid_mode_on_channel():
    """`EscalateChannel.acquire_cpu_readback` rejects bad modes locally
    so a typo doesn't end up on the wire."""
    from streamlib.escalate import EscalateChannel

    # The channel doesn't actually need real pipes for this validation.
    channel = EscalateChannel.__new__(EscalateChannel)  # bypass __init__
    with pytest.raises(ValueError, match="must be 'read' or 'write'"):
        channel.acquire_cpu_readback(42, "read-only")

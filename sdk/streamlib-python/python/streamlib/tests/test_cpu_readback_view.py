# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Tests for the lazy-numpy contract on cpu-readback plane views.

Pins issue #603: a polyglot Python consumer that only touches
``plane.bytes`` (a memoryview) must be able to acquire / construct a
view without numpy installed. ``plane.numpy`` is opt-in and only
imports numpy on access.
"""

import ctypes
import sys

import pytest

from streamlib.adapters.cpu_readback import (
    CpuReadbackPlaneView,
    CpuReadbackPlaneViewMut,
)


def _writable_memoryview(byte_size: int) -> tuple[memoryview, "ctypes.Array[ctypes.c_uint8]"]:
    """Allocate a writable byte buffer + return (memoryview, raw)."""
    raw = (ctypes.c_uint8 * byte_size)()
    mv = memoryview(raw).cast("B")
    return mv, raw


def test_plane_view_constructs_without_numpy(monkeypatch):
    """Constructing a plane view and reading .bytes must not import numpy."""
    monkeypatch.setitem(sys.modules, "numpy", None)

    mv, _raw = _writable_memoryview(2 * 2 * 4)
    view = CpuReadbackPlaneView(
        width=2, height=2, bytes_per_pixel=4, bytes=mv.toreadonly()
    )

    assert view.width == 2
    assert view.height == 2
    assert view.bytes_per_pixel == 4
    assert view.row_stride == 8
    assert view.bytes.readonly
    assert view.bytes.nbytes == 16


def test_plane_view_mut_constructs_without_numpy(monkeypatch):
    """Mutable plane view: .bytes is writable, no numpy on the path."""
    monkeypatch.setitem(sys.modules, "numpy", None)

    mv, _raw = _writable_memoryview(2 * 2 * 4)
    view = CpuReadbackPlaneViewMut(
        width=2, height=2, bytes_per_pixel=4, bytes=mv
    )

    view.bytes[0] = 0xAB
    assert view.bytes[0] == 0xAB
    assert not view.bytes.readonly


def test_plane_view_numpy_raises_when_numpy_missing(monkeypatch):
    """Accessing .numpy without numpy installed surfaces ModuleNotFoundError."""
    monkeypatch.setitem(sys.modules, "numpy", None)

    mv, _raw = _writable_memoryview(4)
    view = CpuReadbackPlaneView(
        width=1, height=1, bytes_per_pixel=4, bytes=mv.toreadonly()
    )

    with pytest.raises(ModuleNotFoundError):
        _ = view.numpy


def test_plane_view_mut_numpy_raises_when_numpy_missing(monkeypatch):
    """Same lazy contract on the mutable variant."""
    monkeypatch.setitem(sys.modules, "numpy", None)

    mv, _raw = _writable_memoryview(4)
    view = CpuReadbackPlaneViewMut(
        width=1, height=1, bytes_per_pixel=4, bytes=mv
    )

    with pytest.raises(ModuleNotFoundError):
        _ = view.numpy


def test_plane_view_numpy_aliases_bytes_when_numpy_installed():
    """When numpy is available, .numpy returns a (H, W, BPP) uint8 ndarray
    that aliases .bytes — write through one, see it on the other."""
    np = pytest.importorskip("numpy")

    mv, _raw = _writable_memoryview(2 * 3 * 4)
    view = CpuReadbackPlaneViewMut(
        width=3, height=2, bytes_per_pixel=4, bytes=mv
    )

    arr = view.numpy
    assert arr.shape == (2, 3, 4)
    assert arr.dtype == np.uint8

    arr[1, 2, 3] = 0xCD
    # Last byte of a 24-byte buffer reflects the write.
    assert view.bytes[2 * 3 * 4 - 1] == 0xCD
    # Bidirectional: write through .bytes, see it via the same ndarray.
    view.bytes[0] = 0x42
    assert arr[0, 0, 0] == 0x42


def test_plane_view_numpy_inherits_readonly_from_bytes():
    """Read-only plane view: numpy view is read-only too."""
    np = pytest.importorskip("numpy")

    mv, _raw = _writable_memoryview(4)
    view = CpuReadbackPlaneView(
        width=1, height=1, bytes_per_pixel=4, bytes=mv.toreadonly()
    )

    arr = view.numpy
    assert arr.shape == (1, 1, 4)
    assert not arr.flags.writeable
    with pytest.raises(ValueError):
        arr[0, 0, 0] = 1

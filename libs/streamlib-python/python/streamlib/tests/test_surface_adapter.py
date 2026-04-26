# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Layout regression suite for the Python ctypes mirror of
streamlib_adapter_abi::StreamlibSurface and friends.

Numbers must match the Rust unit tests in
libs/streamlib-adapter-abi/src/surface.rs (search for `streamlib_surface_layout`).
When this file changes, update the Rust tests too — both are the
contract.
"""

from __future__ import annotations

import ctypes

from streamlib.surface_adapter import (
    AccessMode,
    MAX_DMA_BUF_PLANES,
    STREAMLIB_ADAPTER_ABI_VERSION,
    SurfaceFormat,
    SurfaceUsage,
    _StreamlibSurfaceC,
    _SurfaceSyncStateC,
    _SurfaceTransportHandleC,
)


def test_max_dma_buf_planes_matches_rust():
    assert MAX_DMA_BUF_PLANES == 4


def test_abi_version_matches_rust():
    # When this constant flips to 2, also update Rust side and Deno
    # mirror in the same commit.
    assert STREAMLIB_ADAPTER_ABI_VERSION == 1


def test_surface_format_is_4_bytes():
    assert ctypes.sizeof(ctypes.c_uint32) == 4
    assert int(SurfaceFormat.BGRA8) == 0
    assert int(SurfaceFormat.RGBA8) == 1
    assert int(SurfaceFormat.NV12) == 2


def test_surface_usage_flag_values():
    assert int(SurfaceUsage.RENDER_TARGET) == 1
    assert int(SurfaceUsage.SAMPLED) == 2
    assert int(SurfaceUsage.CPU_READBACK) == 4


def test_access_mode_values():
    assert int(AccessMode.READ) == 0
    assert int(AccessMode.WRITE) == 1


def test_surface_transport_handle_layout():
    """Locks the Rust `#[repr(C)] SurfaceTransportHandle` layout."""
    # Per Rust unit test:
    # plane_count: u32 @ 0
    # dma_buf_fds: [i32; 4] @ 4
    # plane_offsets: [u64; 4] @ 24 (alignment pad 20→24)
    # plane_strides: [u64; 4] @ 56
    # drm_format_modifier: u64 @ 88
    # total: 96 bytes, align 8
    assert _SurfaceTransportHandleC.plane_count.offset == 0
    assert _SurfaceTransportHandleC.dma_buf_fds.offset == 4
    assert _SurfaceTransportHandleC.plane_offsets.offset == 24
    assert _SurfaceTransportHandleC.plane_strides.offset == 56
    assert _SurfaceTransportHandleC.drm_format_modifier.offset == 88
    assert ctypes.sizeof(_SurfaceTransportHandleC) == 96
    assert ctypes.alignment(_SurfaceTransportHandleC) == 8


def test_surface_sync_state_layout():
    assert _SurfaceSyncStateC.timeline_semaphore.offset == 0
    assert _SurfaceSyncStateC.last_acquire_value.offset == 8
    assert _SurfaceSyncStateC.last_release_value.offset == 16
    assert _SurfaceSyncStateC.current_image_layout.offset == 24
    assert _SurfaceSyncStateC._pad.offset == 28
    assert ctypes.sizeof(_SurfaceSyncStateC) == 32
    assert ctypes.alignment(_SurfaceSyncStateC) == 8


def test_streamlib_surface_layout():
    """Locks the Rust `#[repr(C)] StreamlibSurface` layout."""
    # id: u64 @ 0; width: u32 @ 8; height: u32 @ 12; format: u32 @ 16;
    # usage: u32 @ 20; transport: SurfaceTransportHandle @ 24;
    # sync: SurfaceSyncState @ 120; total 152, align 8.
    assert _StreamlibSurfaceC.id.offset == 0
    assert _StreamlibSurfaceC.width.offset == 8
    assert _StreamlibSurfaceC.height.offset == 12
    assert _StreamlibSurfaceC.format.offset == 16
    assert _StreamlibSurfaceC.usage.offset == 20
    assert _StreamlibSurfaceC.transport.offset == 24
    assert _StreamlibSurfaceC.sync.offset == 120
    assert ctypes.sizeof(_StreamlibSurfaceC) == 152
    assert ctypes.alignment(_StreamlibSurfaceC) == 8


def test_surface_format_round_trip():
    """Construct a surface with format=NV12 and read it back."""
    s = _StreamlibSurfaceC()
    s.id = 0xDEAD_BEEF
    s.width = 1920
    s.height = 1080
    s.format = int(SurfaceFormat.NV12)
    s.usage = int(SurfaceUsage.SAMPLED | SurfaceUsage.RENDER_TARGET)
    assert s.id == 0xDEAD_BEEF
    assert s.width == 1920
    assert s.height == 1080
    assert SurfaceFormat(s.format) is SurfaceFormat.NV12
    flags = SurfaceUsage(s.usage)
    assert SurfaceUsage.SAMPLED in flags
    assert SurfaceUsage.RENDER_TARGET in flags
    assert SurfaceUsage.CPU_READBACK not in flags

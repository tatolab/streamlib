// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Layout regression suite for the Deno mirror of
 * streamlib_adapter_abi::StreamlibSurface.
 *
 * These offsets must match the Rust unit tests in
 * libs/streamlib-adapter-abi/src/surface.rs (`streamlib_surface_layout`,
 * `surface_transport_handle_layout`, `surface_sync_state_layout`) and
 * the Python mirror in
 * libs/streamlib-python/python/streamlib/tests/test_surface_adapter.py.
 *
 * When this file changes, update both other mirrors in the same commit.
 */

import { assertEquals, assertThrows } from "@std/assert";
import {
  AccessMode,
  MAX_DMA_BUF_PLANES,
  STREAMLIB_ADAPTER_ABI_VERSION,
  SurfaceFormat,
  surfaceFormatPlaneBytesPerPixel,
  surfaceFormatPlaneCount,
  surfaceFormatPlaneHeight,
  surfaceFormatPlaneWidth,
  SurfaceLayout,
  SurfaceUsage,
} from "./surface_adapter.ts";

Deno.test("MAX_DMA_BUF_PLANES matches Rust", () => {
  assertEquals(MAX_DMA_BUF_PLANES, 4);
});

Deno.test("STREAMLIB_ADAPTER_ABI_VERSION matches Rust", () => {
  assertEquals(STREAMLIB_ADAPTER_ABI_VERSION, 1);
});

Deno.test("SurfaceFormat values match Rust #[repr(u32)] layout", () => {
  assertEquals(SurfaceFormat.Bgra8, 0);
  assertEquals(SurfaceFormat.Rgba8, 1);
  assertEquals(SurfaceFormat.Nv12, 2);
});

Deno.test("SurfaceFormat plane_count matches Rust", () => {
  assertEquals(surfaceFormatPlaneCount(SurfaceFormat.Bgra8), 1);
  assertEquals(surfaceFormatPlaneCount(SurfaceFormat.Rgba8), 1);
  assertEquals(surfaceFormatPlaneCount(SurfaceFormat.Nv12), 2);
});

Deno.test("SurfaceFormat plane geometry matches Rust", () => {
  // BGRA: 1 plane, 4 bpp, full size.
  assertEquals(surfaceFormatPlaneBytesPerPixel(SurfaceFormat.Bgra8, 0), 4);
  assertEquals(surfaceFormatPlaneWidth(SurfaceFormat.Bgra8, 64, 0), 64);
  assertEquals(surfaceFormatPlaneHeight(SurfaceFormat.Bgra8, 48, 0), 48);

  // NV12 plane 0 (Y) — full resolution, 1 byte per texel.
  assertEquals(surfaceFormatPlaneBytesPerPixel(SurfaceFormat.Nv12, 0), 1);
  assertEquals(surfaceFormatPlaneWidth(SurfaceFormat.Nv12, 64, 0), 64);
  assertEquals(surfaceFormatPlaneHeight(SurfaceFormat.Nv12, 48, 0), 48);

  // NV12 plane 1 (UV interleaved) — half resolution, 2 bytes per texel.
  assertEquals(surfaceFormatPlaneBytesPerPixel(SurfaceFormat.Nv12, 1), 2);
  assertEquals(surfaceFormatPlaneWidth(SurfaceFormat.Nv12, 64, 1), 32);
  assertEquals(surfaceFormatPlaneHeight(SurfaceFormat.Nv12, 48, 1), 24);
});

Deno.test("SurfaceFormat plane out-of-range throws RangeError", () => {
  assertThrows(
    () => surfaceFormatPlaneBytesPerPixel(SurfaceFormat.Bgra8, 1),
    RangeError,
  );
  assertThrows(
    () => surfaceFormatPlaneBytesPerPixel(SurfaceFormat.Nv12, 2),
    RangeError,
  );
});

Deno.test("SurfaceUsage flag bits match Rust bitflags", () => {
  assertEquals(SurfaceUsage.RenderTarget, 1);
  assertEquals(SurfaceUsage.Sampled, 2);
  assertEquals(SurfaceUsage.CpuReadback, 4);
});

Deno.test("AccessMode wire values match Rust #[repr(u32)]", () => {
  assertEquals(AccessMode.Read, 0);
  assertEquals(AccessMode.Write, 1);
});

Deno.test("SurfaceTransportHandle layout matches Rust", () => {
  // Rust offsets:
  //   plane_count: u32 @ 0
  //   dma_buf_fds: [i32; 4] @ 4
  //   plane_offsets: [u64; 4] @ 24 (alignment pad 20→24)
  //   plane_strides: [u64; 4] @ 56
  //   drm_format_modifier: u64 @ 88
  //   total: 96, align 8
  const t = SurfaceLayout.TransportHandle;
  assertEquals(t.Offsets.planeCount, 0);
  assertEquals(t.Offsets.dmaBufFds, 4);
  assertEquals(t.Offsets.planeOffsets, 24);
  assertEquals(t.Offsets.planeStrides, 56);
  assertEquals(t.Offsets.drmFormatModifier, 88);
  assertEquals(t.Size, 96);
  assertEquals(t.Align, 8);
});

Deno.test("SurfaceSyncState layout matches Rust", () => {
  // timeline_semaphore_handle: u64 @ 0
  // timeline_semaphore_sync_fd: i32 @ 8
  // _pad_a: u32 @ 12
  // last_acquire_value: u64 @ 16
  // last_release_value: u64 @ 24
  // current_image_layout: i32 @ 32
  // _pad_b: u32 @ 36
  // _reserved: [u8; 16] @ 40
  // total: 56, align 8
  const s = SurfaceLayout.SyncState;
  assertEquals(s.Offsets.timelineSemaphoreHandle, 0);
  assertEquals(s.Offsets.timelineSemaphoreSyncFd, 8);
  assertEquals(s.Offsets.padA, 12);
  assertEquals(s.Offsets.lastAcquireValue, 16);
  assertEquals(s.Offsets.lastReleaseValue, 24);
  assertEquals(s.Offsets.currentImageLayout, 32);
  assertEquals(s.Offsets.padB, 36);
  assertEquals(s.Offsets.reserved, 40);
  assertEquals(s.Size, 56);
  assertEquals(s.Align, 8);
});

Deno.test("StreamlibSurface layout matches Rust", () => {
  const s = SurfaceLayout.Surface;
  assertEquals(s.Offsets.id, 0);
  assertEquals(s.Offsets.width, 8);
  assertEquals(s.Offsets.height, 12);
  assertEquals(s.Offsets.format, 16);
  assertEquals(s.Offsets.usage, 20);
  assertEquals(s.Offsets.transport, 24);
  assertEquals(s.Offsets.sync, 120);
  assertEquals(s.Size, 176);
  assertEquals(s.Align, 8);
});

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

import { assertEquals } from "@std/assert";
import {
  AccessMode,
  MAX_DMA_BUF_PLANES,
  STREAMLIB_ADAPTER_ABI_VERSION,
  SurfaceFormat,
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
  const s = SurfaceLayout.SyncState;
  assertEquals(s.Offsets.timelineSemaphore, 0);
  assertEquals(s.Offsets.lastAcquireValue, 8);
  assertEquals(s.Offsets.lastReleaseValue, 16);
  assertEquals(s.Offsets.currentImageLayout, 24);
  assertEquals(s.Offsets.pad, 28);
  assertEquals(s.Size, 32);
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
  assertEquals(s.Size, 152);
  assertEquals(s.Align, 8);
});

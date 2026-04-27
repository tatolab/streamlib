// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Explicit GPU→CPU surface adapter — Deno customer-facing API.
 *
 * Mirrors the Rust crate `streamlib-adapter-cpu-readback` (#514, #533).
 * The subprocess's actual GPU→CPU copy is performed by the host
 * adapter via per-plane `vkCmdCopyImageToBuffer` against per-plane
 * HOST_VISIBLE staging buffers; this module declares the type shapes
 * a Deno customer programs against.
 *
 *  - `CpuReadbackPlaneView` / `CpuReadbackPlaneViewMut` — per-plane
 *    byte slices (`Uint8Array`) and dimensions in plane texels. NV12
 *    UV plane has half the surface width × half the surface height.
 *  - `CpuReadbackReadView` / `CpuReadbackWriteView` — surface-level
 *    metadata plus the array of plane views inside `acquireRead` /
 *    `acquireWrite` scopes. `planeCount` reflects the surface's
 *    `SurfaceFormat`: 1 for BGRA8/RGBA8, 2 for NV12.
 *  - `CpuReadbackContext` interface — runtime hands one out;
 *    customers use TC39 `using` blocks for scoped acquire/release.
 *
 * This is the **single sanctioned CPU exit** in the surface-adapter
 * architecture. GPU adapters (`vulkan`, `opengl`, `skia`)
 * deliberately do not expose CPU bytes — switching to this adapter
 * is the contractual signal that you've opted in to a host-side
 * GPU→CPU roundtrip. Do not use this in performance-critical
 * pipelines; the copy is per-acquire and blocks on
 * `vkQueueWaitIdle`.
 */

import {
  STREAMLIB_ADAPTER_ABI_VERSION,
  type StreamlibSurface,
  type SurfaceAccessGuard,
  type SurfaceFormat,
} from "../surface_adapter.ts";

export { STREAMLIB_ADAPTER_ABI_VERSION };

/** Read-only view of a single plane of an acquired surface. */
export interface CpuReadbackPlaneView {
  /** Plane width in texels. */
  readonly width: number;
  /** Plane height in texels. */
  readonly height: number;
  /** Bytes per texel of this plane (BGRA: 4, NV12 Y: 1, NV12 UV: 2). */
  readonly bytesPerPixel: number;
  /** Tightly-packed row stride in bytes (`width * bytesPerPixel`). */
  readonly rowStride: number;
  /** Read-only view of this plane's staging buffer. */
  readonly bytes: Uint8Array;
}

/** Mutable view of a single plane of an acquired surface. */
export interface CpuReadbackPlaneViewMut {
  readonly width: number;
  readonly height: number;
  readonly bytesPerPixel: number;
  readonly rowStride: number;
  /** Mutable view of this plane's staging buffer. */
  readonly bytes: Uint8Array;
}

/** Read-side view inside an `acquireRead` scope. */
export interface CpuReadbackReadView {
  readonly width: number;
  readonly height: number;
  readonly format: SurfaceFormat;
  /** Number of planes (1 for BGRA/RGBA, 2 for NV12). */
  readonly planeCount: number;
  /** All planes in declaration order — for NV12, `[Y, UV]`. */
  readonly planes: readonly CpuReadbackPlaneView[];
  /** Borrow plane `index`. Throws `RangeError` on out-of-range. */
  plane(index: number): CpuReadbackPlaneView;
}

/** Write-side view inside an `acquireWrite` scope. Edits to any
 * plane's `bytes` are flushed back to the host `VkImage` via per-
 * plane `vkCmdCopyBufferToImage` on guard drop. */
export interface CpuReadbackWriteView {
  readonly width: number;
  readonly height: number;
  readonly format: SurfaceFormat;
  readonly planeCount: number;
  readonly planes: readonly CpuReadbackPlaneViewMut[];
  plane(index: number): CpuReadbackPlaneViewMut;
}

/** Public cpu-readback adapter contract. */
export interface CpuReadbackSurfaceAdapter {
  acquireRead(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<CpuReadbackReadView>;
  acquireWrite(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<CpuReadbackWriteView>;
  tryAcquireRead(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<CpuReadbackReadView> | null;
  tryAcquireWrite(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<CpuReadbackWriteView> | null;
}

/** Customer-facing context. Same shape as the adapter — the runtime
 * wraps the adapter and hands the context out. Mirrors the Rust
 * `CpuReadbackContext`. */
export type CpuReadbackContext = CpuReadbackSurfaceAdapter;

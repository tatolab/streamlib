// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Explicit GPU→CPU surface adapter — Deno customer-facing API.
 *
 * Mirrors the Rust crate `streamlib-adapter-cpu-readback` (#514).
 * The subprocess's actual GPU→CPU copy is performed by the host
 * adapter via `vkCmdCopyImageToBuffer` against a HOST_VISIBLE
 * staging buffer; this module declares the type shapes a Deno
 * customer programs against.
 *
 *  - `CpuReadbackReadView` / `CpuReadbackWriteView` — typed views
 *    inside `acquireRead` / `acquireWrite` scopes. Both expose
 *    `bytes` as a tightly-packed `Uint8Array` (read-only on the
 *    read side, mutable on the write side). Length is exactly
 *    `width * height * bytesPerPixel`.
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
} from "../surface_adapter.ts";

export { STREAMLIB_ADAPTER_ABI_VERSION };

/** Read-side view inside an `acquireRead` scope. */
export interface CpuReadbackReadView {
  readonly width: number;
  readonly height: number;
  readonly bytesPerPixel: number;
  /** Tightly-packed row stride in bytes
   * (`width * bytesPerPixel`). */
  readonly rowStride: number;
  /** Read-only view of the staging buffer. The GPU→CPU copy already
   * happened at acquire time; reading is O(1). */
  readonly bytes: Uint8Array;
}

/** Write-side view inside an `acquireWrite` scope. */
export interface CpuReadbackWriteView {
  readonly width: number;
  readonly height: number;
  readonly bytesPerPixel: number;
  readonly rowStride: number;
  /** Mutable view of the staging buffer. Edits are flushed back to
   * the host `VkImage` via `vkCmdCopyBufferToImage` on guard drop. */
  readonly bytes: Uint8Array;
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

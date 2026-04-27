// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Explicit GPU→CPU surface adapter — Deno customer-facing API.
 *
 * Mirrors the Rust crate `streamlib-adapter-cpu-readback` (#514, #529,
 * #533). The subprocess's actual GPU→CPU copy is performed by the
 * host (the adapter runs in-process on the host and issues
 * `vkCmdCopyImageToBuffer` against per-plane HOST_VISIBLE staging
 * buffers).
 *
 *  - `CpuReadbackPlaneView` / `CpuReadbackPlaneViewMut` — per-plane
 *    byte slices (`Uint8Array`) and dimensions in plane texels. NV12
 *    UV plane has half the surface width × half the surface height.
 *  - `CpuReadbackReadView` / `CpuReadbackWriteView` — surface-level
 *    metadata plus the array of plane views inside `acquireRead` /
 *    `acquireWrite` scopes. `planeCount` reflects the surface's
 *    `SurfaceFormat`: 1 for BGRA8/RGBA8, 2 for NV12.
 *  - `CpuReadbackContext` — concrete subprocess runtime. Wires the
 *    SDK's escalate channel (host-side `acquire` / release op) to
 *    the surface-share `resolveSurface` path (per-plane staging
 *    buffer mmap). Customers use TC39 `await using` for scoped
 *    acquire/release.
 *
 * This is the **single sanctioned CPU exit** in the surface-adapter
 * architecture. GPU adapters (`vulkan`, `opengl`, `skia`)
 * deliberately do not expose CPU bytes — switching to this adapter
 * is the contractual signal that you've opted in to a host-side
 * GPU→CPU roundtrip. Do not use this in performance-critical
 * pipelines; the copy is per-acquire and the host blocks on a
 * per-submit fence.
 *
 * Note: the API here is `async` (Promises + `await using`), unlike
 * the synchronous Python equivalent. Deno's stdio + the escalate
 * channel are Promise-based; the language idiom for scoped async
 * release is `await using`. Customers `await` the acquire, then
 * leave scope normally — the guard's `[Symbol.asyncDispose]` runs
 * the release.
 */

import {
  STREAMLIB_ADAPTER_ABI_VERSION,
  type StreamlibSurface,
  type SurfaceFormat,
} from "../surface_adapter.ts";
import { getChannel } from "../escalate.ts";
import type { EscalateChannel, EscalateResponseOk } from "../escalate.ts";

export { STREAMLIB_ADAPTER_ABI_VERSION };

/** Minimal subset of `GpuContextLimitedAccess` the cpu-readback runtime
 * needs: per-plane staging-surface lookup. The full shape lives in
 * `context.ts`; we type against a structural subset here so tests can
 * stub it without dragging the FFI in. */
export interface CpuReadbackGpuLimitedAccess {
  resolveSurface(stagingSurfaceId: string): {
    readonly width: number;
    readonly height: number;
    readonly bytesPerRow: number;
    lock(readOnly: boolean): void;
    unlock(readOnly: boolean): void;
    asBuffer(): ArrayBuffer;
    release(): void;
  };
}

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

/** Async-disposable guard returned by `acquireRead` / `acquireWrite`.
 * `await using` runs `[Symbol.asyncDispose]` at scope exit, which
 * unlocks every plane's staging buffer and tells the host to release
 * the adapter guard (CPU→GPU flush on write + timeline signal). */
export interface CpuReadbackAccessGuard<V> extends AsyncDisposable {
  readonly view: V;
  readonly handleId: string;
}

const _FORMAT_FROM_WIRE: Record<string, SurfaceFormat> = {
  bgra8: 0 as SurfaceFormat, // SurfaceFormat.BGRA8
  rgba8: 1 as SurfaceFormat, // SurfaceFormat.RGBA8
  nv12: 2 as SurfaceFormat, // SurfaceFormat.NV12
};

function _formatFromWire(wire: string | undefined): SurfaceFormat {
  if (!wire) return 0 as SurfaceFormat; // BGRA8 default
  const fmt = _FORMAT_FROM_WIRE[wire.toLowerCase()];
  if (fmt === undefined) {
    throw new Error(
      `unknown cpu-readback surface format on the wire: ${JSON.stringify(wire)}`,
    );
  }
  return fmt;
}

function _surfaceIdFrom(surface: StreamlibSurface | bigint | number): bigint {
  if (typeof surface === "bigint") return surface;
  if (typeof surface === "number") return BigInt(Math.trunc(surface));
  // StreamlibSurface descriptor
  const id = (surface as { id?: bigint | number }).id;
  if (id === undefined) {
    throw new TypeError(
      `CpuReadbackContext: expected StreamlibSurface, bigint, or number — got ${
        typeof surface
      }`,
    );
  }
  return typeof id === "bigint" ? id : BigInt(Math.trunc(id));
}

/** Customer-facing context for the Deno subprocess SDK.
 *
 * Customers obtain one via the SDK's `adapters` factory; tests
 * construct one directly with `new CpuReadbackContext(gpu, escalate)`.
 *
 *     await using guard = await ctx.acquireWrite(surface);
 *     guard.view.plane(0).bytes.set(myImage);
 *     // [Symbol.asyncDispose] runs at scope exit:
 *     //   unlock + release every plane staging buffer, then
 *     //   send release_handle so the host flushes CPU→GPU.
 */
export class CpuReadbackContext {
  private readonly gpu: CpuReadbackGpuLimitedAccess;
  private readonly escalate: EscalateChannel;

  constructor(gpu: CpuReadbackGpuLimitedAccess, escalate: EscalateChannel) {
    this.gpu = gpu;
    this.escalate = escalate;
  }

  /**
   * Build from a typed runtime context. Mirrors Python's
   * `CpuReadbackContext.from_runtime`. Pulls the process-wide escalate
   * channel singleton (installed by the subprocess runner) and pairs
   * it with the runtime context's `gpuLimitedAccess`.
   */
  static fromRuntime(
    ctx: { readonly gpuLimitedAccess: CpuReadbackGpuLimitedAccess },
  ): CpuReadbackContext {
    return new CpuReadbackContext(ctx.gpuLimitedAccess, getChannel());
  }

  async acquireRead(
    surface: StreamlibSurface | bigint | number,
  ): Promise<CpuReadbackAccessGuard<CpuReadbackReadView>> {
    return await this._acquire(surface, "read", false);
  }

  async acquireWrite(
    surface: StreamlibSurface | bigint | number,
  ): Promise<CpuReadbackAccessGuard<CpuReadbackWriteView>> {
    return (await this._acquire(
      surface,
      "write",
      true,
    )) as CpuReadbackAccessGuard<CpuReadbackWriteView>;
  }

  /** Non-blocking variant. Today the wire format is request/response,
   * so the host blocks on the bridge call; callers fall back to
   * `acquireRead` here. A future change can add a dedicated
   * try-acquire escalate op. */
  async tryAcquireRead(
    surface: StreamlibSurface | bigint | number,
  ): Promise<CpuReadbackAccessGuard<CpuReadbackReadView>> {
    return await this.acquireRead(surface);
  }

  async tryAcquireWrite(
    surface: StreamlibSurface | bigint | number,
  ): Promise<CpuReadbackAccessGuard<CpuReadbackWriteView>> {
    return await this.acquireWrite(surface);
  }

  private async _acquire(
    surface: StreamlibSurface | bigint | number,
    mode: "read" | "write",
    writable: boolean,
  ): Promise<CpuReadbackAccessGuard<CpuReadbackReadView | CpuReadbackWriteView>> {
    const surfaceId = _surfaceIdFrom(surface);
    const response = await this.escalate.acquireCpuReadback(surfaceId, mode);
    const handleId = response.handle_id;

    let view: CpuReadbackReadView | CpuReadbackWriteView;
    const lockedHandles: ReturnType<
      CpuReadbackGpuLimitedAccess["resolveSurface"]
    >[] = [];
    try {
      view = this._buildView(response, writable, lockedHandles);
    } catch (e) {
      // Unwind any plane handles already locked, then drop the host's
      // adapter guard so it doesn't leak.
      this._releaseLocked(lockedHandles, writable);
      try {
        await this.escalate.releaseHandle(handleId);
      } catch (_releaseErr) {
        // Surface the original failure — release errors during a
        // failure unwind are diagnostic-only.
      }
      throw e;
    }

    const release = async () => {
      this._releaseLocked(lockedHandles, writable);
      await this.escalate.releaseHandle(handleId);
    };

    return {
      view,
      handleId,
      [Symbol.asyncDispose]: release,
    };
  }

  private _releaseLocked(
    handles: ReturnType<CpuReadbackGpuLimitedAccess["resolveSurface"]>[],
    writable: boolean,
  ): void {
    // Release in reverse acquire order. Best-effort: a single bad
    // handle must not block the rest of the cleanup.
    for (let i = handles.length - 1; i >= 0; i -= 1) {
      const handle = handles[i];
      try {
        handle.unlock(!writable);
      } catch (_e) { /* swallow */ }
      try {
        handle.release();
      } catch (_e) { /* swallow */ }
    }
    handles.length = 0;
  }

  private _buildView(
    response: EscalateResponseOk,
    writable: boolean,
    lockedHandles: ReturnType<
      CpuReadbackGpuLimitedAccess["resolveSurface"]
    >[],
  ): CpuReadbackReadView | CpuReadbackWriteView {
    const format = _formatFromWire(response.format);
    const width = response.width ?? 0;
    const height = response.height ?? 0;
    const planeDescriptors = response.cpu_readback_planes ?? [];
    if (planeDescriptors.length === 0) {
      throw new Error(
        "cpu-readback acquire response missing cpu_readback_planes",
      );
    }

    const planeViews: (CpuReadbackPlaneView | CpuReadbackPlaneViewMut)[] = [];
    for (const descriptor of planeDescriptors) {
      const handle = this.gpu.resolveSurface(descriptor.staging_surface_id);
      handle.lock(!writable);
      lockedHandles.push(handle);

      const buf = handle.asBuffer();
      const expected = handle.bytesPerRow * descriptor.height;
      // The mmap may be larger than the tightly-packed plane (e.g. the
      // staging buffer was sized to bytesPerRow*height but the response
      // describes plane geometry). Slice to the descriptor extent so
      // customer-visible bytes match the documented shape.
      const bytes = new Uint8Array(buf, 0, expected);
      planeViews.push({
        width: descriptor.width,
        height: descriptor.height,
        bytesPerPixel: descriptor.bytes_per_pixel,
        rowStride: descriptor.width * descriptor.bytes_per_pixel,
        bytes,
      });
    }

    return {
      width,
      height,
      format,
      planeCount: planeViews.length,
      planes: planeViews,
      plane(index: number) {
        if (index < 0 || index >= planeViews.length) {
          throw new RangeError(
            `cpu-readback plane index ${index} out of range (0..${planeViews.length})`,
          );
        }
        return planeViews[index];
      },
    } as CpuReadbackReadView | CpuReadbackWriteView;
  }
}

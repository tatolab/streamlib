// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Explicit GPU→CPU surface adapter — Deno customer-facing API.
 *
 * Mirrors the Rust crate `streamlib-adapter-cpu-readback` (#562, Path E
 * single-pattern shape). The subprocess delegates to
 * `streamlib-deno-native`'s `sldn_cpu_readback_*` FFI surface, which
 * itself wraps the host adapter crate's
 * `CpuReadbackSurfaceAdapter<ConsumerVulkanDevice>` against a
 * subprocess-local Vulkan device. Per-acquire control flow:
 *
 * 1. The Deno SDK looks the host's pre-registered cpu-readback surface
 *    up via surface-share once (`sldn_cpu_readback_register_surface`).
 *    The DMA-BUF FDs and timeline OPAQUE_FD are consumed by the cdylib
 *    via `ConsumerVulkanPixelBuffer` / `ConsumerVulkanTimelineSemaphore`.
 * 2. On every `acquireRead` / `acquireWrite` the cdylib's adapter
 *    calls back into a JS-installed `Deno.UnsafeCallback` that sends a
 *    `run_cpu_readback_copy` escalate-IPC request to the host. The
 *    host runs `vkCmdCopyImageToBuffer` (or its inverse on write
 *    release), signals the shared timeline, replies with the new
 *    timeline value. The cdylib waits on the imported timeline through
 *    the carve-out and hands back mapped pointers as `Uint8Array`.
 *
 * This is the **single sanctioned CPU exit** in the surface-adapter
 * architecture. GPU adapters (`vulkan`, `opengl`, `skia`) deliberately
 * do not expose CPU bytes — switching to this adapter is the
 * contractual signal that you've opted in to a host-side GPU→CPU
 * roundtrip. Do not use this in performance-critical pipelines; the
 * copy is per-acquire and the host blocks on a per-submit timeline
 * wait.
 *
 * The acquire / release API here is `async` (Promises + `await using`)
 * because the trigger callback's escalate IPC must be `await`ed. Deno
 * runs JS callbacks invoked from FFI on the same event loop, so the
 * cdylib's synchronous `acquire_*` returns Promises here.
 */

import {
  STREAMLIB_ADAPTER_ABI_VERSION,
  type StreamlibSurface,
  type SurfaceFormat,
} from "../surface_adapter.ts";
import { getChannel } from "../escalate.ts";
import type { EscalateChannel } from "../escalate.ts";
import * as log from "../log.ts";

export { STREAMLIB_ADAPTER_ABI_VERSION };

/** Direction wire constants — match `SLDN_CPU_READBACK_DIRECTION_*`. */
const DIRECTION_IMAGE_TO_BUFFER = 0;
const DIRECTION_BUFFER_TO_IMAGE = 1;

const RC_OK = 0;
const RC_CONTENDED = 1;

/** Maximum planes the cdylib's view struct exposes. Matches
 * `SLDN_CPU_READBACK_MAX_PLANES`. */
const MAX_PLANES = 4;

/** `#[repr(C)]` plane struct: `{ptr u8, u32 w, u32 h, u32 bpp, u64 byte_size}`
 * with 4-byte tail padding before alignment to 8-byte boundary →
 * 8 + 4 + 4 + 4 + 4 (pad) + 8 = 32 bytes per plane. */
const PLANE_STRIDE = 32;
/** View struct: `{u32 w, u32 h, u32 fmt, u32 plane_count, planes[MAX_PLANES]}`
 * → 16 + MAX_PLANES * PLANE_STRIDE. */
const VIEW_STRUCT_SIZE = 16 + MAX_PLANES * PLANE_STRIDE;

/** Read-only view of a single plane of an acquired surface. */
export interface CpuReadbackPlaneView {
  readonly width: number;
  readonly height: number;
  readonly bytesPerPixel: number;
  readonly rowStride: number;
  readonly bytes: Uint8Array;
}

/** Mutable view of a single plane of an acquired surface. */
export interface CpuReadbackPlaneViewMut {
  readonly width: number;
  readonly height: number;
  readonly bytesPerPixel: number;
  readonly rowStride: number;
  readonly bytes: Uint8Array;
}

/** Read-side view inside an `acquireRead` scope. */
export interface CpuReadbackReadView {
  readonly width: number;
  readonly height: number;
  readonly format: SurfaceFormat;
  readonly planeCount: number;
  readonly planes: readonly CpuReadbackPlaneView[];
  plane(index: number): CpuReadbackPlaneView;
}

/** Write-side view inside an `acquireWrite` scope. Edits to any
 * plane's `bytes` are flushed back to the host `VkImage` on guard
 * drop. */
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
 * tells the host to release the adapter guard (CPU→GPU flush on
 * write + timeline signal). */
export interface CpuReadbackAccessGuard<V> extends AsyncDisposable {
  readonly view: V;
}

/** Minimal subset of `GpuContextLimitedAccess` the cpu-readback runtime
 * needs. The full shape lives in `context.ts`; we type against a
 * structural subset here so tests can stub it without dragging the
 * FFI in. */
export interface CpuReadbackGpuLimitedAccess {
  resolveSurface(poolId: string): {
    readonly nativeHandlePtr: Deno.PointerObject | null;
    release(): void;
  };
  // deno-lint-ignore no-explicit-any
  readonly nativeLib: { readonly symbols: any };
}

let _SURFACE_ID_COUNTER = 0n;
function nextSurfaceId(): bigint {
  _SURFACE_ID_COUNTER += 1n;
  return _SURFACE_ID_COUNTER;
}

let _SHARED_INSTANCE: CpuReadbackContext | null = null;

function surfacePoolId(
  surface: StreamlibSurface | string | bigint | number,
): string {
  if (typeof surface === "string") return surface;
  if (typeof surface === "bigint") return surface.toString();
  if (typeof surface === "number") return String(Math.trunc(surface));
  const id = (surface as { id?: bigint | string | number }).id;
  if (id === undefined) {
    throw new TypeError(
      `CpuReadbackContext: expected StreamlibSurface or pool_id, got ${typeof surface}`,
    );
  }
  return String(id);
}

function surfaceFormatFrom(
  surface: StreamlibSurface | string | bigint | number,
): SurfaceFormat {
  if (typeof surface === "object" && surface !== null) {
    const fmt = (surface as { format?: number }).format;
    if (fmt !== undefined) return fmt as SurfaceFormat;
  }
  return 0 as SurfaceFormat; // BGRA8 default
}

/** Customer-facing context for the Deno subprocess SDK.
 *
 *     await using guard = await ctx.acquireWrite(surface);
 *     guard.view.plane(0).bytes.set(myImage);
 *     // [Symbol.asyncDispose] runs at scope exit:
 *     //   release the adapter guard so the host flushes CPU→GPU.
 */
export class CpuReadbackContext {
  private readonly gpu: CpuReadbackGpuLimitedAccess;
  private readonly escalate: EscalateChannel;
  // deno-lint-ignore no-explicit-any
  private readonly symbols: any;
  private readonly rt: Deno.PointerObject;
  private readonly triggerCallback: Deno.UnsafeCallback;
  private readonly surfaceIds = new Map<string, bigint>();
  private readonly resolvedHandles = new Map<
    string,
    ReturnType<CpuReadbackGpuLimitedAccess["resolveSurface"]>
  >();

  constructor(gpu: CpuReadbackGpuLimitedAccess, escalate: EscalateChannel) {
    this.gpu = gpu;
    this.escalate = escalate;
    this.symbols = gpu.nativeLib.symbols;
    const rtPtr = this.symbols.sldn_cpu_readback_runtime_new();
    if (rtPtr === null) {
      throw new Error(
        "CpuReadbackContext: sldn_cpu_readback_runtime_new returned NULL — " +
          "the subprocess could not bring up a Vulkan device. Check that " +
          "libvulkan.so.1 is installed and the driver supports " +
          "VK_KHR_external_memory_fd, VK_EXT_external_memory_dma_buf, and " +
          "VK_KHR_external_semaphore_fd.",
      );
    }
    this.rt = rtPtr;

    // The trigger callback receives synchronous calls FROM the cdylib
    // during `acquire_*`. Inside it we'd ordinarily dispatch the
    // escalate IPC, but Deno's escalate channel is async (Promises) and
    // `Deno.UnsafeCallback` cannot `await`. The bridge:
    //
    //   1. `_acquire` issues the async escalate IPC FIRST and stores
    //      the host's `timeline_value` in `_pendingTimelineValue`.
    //   2. `_acquire` then calls the synchronous cdylib `acquire_*`,
    //      which calls back into this callback.
    //   3. The callback reads the cell, returns the value as `u64`,
    //      and clears the cell.
    //
    // The cdylib's FFI returns the timeline value directly as `u64`
    // (rather than via an out-pointer) so this callback never needs
    // to write through a foreign pointer — `Deno.UnsafePointerView`
    // is read-only. 0 is the host's unused-by-construction sentinel
    // for "trigger failed" (the host adapter starts every timeline at
    // 0 and only ever signals values >= 1).
    //
    // Single-flight escalate channel + single-threaded JS event loop
    // guarantees no interleaving between priming and the FFI call.
    this.triggerCallback = new Deno.UnsafeCallback(
      {
        parameters: ["pointer", "u64", "u32"] as const,
        result: "u64" as const,
      },
      (
        _userData: Deno.PointerValue,
        _surfaceId: bigint,
        _direction: number,
      ): bigint => {
        if (_pendingTimelineValue === null) {
          return 0n;
        }
        const value = _pendingTimelineValue;
        _pendingTimelineValue = null;
        return value;
      },
    );
    const rc: number = this.symbols.sldn_cpu_readback_set_trigger_callback(
      this.rt,
      this.triggerCallback.pointer,
      null,
    );
    if (rc !== 0) {
      this.triggerCallback.close();
      throw new Error(
        `CpuReadbackContext: set_trigger_callback returned ${rc}`,
      );
    }
  }

  static fromRuntime(
    ctx: { readonly gpuLimitedAccess: CpuReadbackGpuLimitedAccess },
  ): CpuReadbackContext {
    if (_SHARED_INSTANCE === null) {
      _SHARED_INSTANCE = new CpuReadbackContext(
        ctx.gpuLimitedAccess,
        getChannel(),
      );
    }
    return _SHARED_INSTANCE;
  }

  /** Close the cdylib runtime and the trigger callback. After
   * `close()` the context is unusable; this is mostly for tests. */
  close(): void {
    this.symbols.sldn_cpu_readback_runtime_free(this.rt);
    this.triggerCallback.close();
    _SHARED_INSTANCE = null;
  }

  private resolveAndRegister(poolId: string, format: SurfaceFormat): bigint {
    const cached = this.surfaceIds.get(poolId);
    if (cached !== undefined) return cached;
    const handle = this.gpu.resolveSurface(poolId);
    const handlePtr = handle.nativeHandlePtr;
    if (handlePtr === null) {
      throw new Error(
        `CpuReadbackContext: resolveSurface('${poolId}') returned a handle with a null native pointer`,
      );
    }
    const surfaceId = nextSurfaceId();
    const rc: number = this.symbols.sldn_cpu_readback_register_surface(
      this.rt,
      surfaceId,
      handlePtr,
      Number(format),
    );
    if (rc !== 0) {
      throw new Error(
        `CpuReadbackContext: register_surface failed for pool_id ` +
          `'${poolId}' (rc=${rc}). Typically a missing sync_fd or a ` +
          `plane-count mismatch with format ${format}.`,
      );
    }
    this.surfaceIds.set(poolId, surfaceId);
    this.resolvedHandles.set(poolId, handle);
    return surfaceId;
  }

  async acquireRead(
    surface: StreamlibSurface | string | bigint | number,
  ): Promise<CpuReadbackAccessGuard<CpuReadbackReadView>> {
    return await this._acquire(surface, false, true) as CpuReadbackAccessGuard<
      CpuReadbackReadView
    >;
  }

  async acquireWrite(
    surface: StreamlibSurface | string | bigint | number,
  ): Promise<CpuReadbackAccessGuard<CpuReadbackWriteView>> {
    return await this._acquire(surface, true, true) as CpuReadbackAccessGuard<
      CpuReadbackWriteView
    >;
  }

  async tryAcquireRead(
    surface: StreamlibSurface | string | bigint | number,
  ): Promise<CpuReadbackAccessGuard<CpuReadbackReadView> | null> {
    return await this._acquire(surface, false, false) as
      | CpuReadbackAccessGuard<CpuReadbackReadView>
      | null;
  }

  async tryAcquireWrite(
    surface: StreamlibSurface | string | bigint | number,
  ): Promise<CpuReadbackAccessGuard<CpuReadbackWriteView> | null> {
    return await this._acquire(surface, true, false) as
      | CpuReadbackAccessGuard<CpuReadbackWriteView>
      | null;
  }

  private async _acquire(
    surface: StreamlibSurface | string | bigint | number,
    write: boolean,
    blocking: boolean,
  ): Promise<CpuReadbackAccessGuard<CpuReadbackReadView | CpuReadbackWriteView> | null> {
    const poolId = surfacePoolId(surface);
    const format = surfaceFormatFrom(surface);
    const surfaceId = this.resolveAndRegister(poolId, format);

    // Prime the trigger cell with the host's timeline value BEFORE we
    // call into the cdylib's synchronous acquire. The cdylib calls
    // back into the JS trigger to fetch the value during the same
    // call; the trigger reads from `_pendingTimelineValue` and zeros it.
    //
    // Both read and write acquires drive an `image_to_buffer` copy
    // first (write mode flushes back via `buffer_to_image` on dispose).
    // Single-flight escalate channel + single-threaded event loop
    // means no interleaving between `prime` and the FFI call.
    const response = await this.escalate.runCpuReadbackCopy(
      surfaceId,
      "image_to_buffer",
    );
    _pendingTimelineValue = BigInt(response.timeline_value);

    const buf = new Uint8Array(VIEW_STRUCT_SIZE);
    const fn = blocking
      ? (write
        ? this.symbols.sldn_cpu_readback_acquire_write
        : this.symbols.sldn_cpu_readback_acquire_read)
      : (write
        ? this.symbols.sldn_cpu_readback_try_acquire_write
        : this.symbols.sldn_cpu_readback_try_acquire_read);
    const rc: number = fn(this.rt, surfaceId, Deno.UnsafePointer.of(buf));
    // Defensive cleanup — the trigger should have consumed the value,
    // but if the cdylib short-circuited (e.g. surface not registered)
    // we don't want a stale value to bleed into the next acquire.
    _pendingTimelineValue = null;
    if (rc === RC_CONTENDED) return null;
    if (rc !== RC_OK) {
      throw new Error(
        `CpuReadbackContext.${blocking ? "" : "try_"}acquire_${
          write ? "write" : "read"
        }: rc=${rc} for surface '${poolId}'`,
      );
    }

    const view = this.parseView(buf, write);
    const surfaceIdSnapshot = surfaceId;
    const symbols = this.symbols;
    const rt = this.rt;
    const escalate = this.escalate;
    const writeMode = write;

    return {
      view,
      [Symbol.asyncDispose]: async () => {
        if (writeMode) {
          // On write release the cdylib's `end_write_access` calls back
          // into the trigger to schedule the buffer→image flush. Same
          // pattern as acquire — prime the cell, then call.
          try {
            const flushResponse = await escalate.runCpuReadbackCopy(
              surfaceIdSnapshot,
              "buffer_to_image",
            );
            _pendingTimelineValue = BigInt(flushResponse.timeline_value);
          } catch (e) {
            // Surface flush failure through the polyglot unified
            // logging pathway; the cdylib logs its own context too.
            log.error(
              "cpu-readback write flush trigger IPC failed",
              {
                surfaceId: surfaceIdSnapshot.toString(),
                error: String(e),
              },
            );
            _pendingTimelineValue = 0n;
          }
          symbols.sldn_cpu_readback_release_write(rt, surfaceIdSnapshot);
        } else {
          symbols.sldn_cpu_readback_release_read(rt, surfaceIdSnapshot);
        }
        _pendingTimelineValue = null;
      },
    };
  }

  private parseView(
    buf: Uint8Array,
    writable: boolean,
  ): CpuReadbackReadView | CpuReadbackWriteView {
    const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    const width = dv.getUint32(0, true);
    const height = dv.getUint32(4, true);
    const format = dv.getUint32(8, true) as SurfaceFormat;
    const planeCount = dv.getUint32(12, true);
    if (planeCount === 0) {
      throw new Error("cpu-readback acquire returned zero planes");
    }
    const planes: (CpuReadbackPlaneView | CpuReadbackPlaneViewMut)[] = [];
    for (let idx = 0; idx < planeCount && idx < MAX_PLANES; idx += 1) {
      const offset = 16 + idx * PLANE_STRIDE;
      // Plane struct field offsets:
      //   ptr        : 0  (8 bytes)
      //   width      : 8  (4)
      //   height     : 12 (4)
      //   bytes_per_pixel: 16 (4)
      //   pad        : 20 (4)
      //   byte_size  : 24 (8)
      const mappedPtrLow = dv.getUint32(offset, true);
      const mappedPtrHigh = dv.getUint32(offset + 4, true);
      const mappedPtrAddr = (BigInt(mappedPtrHigh) << 32n) |
        BigInt(mappedPtrLow);
      const planeWidth = dv.getUint32(offset + 8, true);
      const planeHeight = dv.getUint32(offset + 12, true);
      const planeBpp = dv.getUint32(offset + 16, true);
      const planeBytes = dv.getBigUint64(offset + 24, true);
      if (mappedPtrAddr === 0n) {
        throw new Error(
          `cpu-readback plane ${idx} has null mapped_ptr — host staging registration may be stale`,
        );
      }
      const planePtr = Deno.UnsafePointer.create(mappedPtrAddr);
      if (planePtr === null) {
        throw new Error(
          `cpu-readback plane ${idx} mapped_ptr could not be wrapped`,
        );
      }
      const planeView = new Deno.UnsafePointerView(planePtr);
      const arrayBuffer = planeView.getArrayBuffer(Number(planeBytes));
      const bytes = new Uint8Array(arrayBuffer);
      planes.push({
        width: planeWidth,
        height: planeHeight,
        bytesPerPixel: planeBpp,
        rowStride: planeWidth * planeBpp,
        bytes,
      });
    }
    const view = {
      width,
      height,
      format,
      planeCount: planes.length,
      planes,
      plane(index: number): CpuReadbackPlaneView | CpuReadbackPlaneViewMut {
        if (index < 0 || index >= planes.length) {
          throw new RangeError(
            `cpu-readback plane index ${index} out of range (0..${planes.length})`,
          );
        }
        return planes[index];
      },
    };
    return writable
      ? (view as unknown as CpuReadbackWriteView)
      : (view as unknown as CpuReadbackReadView);
  }
}

// Module-scoped cell used to thread the host's timeline value from the
// async escalate IPC response into the synchronous trigger callback
// the cdylib calls during `acquire_*`. Single-flight escalate + single-
// threaded event loop means no interleaving — between `_acquire`
// priming the cell and the cdylib calling the trigger, no other
// JS code runs.
let _pendingTimelineValue: bigint | null = null;


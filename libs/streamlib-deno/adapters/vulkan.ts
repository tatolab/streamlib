// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Vulkan-native surface adapter — Deno customer-facing API.
 *
 * Mirrors the Rust crate `streamlib-adapter-vulkan` (#511, #531). The
 * subprocess's actual Vulkan handling delegates to
 * `streamlib-deno-native`'s `sldn_vulkan_*` FFI surface, which itself
 * wraps the host adapter crate's `VulkanSurfaceAdapter` against a
 * subprocess-local `VulkanDevice`. There is **no** parallel Vulkan
 * implementation per language — every line of layout-transition,
 * timeline-wait, and queue-mutex coordination lives in
 * `streamlib-adapter-vulkan` and runs in the subprocess process.
 *
 * This module provides:
 *
 *  - `VulkanReadView` / `VulkanWriteView` — typed views the subprocess
 *    sees inside `acquireRead` / `acquireWrite` scopes; expose
 *    `vkImage` (a `bigint` Vulkan handle) plus the current
 *    `vkImageLayout`.
 *  - The `VulkanContext` class — built via `VulkanContext.fromRuntime()`
 *    inside a polyglot processor's `setup` hook. Customers acquire
 *    scoped read/write access via TC39 `using` blocks and dispatch
 *    their own raw vulkanalia / Deno-FFI work against `view.vkImage`.
 *  - `RawVulkanHandles` + `rawHandles()` shape — escape hatch for
 *    customers driving Vulkan directly.
 */

import { getChannel as getEscalateChannel } from "../escalate.ts";
import {
  STREAMLIB_ADAPTER_ABI_VERSION,
  type StreamlibSurface,
} from "../surface_adapter.ts";

export { STREAMLIB_ADAPTER_ABI_VERSION };

/** Mirror of `vk::ImageLayout` enumerant values used in views. */
export const VkImageLayout = {
  Undefined: 0,
  General: 1,
  ColorAttachmentOptimal: 2,
  ShaderReadOnlyOptimal: 5,
  TransferSrcOptimal: 6,
  TransferDstOptimal: 7,
} as const;
export type VkImageLayout = (typeof VkImageLayout)[keyof typeof VkImageLayout];

/** Read-side view inside an `acquireRead` scope. */
export interface VulkanReadView {
  readonly vkImage: bigint;
  readonly vkImageLayout: VkImageLayout;
}

/** Write-side view inside an `acquireWrite` scope. */
export interface VulkanWriteView {
  readonly vkImage: bigint;
  readonly vkImageLayout: VkImageLayout;
}

/**
 * Power-user escape hatch — raw Vulkan handles as `bigint`s the
 * customer feeds into their preferred Vulkan binding (e.g. through
 * `Deno.UnsafePointer.create`). Valid for the lifetime of the
 * runtime; using them after shutdown is undefined.
 */
export interface RawVulkanHandles {
  readonly vkInstance: bigint;
  readonly vkPhysicalDevice: bigint;
  readonly vkDevice: bigint;
  readonly vkQueue: bigint;
  readonly vkQueueFamilyIndex: number;
  readonly apiVersion: number;
}

/** Public Vulkan adapter contract. */
export interface VulkanSurfaceAdapter {
  acquireRead(surface: StreamlibSurface): SurfaceAccessGuard<VulkanReadView>;
  acquireWrite(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<VulkanWriteView>;
  tryAcquireRead(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<VulkanReadView> | null;
  tryAcquireWrite(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<VulkanWriteView> | null;
  rawHandles(): RawVulkanHandles;
}

// =============================================================================
// Concrete VulkanContext implementation (#531)
// =============================================================================
//
// Mirrors `streamlib-deno/adapters/opengl.ts::OpenGLContext` exactly:
// cached singleton per subprocess, surface-share `pool_id` → local
// `surface_id` mapping, FFI calls into `sldn_vulkan_*` symbols loaded by
// the runner.

/** Async-disposable guard returned by acquire ops. `using` (synchronous
 * disposable) suffices because the per-acquire path is fully synchronous
 * — the host adapter blocks on the timeline wait before returning. */
export interface VulkanAccessGuard<V> extends Disposable {
  readonly view: V;
}

/** Minimal subset of `GpuContextLimitedAccess` the Vulkan adapter
 * runtime needs. The full shape lives in `context.ts`; we type against
 * a structural subset here so tests can stub it without dragging the
 * whole FFI surface. */
export interface VulkanGpuLimitedAccess {
  resolveSurface(poolId: string): {
    readonly nativeHandlePtr: Deno.PointerObject | null;
    release(): void;
  };
  /** Publish a producer-side post-release `VkImageLayout` for `poolId`
   * via the surface-share `update_layout` op (#633). Called by
   * `VulkanContext.releaseForCrossProcess` after the QFOT release
   * barrier records. */
  updateImageLayout(poolId: string, layout: number): void;
  // deno-lint-ignore no-explicit-any
  readonly nativeLib: { readonly symbols: any };
}

let _SURFACE_ID_COUNTER = 0n;
function nextSurfaceId(): bigint {
  _SURFACE_ID_COUNTER += 1n;
  return _SURFACE_ID_COUNTER;
}

let _SHARED_INSTANCE: VulkanContext | null = null;

/** Subprocess-side Vulkan adapter runtime (#531, Linux).
 *
 * Brings up `streamlib_consumer_rhi::ConsumerVulkanDevice` +
 * `streamlib_adapter_vulkan::VulkanSurfaceAdapter` inside this
 * subprocess and exposes scoped acquire/release that hands customers a
 * real `VkImage` handle plus the layout the adapter transitioned to.
 * The acquire/release calls reuse every line of host-RHI logic
 * (timeline wait, layout transition, queue-mutex coordination,
 * contention checking) — the Deno side is a thin FFI shim.
 *
 * Construct via `VulkanContext.fromRuntime(ctx)` — single instance per
 * subprocess. Repeat calls return the cached instance.
 *
 * Customers dispatch their own Vulkan work using their preferred Deno
 * Vulkan binding (raw `Deno.dlopen` against `libvulkan.so.1`,
 * `@webgpu/types`-flavored helpers, etc.). The cdylib's runtime exposes
 * its raw handles through `rawHandles()` so the customer's submissions
 * interleave correctly with the adapter's layout transitions on the
 * same `VkQueue`.
 */
export class VulkanContext {
  private readonly gpu: VulkanGpuLimitedAccess;
  // deno-lint-ignore no-explicit-any
  private readonly symbols: any;
  private readonly rt: Deno.PointerObject;
  private readonly surfaceIds = new Map<string, bigint>();
  /** Identity-keyed kernel-id cache: keying by the `Uint8Array` itself
   * keeps the hot path O(1) (no per-call hashing), so multi-MB ML
   * SPIR-V doesn't pay a SHA-256 cost on every dispatch. `WeakMap`
   * auto-clears entries when the customer drops their reference to the
   * bytes — no unbounded growth and no manual eviction. Customers who
   * dispatch repeatedly should reuse the same `Uint8Array` (stash it
   * on the processor at setup); a fresh instance is a cache miss and
   * re-registers through escalate IPC (host-side cache hit, but the
   * IPC payload is sent again).
   */
  private readonly computeKernelIds = new WeakMap<Uint8Array, string>();
  private readonly resolvedHandles = new Map<
    string,
    ReturnType<VulkanGpuLimitedAccess["resolveSurface"]>
  >();

  constructor(gpu: VulkanGpuLimitedAccess) {
    this.gpu = gpu;
    this.symbols = gpu.nativeLib.symbols;
    const rtPtr = this.symbols.sldn_vulkan_runtime_new();
    if (rtPtr === null) {
      throw new Error(
        "VulkanContext: sldn_vulkan_runtime_new returned NULL — the " +
          "subprocess could not bring up a Vulkan device. Check that " +
          "libvulkan.so.1 is installed and the driver supports " +
          "VK_KHR_external_memory_fd, VK_EXT_external_memory_dma_buf, " +
          "VK_EXT_image_drm_format_modifier, and " +
          "VK_KHR_external_semaphore_fd.",
      );
    }
    this.rt = rtPtr;
  }

  /** Build (or fetch the cached) `VulkanContext` for this subprocess. */
  static fromRuntime(
    ctx: { readonly gpuLimitedAccess: VulkanGpuLimitedAccess },
  ): VulkanContext {
    if (_SHARED_INSTANCE === null) {
      _SHARED_INSTANCE = new VulkanContext(ctx.gpuLimitedAccess);
    }
    return _SHARED_INSTANCE;
  }

  private resolveAndRegister(poolId: string): bigint {
    const cached = this.surfaceIds.get(poolId);
    if (cached !== undefined) return cached;
    const handle = this.gpu.resolveSurface(poolId);
    const handlePtr = handle.nativeHandlePtr;
    if (handlePtr === null) {
      throw new Error(
        `VulkanContext: resolveSurface('${poolId}') returned a handle with a null native pointer`,
      );
    }
    const surfaceId = nextSurfaceId();
    const rc: number = this.symbols.sldn_vulkan_register_surface(
      this.rt,
      surfaceId,
      handlePtr,
    );
    if (rc !== 0) {
      throw new Error(
        `VulkanContext: register_surface failed for pool_id ` +
          `'${poolId}' (rc=${rc}). Check the subprocess log for ` +
          `import errors — typically a missing sync_fd, an unsupported ` +
          `DRM modifier, or an unsupported pixel format.`,
      );
    }
    this.surfaceIds.set(poolId, surfaceId);
    this.resolvedHandles.set(poolId, handle);
    return surfaceId;
  }

  private static surfacePoolId(
    surface: StreamlibSurface | string | bigint,
  ): string {
    if (typeof surface === "string") return surface;
    if (typeof surface === "bigint") return surface.toString();
    const id = (surface as { id?: bigint | string | number }).id;
    if (id === undefined) {
      throw new TypeError(
        `VulkanContext: expected StreamlibSurface, string pool_id, or bigint — got ${
          typeof surface
        }`,
      );
    }
    return String(id);
  }

  /** Read a `VulkanView`-shaped struct out of a Deno FFI buffer the
   * cdylib populated. The struct layout is `{u64 vk_image, i32 vk_image_layout}`
   * — 16 bytes total with 4-byte tail padding. */
  private static readView(buf: Uint8Array): {
    vkImage: bigint;
    vkImageLayout: number;
  } {
    const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    return {
      vkImage: dv.getBigUint64(0, true),
      vkImageLayout: dv.getInt32(8, true),
    };
  }

  /** Acquire write access. Returns a `using`-disposable guard whose
   * `view.vkImage` is a `VkImage` valid against the cdylib's
   * `VkDevice`, transitioned to `GENERAL`. On dispose the adapter
   * advances the host's timeline so the next consumer can wake up;
   * the customer must `vkQueueWaitIdle` (or chain a binary semaphore
   * on their submission) BEFORE leaving the scope so writes are
   * visible. */
  acquireWrite(
    surface: StreamlibSurface | string | bigint,
  ): VulkanAccessGuard<VulkanWriteView> {
    const poolId = VulkanContext.surfacePoolId(surface);
    const surfaceId = this.resolveAndRegister(poolId);
    const buf = new Uint8Array(16);
    const rc = this.symbols.sldn_vulkan_acquire_write(
      this.rt,
      surfaceId,
      Deno.UnsafePointer.of(buf),
    );
    if (rc !== 0) {
      throw new Error(
        `VulkanContext.acquireWrite: sldn_vulkan_acquire_write returned ${rc} for surface '${poolId}'`,
      );
    }
    const v = VulkanContext.readView(buf);
    const symbols = this.symbols;
    const rt = this.rt;
    return {
      view: {
        vkImage: v.vkImage,
        vkImageLayout: v.vkImageLayout as VkImageLayout,
      },
      [Symbol.dispose]() {
        symbols.sldn_vulkan_release_write(rt, surfaceId);
      },
    };
  }

  /** Acquire read access. Same shape as `acquireWrite`, but the image
   * is in `SHADER_READ_ONLY_OPTIMAL` (multiple readers may coexist; no
   * writer can be active). */
  acquireRead(
    surface: StreamlibSurface | string | bigint,
  ): VulkanAccessGuard<VulkanReadView> {
    const poolId = VulkanContext.surfacePoolId(surface);
    const surfaceId = this.resolveAndRegister(poolId);
    const buf = new Uint8Array(16);
    const rc = this.symbols.sldn_vulkan_acquire_read(
      this.rt,
      surfaceId,
      Deno.UnsafePointer.of(buf),
    );
    if (rc !== 0) {
      throw new Error(
        `VulkanContext.acquireRead: sldn_vulkan_acquire_read returned ${rc} for surface '${poolId}'`,
      );
    }
    const v = VulkanContext.readView(buf);
    const symbols = this.symbols;
    const rt = this.rt;
    return {
      view: {
        vkImage: v.vkImage,
        vkImageLayout: v.vkImageLayout as VkImageLayout,
      },
      [Symbol.dispose]() {
        symbols.sldn_vulkan_release_read(rt, surfaceId);
      },
    };
  }

  /** Issue a producer-side queue-family-ownership-transfer (QFOT)
   * release barrier on this subprocess's `ConsumerVulkanDevice` and
   * publish the post-release `VkImageLayout` to surface-share so the
   * next cross-process consumer's `acquire_from_foreign` sees the
   * right source layout (#633).
   *
   * Call this *after* the matching `acquireWrite`/`acquireRead`
   * `using` block has exited and after the producer's queue
   * submission has actually retired (e.g. the customer has signalled
   * their own timeline or `vkQueueWaitIdle`-ed). The adapter's QFOT
   * release barrier carries `srcAccessMask = MEMORY_WRITE_BIT` and
   * assumes producer-side hazard coverage upstream.
   *
   * Also serves the **dual-registration** path used by non-Vulkan
   * adapters that need cross-process release wiring (OpenGL via
   * `OpenGLContext.releaseForCrossProcess`, and Skia GL by
   * extension). In that mode the surface may not have been touched
   * by an explicit `acquire*` on this Vulkan context — the release
   * barrier still issues correctly because the surface-share
   * registration carries the producer's post-write layout as the
   * Vulkan adapter's initial layout.
   *
   * `postReleaseLayout` is a Vulkan `VkImageLayout` enumerant as a
   * number (use `VkImageLayout` constants). Picking `GENERAL` is the
   * safest default for cross-process handoffs — the consumer's
   * `acquire_from_foreign` re-transitions to whatever layout it
   * actually needs.
   *
   * On NVIDIA Linux drivers without
   * `VK_EXT_external_memory_acquire_unmodified` (current state as of
   * 2026-05-03), the host consumer side falls back to a bridging
   * `UNDEFINED → target` transition; content preservation is
   * empirical (see `docs/learnings/cross-process-vkimage-layout.md`).
   * The producer-side path here is correct under both modes. */
  releaseForCrossProcess(
    surface: StreamlibSurface | string | bigint,
    postReleaseLayout: number,
  ): void {
    const poolId = VulkanContext.surfacePoolId(surface);
    // Lazily resolve+register so dual-registration callers
    // (OpenGL via OpenGLContext.releaseForCrossProcess, Skia GL by
    // extension) don't have to issue a no-op acquire first.
    // Idempotent — repeat calls return the cached id.
    const surfaceId = this.resolveAndRegister(poolId);
    const rc: number = this.symbols.sldn_vulkan_release_to_foreign(
      this.rt,
      surfaceId,
      postReleaseLayout,
    );
    if (rc !== 0) {
      throw new Error(
        `VulkanContext.releaseForCrossProcess: ` +
          `sldn_vulkan_release_to_foreign returned ${rc} for surface ` +
          `'${poolId}' (check subprocess log for the underlying adapter error)`,
      );
    }
    // Pair the QFOT release with the surface-share `update_layout`
    // publish so the next host-side consumer's `acquire_from_foreign`
    // picks up the new layout instead of the cached registration one.
    this.gpu.updateImageLayout(poolId, postReleaseLayout);
  }

  /** Dispatch a compute shader against the surface's host-side `VkImage`
   * via escalate IPC. The surface MUST currently be held in WRITE
   * mode (call inside an `acquireWrite` `using` block).
   *
   * The shader's `binding=0` is bound as a storage image; push
   * constants are forwarded byte-for-byte.
   *
   * Compute is synchronous host-side: when this resolves, the GPU
   * work has retired and the host's writes are visible. The
   * `VulkanComputeKernel` is built once on the host (SPIR-V
   * reflection, on-disk pipeline cache via
   * `$STREAMLIB_PIPELINE_CACHE_DIR` /
   * `$XDG_CACHE_HOME/streamlib/pipeline-cache`) and re-used across
   * dispatches with the same SPIR-V. */
  async dispatchCompute(
    surface: StreamlibSurface | string | bigint,
    spirv: Uint8Array,
    pushConstants: Uint8Array,
    groupCountX: number,
    groupCountY: number,
    groupCountZ: number,
  ): Promise<void> {
    const poolId = VulkanContext.surfacePoolId(surface);
    const cached = this.surfaceIds.get(poolId);
    if (cached === undefined) {
      throw new Error(
        `VulkanContext.dispatchCompute: surface '${poolId}' is not registered ` +
          "— call acquireWrite inside a `using` block first.",
      );
    }
    const ch = getEscalateChannel();
    // Identity-keyed kernel-id cache: `WeakMap` lookup is O(1) per
    // dispatch, so multi-MB ML SPIR-V doesn't pay a hashing cost on
    // the hot path. Entries auto-clear when the customer drops the
    // Uint8Array — see the field comment.
    let kernelId = this.computeKernelIds.get(spirv);
    if (kernelId === undefined) {
      const response = await ch.registerComputeKernel(
        spirv,
        pushConstants.byteLength,
      );
      kernelId = response.handle_id;
      this.computeKernelIds.set(spirv, kernelId);
    }
    // Send the surface-share UUID, not the cdylib's local u64
    // surfaceId — the host bridge resolves UUID → host
    // `StreamTexture` via an application-provided map.
    void cached;
    await ch.runComputeKernel(
      kernelId,
      poolId,
      pushConstants,
      groupCountX,
      groupCountY,
      groupCountZ,
    );
  }

  /** Return the cdylib runtime's raw Vulkan handles — same shape as
   * `streamlib_adapter_vulkan::raw_handles()`. Use these to drive your
   * preferred Vulkan binding against the SAME `VkDevice` the adapter
   * manages.
   *
   * Struct layout: `{u64 vk_instance, u64 vk_physical_device, u64
   * vk_device, u64 vk_queue, u32 vk_queue_family_index, u32
   * api_version}` — 40 bytes total. */
  rawHandles(): RawVulkanHandles {
    const buf = new Uint8Array(40);
    const rc = this.symbols.sldn_vulkan_raw_handles(
      this.rt,
      Deno.UnsafePointer.of(buf),
    );
    if (rc !== 0) {
      throw new Error(`VulkanContext.rawHandles: sldn_vulkan_raw_handles returned ${rc}`);
    }
    const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    return {
      vkInstance: dv.getBigUint64(0, true),
      vkPhysicalDevice: dv.getBigUint64(8, true),
      vkDevice: dv.getBigUint64(16, true),
      vkQueue: dv.getBigUint64(24, true),
      vkQueueFamilyIndex: dv.getUint32(32, true),
      apiVersion: dv.getUint32(36, true),
    };
  }
}

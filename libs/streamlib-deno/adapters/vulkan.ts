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

async function sha256Hex(bytes: Uint8Array): Promise<string> {
  // Copy into a fresh ArrayBuffer-backed view so `crypto.subtle.digest`
  // accepts it regardless of the source's underlying buffer flavor.
  const fresh = new Uint8Array(bytes.byteLength);
  fresh.set(bytes);
  const digest = await crypto.subtle.digest("SHA-256", fresh);
  const view = new Uint8Array(digest);
  let s = "";
  for (let i = 0; i < view.length; i++) {
    s += view[i].toString(16).padStart(2, "0");
  }
  return s;
}

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
  /** SHA-256(spv) hex → host-assigned kernel_id. Re-registering
   * identical SPIR-V is a host-side cache hit and returns the same
   * id; this map keeps us from re-issuing the register IPC for blobs
   * we've already shown the host once.
   */
  private readonly computeKernelIds = new Map<string, string>();
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
    // Cache kernel registrations by SHA-256(spv) hex (the same key
    // the host uses), so identical SPIR-V is registered once per
    // subprocess. Re-registration of the same blob is a host-side
    // cache hit too.
    const spvKey = await sha256Hex(spirv);
    let kernelId = this.computeKernelIds.get(spvKey);
    if (kernelId === undefined) {
      const response = await ch.registerComputeKernel(
        spirv,
        pushConstants.byteLength,
      );
      kernelId = response.handle_id;
      this.computeKernelIds.set(spvKey, kernelId);
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

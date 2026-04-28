// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Polyglot Vulkan compute processor — Deno twin of
 * `examples/polyglot-vulkan-compute/python/vulkan_compute.py`.
 *
 * End-to-end gate for the subprocess `VulkanContext` runtime (#531).
 * The host pre-allocates a render-target-capable DMA-BUF surface AND
 * an exportable `VulkanTimelineSemaphore`, registers both with
 * surface-share. This processor receives a trigger Videoframe, opens
 * the host surface through `VulkanContext.acquireWrite` (which imports
 * the DMA-BUF as a `VkImage` in the subprocess and imports the
 * timeline via `from_imported_opaque_fd`), dispatches the Mandelbrot
 * compute kernel via the cdylib's quarantined
 * `sldn_vulkan_dispatch_compute` helper, and releases — the host
 * adapter advances the timeline so the host's pre-stop readback sees
 * the writes.
 *
 * Same compute shader as the Python twin, with `variant=1` so the
 * cosine palette differs slightly — visually distinct PNGs make
 * reviewer comparisons easy.
 *
 * Config keys: vulkan_surface_uuid, width, height, max_iter, variant,
 * shader_spv_hex (hex-encoded SPIR-V from
 * `examples/polyglot-vulkan-compute/shaders/mandelbrot.comp`).
 */

import type {
  ReactiveProcessor,
  RuntimeContextFullAccess,
  RuntimeContextLimitedAccess,
} from "../../../libs/streamlib-deno/mod.ts";
import { VulkanContext } from "../../../libs/streamlib-deno/adapters/vulkan.ts";

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(hex.substr(i * 2, 2), 16);
  }
  return out;
}

export default class VulkanComputeProcessor implements ReactiveProcessor {
  private uuid = "";
  private width = 0;
  private height = 0;
  private maxIter = 0;
  private variant = 1; // Default to Deno palette (#531).
  private spv: Uint8Array | null = null;
  private vk: VulkanContext | null = null;
  private dispatched = false;
  private errorMessage: string | null = null;

  setup(ctx: RuntimeContextFullAccess): void {
    const cfg = ctx.config;
    this.uuid = String(cfg["vulkan_surface_uuid"]);
    this.width = Number(cfg["width"] ?? 0);
    this.height = Number(cfg["height"] ?? 0);
    this.maxIter = Number(cfg["max_iter"] ?? 256);
    this.variant = Number(cfg["variant"] ?? 1);
    this.spv = hexToBytes(String(cfg["shader_spv_hex"] ?? ""));
    this.vk = VulkanContext.fromRuntime(ctx);
    console.error(
      `[VulkanCompute/deno] setup uuid=${this.uuid} ` +
        `size=${this.width}x${this.height} ` +
        `spv_bytes=${this.spv.byteLength} variant=${this.variant}`,
    );
  }

  process(ctx: RuntimeContextLimitedAccess): void {
    const result = ctx.inputs.read("video_in");
    if (!result) return;
    if (this.dispatched) return;
    try {
      this.dispatchOnce();
      this.dispatched = true;
      console.error(
        `[VulkanCompute/deno] Mandelbrot dispatched into surface '${this.uuid}'`,
      );
    } catch (e) {
      this.errorMessage = e instanceof Error ? e.message : String(e);
      console.error(
        `[VulkanCompute/deno] dispatch failed: ${this.errorMessage}`,
      );
    }
  }

  private dispatchOnce(): void {
    if (this.vk === null || this.spv === null) {
      throw new Error("VulkanContext / SPIR-V not initialized in setup");
    }
    // Push-constant layout matches the shader: `{u32 width, u32 height, u32 max_iter, u32 variant}`.
    const pc = new Uint8Array(16);
    const pcView = new DataView(pc.buffer);
    pcView.setUint32(0, this.width, true);
    pcView.setUint32(4, this.height, true);
    pcView.setUint32(8, this.maxIter, true);
    pcView.setUint32(12, this.variant, true);

    // Shader's `local_size_x = local_size_y = 16`.
    const local = 16;
    const groupX = Math.ceil(this.width / local);
    const groupY = Math.ceil(this.height / local);

    using guard = this.vk.acquireWrite(this.uuid);
    console.error(
      `[VulkanCompute/deno] acquired vk_image=0x${guard.view.vkImage.toString(16)} ` +
        `layout=${guard.view.vkImageLayout}`,
    );
    this.vk.dispatchCompute(
      this.uuid,
      this.spv,
      pc,
      groupX,
      groupY,
      1,
    );
  }

  teardown(_ctx: RuntimeContextFullAccess): void {
    console.error(
      `[VulkanCompute/deno] teardown dispatched=${this.dispatched} ` +
        `error=${this.errorMessage}`,
    );
  }
}

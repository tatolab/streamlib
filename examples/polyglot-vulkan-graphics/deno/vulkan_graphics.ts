// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Polyglot Vulkan graphics processor — Deno twin of
 * `examples/polyglot-vulkan-graphics/python/vulkan_graphics.py`.
 *
 * End-to-end gate for the subprocess `VulkanContext.dispatchGraphics`
 * runtime (#656). The host pre-allocates a render-target-capable
 * DMA-BUF surface AND an exportable `VulkanTimelineSemaphore`,
 * registers both with surface-share, and installs a
 * `GraphicsKernelBridge` wired to its `VulkanGraphicsKernel`. This
 * processor receives a trigger Videoframe, opens the host surface
 * through `VulkanContext.acquireWrite`, and calls
 * `VulkanContext.dispatchGraphics` — which routes through escalate
 * IPC's `register_graphics_kernel` + `run_graphics_draw` ops to the
 * host's `VulkanGraphicsKernel.offscreen_render`.
 *
 * Same vertex+fragment shaders as the Python twin, with
 * `variant = 1` so the triangle is drawn in cyan/magenta/yellow
 * instead of red/green/blue — visually distinct PNGs make reviewer
 * comparisons easy.
 *
 * Config keys: vulkan_surface_uuid, width, height, variant,
 * vertex_spv_hex, fragment_spv_hex.
 */

import type {
  ReactiveProcessor,
  RuntimeContextFullAccess,
  RuntimeContextLimitedAccess,
} from "../../../libs/streamlib-deno/mod.ts";
import {
  VkImageLayout,
  VulkanContext,
} from "../../../libs/streamlib-deno/adapters/vulkan.ts";
import type { EscalateRequestRegisterGraphicsKernelPipelineState } from "../../../libs/streamlib-deno/escalate.ts";

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(hex.substr(i * 2, 2), 16);
  }
  return out;
}

function trianglePipelineState():
  EscalateRequestRegisterGraphicsKernelPipelineState {
  // jtd-codegen produces string-valued enums (`Topology.TriangleList =
  // "triangle_list"`), and the wire JSON shape is the string. Build
  // the literal then cast — the runtime serializer accepts any field
  // whose value matches the expected string discriminator.
  // deno-lint-ignore no-explicit-any
  return {
    topology: "triangle_list",
    vertex_input_bindings: [],
    vertex_input_attributes: [],
    rasterization_polygon_mode: "fill",
    rasterization_cull_mode: "none",
    rasterization_front_face: "counter_clockwise",
    rasterization_line_width: 1.0,
    multisample_samples: 1,
    depth_stencil_enabled: false,
    depth_compare_op: "always",
    depth_write: false,
    color_blend_enabled: false,
    color_write_mask: 0b1111,
    color_blend_src_color_factor: "one",
    color_blend_dst_color_factor: "zero",
    color_blend_color_op: "add",
    color_blend_src_alpha_factor: "one",
    color_blend_dst_alpha_factor: "zero",
    color_blend_alpha_op: "add",
    attachment_color_formats: ["rgba8_unorm"],
    dynamic_state: "viewport_scissor",
  } as unknown as EscalateRequestRegisterGraphicsKernelPipelineState;
}

// Vertex stage visibility for push constants — bit 0 in
// `GraphicsShaderStageFlags`.
const STAGE_VERTEX = 1;

export default class VulkanGraphicsProcessor implements ReactiveProcessor {
  private uuid = "";
  private width = 0;
  private height = 0;
  private variant = 1; // Default to Deno palette.
  private vertSpv: Uint8Array | null = null;
  private fragSpv: Uint8Array | null = null;
  private vk: VulkanContext | null = null;
  private dispatched = false;
  private errorMessage: string | null = null;

  setup(ctx: RuntimeContextFullAccess): void {
    const cfg = ctx.config;
    this.uuid = String(cfg["vulkan_surface_uuid"]);
    this.width = Number(cfg["width"] ?? 0);
    this.height = Number(cfg["height"] ?? 0);
    this.variant = Number(cfg["variant"] ?? 1);
    this.vertSpv = hexToBytes(String(cfg["vertex_spv_hex"] ?? ""));
    this.fragSpv = hexToBytes(String(cfg["fragment_spv_hex"] ?? ""));
    this.vk = VulkanContext.fromRuntime(ctx);
    console.error(
      `[VulkanGraphics/deno] setup uuid=${this.uuid} ` +
        `size=${this.width}x${this.height} ` +
        `vert_bytes=${this.vertSpv.byteLength} ` +
        `frag_bytes=${this.fragSpv.byteLength} variant=${this.variant}`,
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
        `[VulkanGraphics/deno] triangle drawn into surface '${this.uuid}'`,
      );
    } catch (e) {
      this.errorMessage = e instanceof Error ? e.message : String(e);
      console.error(
        `[VulkanGraphics/deno] dispatch failed: ${this.errorMessage}`,
      );
    }
  }

  private async dispatchOnce(): Promise<void> {
    if (this.vk === null || this.vertSpv === null || this.fragSpv === null) {
      throw new Error("VulkanContext / SPIR-V not initialized in setup");
    }
    // Push constants: layout is `{u32 variant}` (vertex stage only).
    const pc = new Uint8Array(4);
    new DataView(pc.buffer).setUint32(0, this.variant, true);

    {
      using guard = this.vk.acquireWrite(this.uuid);
      console.error(
        `[VulkanGraphics/deno] acquired vk_image=0x${
          guard.view.vkImage.toString(16)
        } layout=${guard.view.vkImageLayout}`,
      );
      await this.vk.dispatchGraphics({
        colorTarget: this.uuid,
        extentWidth: this.width,
        extentHeight: this.height,
        vertexSpv: this.vertSpv,
        fragmentSpv: this.fragSpv,
        pipelineState: trianglePipelineState(),
        bindings: [],
        bindingDecls: [],
        vertexBuffers: [],
        pushConstants: pc,
        pushConstantStages: STAGE_VERTEX,
        descriptorSetsInFlight: 2,
        frameIndex: 0,
        viewport: {
          x: 0.0,
          y: 0.0,
          width: this.width,
          height: this.height,
          min_depth: 0.0,
          max_depth: 1.0,
        },
        scissor: {
          x: 0,
          y: 0,
          width: this.width,
          height: this.height,
        },
        label: "polyglot-triangle-deno",
      });
    }
    // Producer-side cross-process release (#643). The graphics
    // kernel's offscreen_render leaves the image in
    // COLOR_ATTACHMENT_OPTIMAL after the render pass.
    this.vk.releaseForCrossProcess(
      this.uuid,
      VkImageLayout.ColorAttachmentOptimal,
    );
    console.error(
      `[VulkanGraphics/deno] published cross-process release ` +
        `layout=ColorAttachmentOptimal for surface '${this.uuid}'`,
    );
  }

  teardown(_ctx: RuntimeContextFullAccess): void {
    console.error(
      `[VulkanGraphics/deno] teardown dispatched=${this.dispatched} ` +
        `error=${this.errorMessage}`,
    );
  }
}

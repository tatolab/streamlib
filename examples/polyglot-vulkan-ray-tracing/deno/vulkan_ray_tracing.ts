// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Polyglot Vulkan ray-tracing processor — Deno twin of
 * `examples/polyglot-vulkan-ray-tracing/python/vulkan_ray_tracing.py`.
 *
 * End-to-end gate for the subprocess `VulkanContext.dispatchRayTracing`
 * runtime (#667). The host pre-allocates a storage-image-capable
 * `HostVulkanTexture` (transitioned to `GENERAL`), registers it
 * via `GpuContext::register_texture_with_layout` under a known UUID,
 * and installs a `RayTracingKernelBridge` wired to its
 * `VulkanRayTracingKernel` + `VulkanAccelerationStructure`. This
 * processor receives a trigger Videoframe, builds a single-triangle
 * BLAS + identity TLAS via escalate IPC, registers the RT kernel,
 * and dispatches one `vkCmdTraceRaysKHR` against the host's
 * storage image.
 *
 * Same scene as the Python twin (single triangle in XY plane, static
 * camera at +Z=2.5), with `variant = 1` so the miss shader paints
 * the violet-orange Deno palette instead of the teal-magenta Python
 * one. Visually distinct PNGs make reviewer comparisons easy.
 *
 * Config keys: vulkan_surface_uuid, width, height, variant,
 * rgen_spv_hex, rmiss_spv_hex, rchit_spv_hex.
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

function f32sToBytes(values: readonly number[]): Uint8Array {
  const buf = new ArrayBuffer(values.length * 4);
  const dv = new DataView(buf);
  for (let i = 0; i < values.length; i++) {
    dv.setFloat32(i * 4, values[i], true);
  }
  return new Uint8Array(buf);
}

function u32sToBytes(values: readonly number[]): Uint8Array {
  const buf = new ArrayBuffer(values.length * 4);
  const dv = new DataView(buf);
  for (let i = 0; i < values.length; i++) {
    dv.setUint32(i * 4, values[i], true);
  }
  return new Uint8Array(buf);
}

// Single-triangle scene at the origin in the XY plane, vertices at
// radius ≈ 0.6 to fill the central two-thirds of the static-camera
// frame. Same as the Python twin so both subprocesses produce
// directly-comparable PNG output (only the sky palette differs).
const TRIANGLE_VERTICES = [
  0.0, 0.6, 0.0, // top
  -0.6, -0.4, 0.0, // bottom-left
  0.6, -0.4, 0.0, // bottom-right
];
const TRIANGLE_INDICES = [0, 1, 2];

// Stage-flag bitmask layout — match the wire format and the host's
// `RayTracingShaderStageFlags`.
const STAGE_RAYGEN = 0b00_0001;
const STAGE_MISS = 0b00_0010;

// Sentinel for "absent stage index" in the wire format. JTD has no
// `Option<uint32>`, so the wire form uses 0xFFFFFFFF.
const NO_STAGE = 0xFFFFFFFF;

function identityTransform(): readonly number[] {
  return [
    1.0, 0.0, 0.0, 0.0,
    0.0, 1.0, 0.0, 0.0,
    0.0, 0.0, 1.0, 0.0,
  ];
}

export default class VulkanRayTracingProcessor implements ReactiveProcessor {
  private uuid = "";
  private width = 0;
  private height = 0;
  private variant = 1; // Default to Deno palette.
  private rgenSpv: Uint8Array | null = null;
  private rmissSpv: Uint8Array | null = null;
  private rchitSpv: Uint8Array | null = null;
  private vk: VulkanContext | null = null;
  private dispatched = false;
  private errorMessage: string | null = null;

  setup(ctx: RuntimeContextFullAccess): void {
    const cfg = ctx.config;
    this.uuid = String(cfg["vulkan_surface_uuid"]);
    this.width = Number(cfg["width"] ?? 0);
    this.height = Number(cfg["height"] ?? 0);
    this.variant = Number(cfg["variant"] ?? 1);
    this.rgenSpv = hexToBytes(String(cfg["rgen_spv_hex"] ?? ""));
    this.rmissSpv = hexToBytes(String(cfg["rmiss_spv_hex"] ?? ""));
    this.rchitSpv = hexToBytes(String(cfg["rchit_spv_hex"] ?? ""));
    this.vk = VulkanContext.fromRuntime(ctx);
    console.error(
      `[VulkanRT/deno] setup uuid=${this.uuid} ` +
        `size=${this.width}x${this.height} ` +
        `rgen_bytes=${this.rgenSpv.byteLength} ` +
        `rmiss_bytes=${this.rmissSpv.byteLength} ` +
        `rchit_bytes=${this.rchitSpv.byteLength} variant=${this.variant}`,
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
        `[VulkanRT/deno] trace dispatched into surface '${this.uuid}'`,
      );
    } catch (e) {
      this.errorMessage = e instanceof Error ? e.message : String(e);
      console.error(
        `[VulkanRT/deno] dispatch failed: ${this.errorMessage}`,
      );
    }
  }

  private async dispatchOnce(): Promise<void> {
    if (
      this.vk === null || this.rgenSpv === null || this.rmissSpv === null ||
      this.rchitSpv === null
    ) {
      throw new Error("VulkanContext / SPIR-V not initialized in setup");
    }

    // 1. Build the BLAS (single triangle in XY plane).
    const blasId = await this.vk.buildBlas({
      vertices: f32sToBytes(TRIANGLE_VERTICES),
      indices: u32sToBytes(TRIANGLE_INDICES),
      label: "polyglot-rt-deno-triangle",
    });
    console.error(`[VulkanRT/deno] BLAS as_id=${blasId}`);

    // 2. Build the TLAS with one identity-transformed instance.
    const tlasId = await this.vk.buildTlas({
      instances: [
        {
          blas_id: blasId,
          transform: identityTransform(),
          custom_index: 0,
          mask: 0xFF,
          sbt_record_offset: 0,
          // `1 = TRIANGLE_FACING_CULL_DISABLE` + `4 = FORCE_OPAQUE`
          flags: 0b101,
          // deno-lint-ignore no-explicit-any
        } as any,
      ],
      label: "polyglot-rt-deno-tlas",
    });
    console.error(`[VulkanRT/deno] TLAS as_id=${tlasId}`);

    // 3. Push constants: layout is `{u32 variant}`.
    const pc = new Uint8Array(4);
    new DataView(pc.buffer).setUint32(0, this.variant, true);

    // 4. Dispatch the trace. RT dispatch doesn't go through
    //    `acquireWrite` — the host's storage image is a
    //    `STORAGE_BINDING`-only `HostVulkanTexture` registered via
    //    `register_texture_with_layout` on the in-tree side, never
    //    exported to surface-share. The host bridge resolves the
    //    UUID from its own surface map.
    await this.vk.dispatchRayTracing({
        storageImage: this.uuid,
        tlasId,
        width: this.width,
        height: this.height,
        stages: [
          { stage: "ray_gen", spv: this.rgenSpv, entry_point: "main" },
          { stage: "miss", spv: this.rmissSpv, entry_point: "main" },
          { stage: "closest_hit", spv: this.rchitSpv, entry_point: "main" },
        ],
        // deno-lint-ignore no-explicit-any
        groups: [
          {
            kind: "general",
            general_stage: 0,
            closest_hit_stage: NO_STAGE,
            any_hit_stage: NO_STAGE,
            intersection_stage: NO_STAGE,
          },
          {
            kind: "general",
            general_stage: 1,
            closest_hit_stage: NO_STAGE,
            any_hit_stage: NO_STAGE,
            intersection_stage: NO_STAGE,
          },
          {
            kind: "triangles_hit",
            general_stage: NO_STAGE,
            closest_hit_stage: 2,
            any_hit_stage: NO_STAGE,
            intersection_stage: NO_STAGE,
          },
        ] as any,
        // deno-lint-ignore no-explicit-any
        bindingDecls: [
          {
            binding: 0,
            kind: "acceleration_structure",
            stages: STAGE_RAYGEN,
          },
          {
            binding: 1,
            kind: "storage_image",
            stages: STAGE_RAYGEN,
          },
        ] as any,
        // deno-lint-ignore no-explicit-any
        bindings: [
          {
            binding: 0,
            kind: "acceleration_structure",
            target_id: "<tlas>", // resolves to tlasId
          },
          {
            binding: 1,
            kind: "storage_image",
            target_id: "<self>", // resolves to surface uuid
          },
        ] as any,
        pushConstants: pc,
        pushConstantStages: STAGE_RAYGEN | STAGE_MISS,
        maxRecursionDepth: 1,
        label: "polyglot-rt-deno",
      });
    console.error(
      `[VulkanRT/deno] trace_rays complete for surface '${this.uuid}'`,
    );
  }

  teardown(_ctx: RuntimeContextFullAccess): void {
    console.error(
      `[VulkanRT/deno] teardown dispatched=${this.dispatched} ` +
        `error=${this.errorMessage}`,
    );
  }
}

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Polyglot GPU escalation channel — subprocess → host IPC for Deno.
 *
 * Mirrors the Python SDK's `escalate.py`. The subprocess sees a limited
 * GPU capability; anything that needs the privileged surface routes
 * through the Rust host via an `escalate_request` on stdout. The host
 * runs the op inside `GpuContextLimitedAccess::escalate` and replies
 * with `escalate_response` on stdin.
 *
 * Wire types (`EscalateRequest`, `EscalateResponse`, …) are generated
 * from `schemas/com.streamlib.escalate_{request,response}@1.0.0.yaml`
 * via `cargo xtask generate-schemas --runtime typescript`. This file
 * owns only the channel coordination logic (request-id bookkeeping,
 * lifecycle-message deferral, promise plumbing).
 */

import type {
  EscalateRequest,
  EscalateRequestAcquirePixelBuffer,
  EscalateRequestAcquireTexture,
  EscalateRequestLog,
  EscalateRequestRegisterAccelerationStructureBlas,
  EscalateRequestRegisterAccelerationStructureTlas,
  EscalateRequestRegisterAccelerationStructureTlasInstance,
  EscalateRequestRegisterComputeKernel,
  EscalateRequestRegisterGraphicsKernel,
  EscalateRequestRegisterGraphicsKernelBinding,
  EscalateRequestRegisterGraphicsKernelPipelineState,
  EscalateRequestRegisterRayTracingKernel,
  EscalateRequestRegisterRayTracingKernelBinding,
  EscalateRequestRegisterRayTracingKernelGroup,
  EscalateRequestRegisterRayTracingKernelStage,
  EscalateRequestReleaseHandle,
  EscalateRequestRunComputeKernel,
  EscalateRequestRunCpuReadbackCopy,
  EscalateRequestRunGraphicsDraw,
  EscalateRequestRunGraphicsDrawBinding,
  EscalateRequestRunGraphicsDrawDraw,
  EscalateRequestRunGraphicsDrawIndexBuffer,
  EscalateRequestRunGraphicsDrawScissor,
  EscalateRequestRunGraphicsDrawVertexBuffer,
  EscalateRequestRunGraphicsDrawViewport,
  EscalateRequestRunRayTracingKernel,
  EscalateRequestRunRayTracingKernelBinding,
  EscalateRequestTryRunCpuReadbackCopy,
} from "./_generated_/com_streamlib_escalate_request.ts";
import {
  EscalateRequestRunCpuReadbackCopyDirection,
  EscalateRequestRunGraphicsDrawDrawKind,
  EscalateRequestTryRunCpuReadbackCopyDirection,
} from "./_generated_/com_streamlib_escalate_request.ts";
import type {
  EscalateResponse,
  EscalateResponseContended,
  EscalateResponseErr,
  EscalateResponseOk,
} from "./_generated_/com_streamlib_escalate_response.ts";
import { monotonicNowNs } from "./clock.ts";

export type {
  EscalateRequest,
  EscalateRequestAcquirePixelBuffer,
  EscalateRequestAcquireTexture,
  EscalateRequestLog,
  EscalateRequestRegisterAccelerationStructureBlas,
  EscalateRequestRegisterAccelerationStructureTlas,
  EscalateRequestRegisterAccelerationStructureTlasInstance,
  EscalateRequestRegisterComputeKernel,
  EscalateRequestRegisterGraphicsKernel,
  EscalateRequestRegisterGraphicsKernelBinding,
  EscalateRequestRegisterGraphicsKernelPipelineState,
  EscalateRequestRegisterRayTracingKernel,
  EscalateRequestRegisterRayTracingKernelBinding,
  EscalateRequestRegisterRayTracingKernelGroup,
  EscalateRequestRegisterRayTracingKernelStage,
  EscalateRequestReleaseHandle,
  EscalateRequestRunComputeKernel,
  EscalateRequestRunCpuReadbackCopy,
  EscalateRequestRunGraphicsDraw,
  EscalateRequestRunGraphicsDrawBinding,
  EscalateRequestRunGraphicsDrawDraw,
  EscalateRequestRunGraphicsDrawIndexBuffer,
  EscalateRequestRunGraphicsDrawScissor,
  EscalateRequestRunGraphicsDrawVertexBuffer,
  EscalateRequestRunGraphicsDrawViewport,
  EscalateRequestRunRayTracingKernel,
  EscalateRequestRunRayTracingKernelBinding,
  EscalateRequestTryRunCpuReadbackCopy,
  EscalateResponse,
  EscalateResponseContended,
  EscalateResponseErr,
  EscalateResponseOk,
};
export {
  EscalateRequestRunCpuReadbackCopyDirection,
  EscalateRequestRunGraphicsDrawDrawKind,
  EscalateRequestTryRunCpuReadbackCopyDirection,
};

/** Backwards-compat alias for the `ok` variant of [`EscalateResponse`]. */
export type EscalateOkResponse = EscalateResponseOk;
/** Backwards-compat alias for the `err` variant of [`EscalateResponse`]. */
export type EscalateErrResponse = EscalateResponseErr;

export const ESCALATE_REQUEST_RPC = "escalate_request";
export const ESCALATE_RESPONSE_RPC = "escalate_response";

/**
 * Default upper bound on how long [`EscalateChannel.request`] waits for
 * a correlated response before rejecting. Generous enough for realistic
 * escalate ops (compute pipeline-cache cold start, GPU readback under
 * load) but tight enough that a stuck host or zombie reader surfaces
 * as a clear `EscalateError("escalate timed out…")` instead of an
 * unobservable hang. Callers can override per-request via the
 * `timeoutMs` option.
 */
export const DEFAULT_REQUEST_TIMEOUT_MS = 60_000;

/**
 * Caller-facing payload — the discriminator variants of
 * [`EscalateRequest`] with `request_id` stripped. The channel injects
 * `request_id` when serializing onto the wire.
 */
export type EscalateOpPayload =
  | Omit<EscalateRequestAcquirePixelBuffer, "request_id">
  | Omit<EscalateRequestAcquireTexture, "request_id">
  | Omit<EscalateRequestRegisterAccelerationStructureBlas, "request_id">
  | Omit<EscalateRequestRegisterAccelerationStructureTlas, "request_id">
  | Omit<EscalateRequestRegisterComputeKernel, "request_id">
  | Omit<EscalateRequestRegisterGraphicsKernel, "request_id">
  | Omit<EscalateRequestRegisterRayTracingKernel, "request_id">
  | Omit<EscalateRequestReleaseHandle, "request_id">
  | Omit<EscalateRequestRunComputeKernel, "request_id">
  | Omit<EscalateRequestRunCpuReadbackCopy, "request_id">
  | Omit<EscalateRequestRunGraphicsDraw, "request_id">
  | Omit<EscalateRequestRunRayTracingKernel, "request_id">
  | Omit<EscalateRequestTryRunCpuReadbackCopy, "request_id">;

/**
 * Encode bytes as lowercase hex without separators. Matches the wire
 * shape expected by `register_compute_kernel.spv_hex` and
 * `run_compute_kernel.push_constants_hex`.
 */
function bytesToHex(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i++) {
    s += bytes[i].toString(16).padStart(2, "0");
  }
  return s;
}

export class EscalateError extends Error {}

/** Sentinel resolved value for an escalate request that opted into the
 * contended-skip shape (e.g. `try_acquire_cpu_readback`) and that the
 * host responded to with `result: "contended"`. The
 * [`EscalateChannel.tryAcquireCpuReadback`] caller branches on
 * `null` vs. an [`EscalateOkResponse`] payload. */
export const ESCALATE_CONTENDED = null;

type Pending = {
  resolve: (value: EscalateOkResponse | null) => void;
  reject: (err: Error) => void;
  allowContended: boolean;
};

/**
 * Bidirectional escalate channel wired into the subprocess_runner's
 * stdin demux. Call `handleIncoming(msg)` from the central stdin
 * reader for every message tagged `rpc: "escalate_response"`.
 */
export class EscalateChannel {
  private pending = new Map<string, Pending>();
  private counter = 0;
  private writer: (msg: Record<string, unknown>) => Promise<void>;

  constructor(writer: (msg: Record<string, unknown>) => Promise<void>) {
    this.writer = writer;
  }

  async acquirePixelBuffer(
    width: number,
    height: number,
    format = "bgra",
  ): Promise<EscalateOkResponse> {
    return await this.request({
      op: "acquire_pixel_buffer",
      width,
      height,
      format,
    }) as EscalateOkResponse;
  }

  async acquireTexture(
    width: number,
    height: number,
    format: string,
    usage: readonly string[],
  ): Promise<EscalateOkResponse> {
    if (usage.length === 0) {
      throw new EscalateError("acquireTexture: usage must not be empty");
    }
    return await this.request({
      op: "acquire_texture",
      width,
      height,
      format,
      usage: [...usage],
    }) as EscalateOkResponse;
  }

  /**
   * Trigger the host-side cpu-readback copy for an already-registered
   * surface. `direction` selects `image_to_buffer` (host runs
   * `vkCmdCopyImageToBuffer`) or `buffer_to_image` (host runs the
   * inverse on write release). The host signals a new value on the
   * surface's shared timeline and returns the value in `timeline_value`
   * (decimal-string `u64`); the consumer waits on its imported
   * `ConsumerVulkanTimelineSemaphore` at that value before reading or
   * after writing the staging buffers' mapped bytes.
   */
  async runCpuReadbackCopy(
    surfaceId: bigint | number,
    direction: "image_to_buffer" | "buffer_to_image",
  ): Promise<EscalateOkResponse> {
    if (direction !== "image_to_buffer" && direction !== "buffer_to_image") {
      throw new EscalateError(
        `runCpuReadbackCopy: direction must be 'image_to_buffer' or 'buffer_to_image', got ${
          JSON.stringify(direction)
        }`,
      );
    }
    const wireDirection = direction === "image_to_buffer"
      ? EscalateRequestRunCpuReadbackCopyDirection.ImageToBuffer
      : EscalateRequestRunCpuReadbackCopyDirection.BufferToImage;
    return await this.request({
      op: "run_cpu_readback_copy",
      surface_id: typeof surfaceId === "bigint"
        ? surfaceId.toString()
        : Math.trunc(surfaceId).toString(),
      direction: wireDirection,
    }) as EscalateOkResponse;
  }

  /**
   * Non-blocking variant of [`runCpuReadbackCopy`]. Resolves to the
   * same `ok`-payload on success or to `null` on contention; rejects
   * on hard errors.
   */
  async tryRunCpuReadbackCopy(
    surfaceId: bigint | number,
    direction: "image_to_buffer" | "buffer_to_image",
  ): Promise<EscalateOkResponse | null> {
    if (direction !== "image_to_buffer" && direction !== "buffer_to_image") {
      throw new EscalateError(
        `tryRunCpuReadbackCopy: direction must be 'image_to_buffer' or 'buffer_to_image', got ${
          JSON.stringify(direction)
        }`,
      );
    }
    const wireDirection = direction === "image_to_buffer"
      ? EscalateRequestTryRunCpuReadbackCopyDirection.ImageToBuffer
      : EscalateRequestTryRunCpuReadbackCopyDirection.BufferToImage;
    return await this.request(
      {
        op: "try_run_cpu_readback_copy",
        surface_id: typeof surfaceId === "bigint"
          ? surfaceId.toString()
          : Math.trunc(surfaceId).toString(),
        direction: wireDirection,
      },
      { allowContended: true },
    );
  }

  /**
   * Register a compute kernel on the host. Resolves to the `ok`-payload
   * whose `handle_id` is the SHA-256 hex of the SPIR-V — re-registering
   * identical SPIR-V hits the host-side cache and returns the same id.
   *
   * The host derives the kernel's binding shape from `rspirv-reflect`
   * and persists driver-compiled pipeline state to
   * `$STREAMLIB_PIPELINE_CACHE_DIR` (or
   * `$XDG_CACHE_HOME/streamlib/pipeline-cache`) so first-inference
   * latency after a host process restart is fast.
   */
  async registerComputeKernel(
    spv: Uint8Array,
    pushConstantSize: number,
  ): Promise<EscalateOkResponse> {
    return await this.request({
      op: "register_compute_kernel",
      spv_hex: bytesToHex(spv),
      push_constant_size: Math.trunc(pushConstantSize),
    }) as EscalateOkResponse;
  }

  /**
   * Dispatch a previously-registered compute kernel against
   * `surfaceId`. Compute is synchronous host-side: this resolves once
   * the GPU work has retired, after which the consumer can advance
   * its surface-share timeline. `pushConstants` length must equal the
   * kernel's declared `push_constant_size`.
   */
  async runComputeKernel(
    kernelId: string,
    surfaceUuid: string,
    pushConstants: Uint8Array,
    groupCountX: number,
    groupCountY: number,
    groupCountZ: number,
  ): Promise<EscalateOkResponse> {
    return await this.request({
      op: "run_compute_kernel",
      kernel_id: kernelId,
      surface_uuid: surfaceUuid,
      push_constants_hex: bytesToHex(pushConstants),
      group_count_x: Math.trunc(groupCountX),
      group_count_y: Math.trunc(groupCountY),
      group_count_z: Math.trunc(groupCountZ),
    }) as EscalateOkResponse;
  }

  /**
   * Register a graphics kernel on the host. Resolves to the
   * `ok`-payload whose `handle_id` is the bridge-assigned kernel_id
   * (typically a stable hash over a canonical representation of all
   * register-time inputs — re-registering an identical descriptor hits
   * the host-side cache and returns the same id).
   *
   * `vertexSpv` and `fragmentSpv` are raw SPIR-V bytes. The host
   * derives the binding shape from `rspirv-reflect`, validates the
   * declared `bindings` match the merged shader declaration, and
   * persists driver-compiled pipeline state to the same on-disk
   * pipeline cache compute uses.
   */
  async registerGraphicsKernel(args: {
    label: string;
    vertexSpv: Uint8Array;
    fragmentSpv: Uint8Array;
    bindings: readonly EscalateRequestRegisterGraphicsKernelBinding[];
    pushConstantSize: number;
    pushConstantStages: number;
    descriptorSetsInFlight: number;
    pipelineState: EscalateRequestRegisterGraphicsKernelPipelineState;
    vertexEntryPoint?: string;
    fragmentEntryPoint?: string;
  }): Promise<EscalateOkResponse> {
    return await this.request({
      op: "register_graphics_kernel",
      label: args.label,
      vertex_spv_hex: bytesToHex(args.vertexSpv),
      fragment_spv_hex: bytesToHex(args.fragmentSpv),
      vertex_entry_point: args.vertexEntryPoint ?? "main",
      fragment_entry_point: args.fragmentEntryPoint ?? "main",
      bindings: [...args.bindings],
      push_constant_size: Math.trunc(args.pushConstantSize),
      push_constant_stages: Math.trunc(args.pushConstantStages),
      descriptor_sets_in_flight: Math.trunc(args.descriptorSetsInFlight),
      pipeline_state: args.pipelineState,
    }) as EscalateOkResponse;
  }

  /**
   * Issue one draw against a previously-registered graphics kernel.
   *
   * Graphics dispatch is synchronous host-side: this resolves once
   * the host's own command buffer + fence have retired and the host's
   * writes to the color attachments are visible to subsequent
   * submissions. `frameIndex` indexes the kernel's descriptor-set
   * ring (`0 ≤ frameIndex < descriptorSetsInFlight`).
   */
  async runGraphicsDraw(args: {
    kernelId: string;
    frameIndex: number;
    bindings: readonly EscalateRequestRunGraphicsDrawBinding[];
    vertexBuffers: readonly EscalateRequestRunGraphicsDrawVertexBuffer[];
    colorTargetUuids: readonly string[];
    extentWidth: number;
    extentHeight: number;
    pushConstants: Uint8Array;
    draw: EscalateRequestRunGraphicsDrawDraw;
    indexBuffer?: EscalateRequestRunGraphicsDrawIndexBuffer;
    depthTargetUuid?: string;
    viewport?: EscalateRequestRunGraphicsDrawViewport;
    scissor?: EscalateRequestRunGraphicsDrawScissor;
  }): Promise<EscalateOkResponse> {
    const payload = {
      op: "run_graphics_draw" as const,
      kernel_id: args.kernelId,
      frame_index: Math.trunc(args.frameIndex),
      bindings: [...args.bindings],
      vertex_buffers: [...args.vertexBuffers],
      color_target_uuids: [...args.colorTargetUuids],
      extent_width: Math.trunc(args.extentWidth),
      extent_height: Math.trunc(args.extentHeight),
      push_constants_hex: bytesToHex(args.pushConstants),
      draw: args.draw,
    } as Record<string, unknown>;
    if (args.indexBuffer !== undefined) {
      payload.index_buffer = args.indexBuffer;
    }
    if (args.depthTargetUuid !== undefined) {
      payload.depth_target_uuid = args.depthTargetUuid;
    }
    if (args.viewport !== undefined) {
      payload.viewport = args.viewport;
    }
    if (args.scissor !== undefined) {
      payload.scissor = args.scissor;
    }
    return await this.request(
      payload as unknown as EscalateOpPayload,
    ) as EscalateOkResponse;
  }

  /**
   * Build a triangle-geometry BLAS on the host. Resolves to the
   * `ok`-payload whose `handle_id` is the bridge-assigned `as_id`.
   *
   * `vertices` is the raw little-endian f32 vertex blob (interleaved
   * `[x, y, z, ...]` — R32G32B32_SFLOAT, stride 12 bytes; total length
   * must be a multiple of 12). `indices` is the raw little-endian u32
   * index blob (three indices per triangle; total length must be a
   * multiple of 12).
   */
  async registerAccelerationStructureBlas(args: {
    label: string;
    vertices: Uint8Array;
    indices: Uint8Array;
  }): Promise<EscalateOkResponse> {
    return await this.request({
      op: "register_acceleration_structure_blas",
      label: args.label,
      vertices_hex: bytesToHex(args.vertices),
      indices_hex: bytesToHex(args.indices),
    }) as EscalateOkResponse;
  }

  /**
   * Build a TLAS on the host from a list of instances referencing
   * previously-built BLASes. Resolves to the `ok`-payload whose
   * `handle_id` is the bridge-assigned `as_id`. Each entry's
   * `transform` is exactly 12 floats (row-major 3×4); `mask` is a
   * uint32 carrying the 8-bit visibility mask (host rejects values
   * > 0xff).
   */
  async registerAccelerationStructureTlas(args: {
    label: string;
    instances:
      readonly EscalateRequestRegisterAccelerationStructureTlasInstance[];
  }): Promise<EscalateOkResponse> {
    return await this.request({
      op: "register_acceleration_structure_tlas",
      label: args.label,
      instances: [...args.instances],
    }) as EscalateOkResponse;
  }

  /**
   * Register a ray-tracing kernel on the host. Resolves to the
   * `ok`-payload whose `handle_id` is the bridge-assigned `kernel_id`
   * (typically a stable hash over a canonical representation of all
   * register-time inputs — re-registering an identical descriptor hits
   * the host-side cache and returns the same id).
   *
   * `stages` is a list of `{stage, spv_hex, entry_point}` shapes;
   * `groups` is the SBT layout (general / triangles_hit /
   * procedural_hit) referencing stage indices into `stages` (use
   * `0xFFFFFFFF` as the absent-stage sentinel — JTD has no
   * `Option<uint32>`); `bindings` is descriptor-set-0 with stages
   * bitmask `1=RAYGEN | 2=MISS | 4=CLOSEST_HIT | 8=ANY_HIT |
   * 16=INTERSECTION | 32=CALLABLE`.
   */
  async registerRayTracingKernel(args: {
    label: string;
    /** Per-stage SPIR-V `Uint8Array` plus stage classification. The
     * channel hex-encodes each stage's bytes inline so callers don't
     * have to. `entry_point` defaults to `"main"`. */
    stages: readonly {
      stage: EscalateRequestRegisterRayTracingKernelStage["stage"];
      spv: Uint8Array;
      entry_point?: string;
    }[];
    groups: readonly EscalateRequestRegisterRayTracingKernelGroup[];
    bindings: readonly EscalateRequestRegisterRayTracingKernelBinding[];
    pushConstantSize: number;
    pushConstantStages: number;
    maxRecursionDepth: number;
  }): Promise<EscalateOkResponse> {
    return await this.request({
      op: "register_ray_tracing_kernel",
      label: args.label,
      stages: args.stages.map((s) => ({
        stage: s.stage,
        spv_hex: bytesToHex(s.spv),
        entry_point: s.entry_point ?? "main",
      })) as unknown as EscalateRequestRegisterRayTracingKernelStage[],
      groups: [...args.groups],
      bindings: [...args.bindings],
      push_constant_size: Math.trunc(args.pushConstantSize),
      push_constant_stages: Math.trunc(args.pushConstantStages),
      max_recursion_depth: Math.trunc(args.maxRecursionDepth),
    }) as EscalateOkResponse;
  }

  /**
   * Issue one `vkCmdTraceRaysKHR` against a previously-registered RT
   * kernel. RT dispatch is synchronous host-side: this resolves once
   * the host's own command buffer + fence have retired and the
   * host's writes to the bound storage image are visible to
   * subsequent submissions.
   */
  async runRayTracingKernel(args: {
    kernelId: string;
    bindings: readonly EscalateRequestRunRayTracingKernelBinding[];
    pushConstants: Uint8Array;
    width: number;
    height: number;
    depth: number;
  }): Promise<EscalateOkResponse> {
    return await this.request({
      op: "run_ray_tracing_kernel",
      kernel_id: args.kernelId,
      bindings: [...args.bindings],
      push_constants_hex: bytesToHex(args.pushConstants),
      width: Math.trunc(args.width),
      height: Math.trunc(args.height),
      depth: Math.trunc(args.depth),
    }) as EscalateOkResponse;
  }

  async releaseHandle(handleId: string): Promise<EscalateOkResponse> {
    return await this.request({ op: "release_handle", handle_id: handleId }) as
      EscalateOkResponse;
  }

  /**
   * Send a fire-and-forget `log` op. The host enqueues the record into
   * the unified JSONL pipeline and returns nothing — no `request_id`,
   * no correlated response. The frame goes through the same writer lock
   * as request/response traffic so length-prefix and payload stay
   * contiguous on the wire.
   */
  async logFireAndForget(payload: EscalateRequestLog): Promise<void> {
    const msg = {
      rpc: ESCALATE_REQUEST_RPC,
      ...payload,
    } as Record<string, unknown>;
    await this.writer(msg);
  }

  async request(
    op: EscalateOpPayload,
    options: { allowContended?: boolean; timeoutMs?: number } = {},
  ): Promise<EscalateOkResponse | null> {
    const allowContended = options.allowContended === true;
    const timeoutMs = options.timeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS;
    const requestId = this.nextRequestId();
    const msg = {
      rpc: ESCALATE_REQUEST_RPC,
      request_id: requestId,
      ...op,
    } as Record<string, unknown>;
    const promise = new Promise<EscalateOkResponse | null>(
      (resolve, reject) => {
        this.pending.set(requestId, { resolve, reject, allowContended });
      },
    );
    // Schedule a timeout that races with the response. Like
    // `handleIncoming` and `cancelAll`, the timeout pops the pending
    // entry under the same map so a late delivery sees no slot and is
    // dropped — no resolve/reject double-fire.
    const timeoutHandle = setTimeout(() => {
      const p = this.pending.get(requestId);
      if (p) {
        this.pending.delete(requestId);
        p.reject(
          new EscalateError(`escalate timed out after ${timeoutMs}ms`),
        );
      }
    }, timeoutMs);
    try {
      await this.writer(msg);
    } catch (e) {
      // Wrap non-EscalateError exceptions (broken pipe from the
      // bridge writer, etc.) so callers see one error type for every
      // failure mode of the channel — same uniformity the Python SDK
      // gives.
      const p = this.pending.get(requestId);
      if (p) {
        this.pending.delete(requestId);
        p.reject(
          e instanceof EscalateError
            ? e
            : new EscalateError(`escalate channel send failed: ${e}`),
        );
      }
    }
    try {
      return await promise;
    } finally {
      clearTimeout(timeoutHandle);
    }
  }

  /**
   * Consume an escalate_response. Returns true if the message was
   * recognised as an escalate response (so the caller can skip
   * lifecycle dispatch for it).
   */
  handleIncoming(msg: Record<string, unknown>): boolean {
    if (msg.rpc !== ESCALATE_RESPONSE_RPC) return false;
    const requestId = msg.request_id as string | undefined;
    if (!requestId) return true; // malformed; eaten
    const pending = this.pending.get(requestId);
    if (!pending) return true;
    this.pending.delete(requestId);
    if (msg.result === "ok") {
      pending.resolve(msg as unknown as EscalateOkResponse);
    } else if (msg.result === "contended") {
      if (pending.allowContended) {
        pending.resolve(ESCALATE_CONTENDED);
      } else {
        pending.reject(
          new EscalateError(
            "escalate returned contended for an op that does not allow it",
          ),
        );
      }
    } else {
      const err = new EscalateError(
        (msg.message as string | undefined) ?? "escalate failed",
      );
      pending.reject(err);
    }
    return true;
  }

  /** Reject all in-flight requests (e.g. on shutdown). */
  cancelAll(reason = "subprocess shutting down"): void {
    for (const [id, pending] of this.pending.entries()) {
      pending.reject(new EscalateError(reason));
      this.pending.delete(id);
    }
  }

  private nextRequestId(): string {
    this.counter += 1;
    // Short correlation id is enough — request_id only has to be unique
    // within this subprocess's escalate channel. Stamps with the
    // canonical monotonic clock for cross-process consistency; falls
    // back to `Date.now()` only if the clock isn't installed yet (which
    // can happen during early bridge handshakes before subprocess_runner
    // wires the FFI lib).
    let stamp: number;
    try {
      stamp = Number(monotonicNowNs() & 0xffffffffn);
    } catch {
      stamp = Date.now();
    }
    return `dn-${stamp.toString(36)}-${this.counter}`;
  }
}

/**
 * Process-wide escalate channel singleton — mirror of Python's
 * `streamlib.escalate.channel()`. Subprocess runner installs it after
 * wiring the bridge stdio pipes; processor code (and SDK helpers like
 * `CpuReadbackContext.fromRuntime`) reach for it via `getChannel()`.
 *
 * Throws if the channel hasn't been installed — that only happens when
 * processor code runs outside the normal subprocess_runner lifecycle
 * (e.g. bare unit tests without a host).
 */
let _channelSingleton: EscalateChannel | null = null;

export function installChannel(channel: EscalateChannel): void {
  _channelSingleton = channel;
}

export function getChannel(): EscalateChannel {
  if (_channelSingleton === null) {
    throw new EscalateError(
      "escalate channel not installed — getChannel() is only available inside the subprocess lifecycle",
    );
  }
  return _channelSingleton;
}

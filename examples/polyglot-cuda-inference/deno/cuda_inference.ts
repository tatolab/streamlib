// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Polyglot CUDA inference processor — Deno twin of
 * `python/cuda_inference.py` (#591).
 *
 * Per `libs/streamlib-deno/adapters/cuda.ts` lines 28–37, Deno's ML
 * ecosystem (TensorFlow.js / ONNX-Runtime-Web / WebGPU bindings) does
 * not expose a native `from_dlpack` consumer for `DLManagedTensor*`,
 * so a Deno-side YOLO run is out of reach without a custom dlopen'd
 * cdylib that knows how to consume DLPack. The on-spec Deno gate for
 * #591 is therefore **structural capsule validation**: open the host-
 * pre-registered OPAQUE_FD cuda surface through
 * `CudaContext.acquireRead`, verify the DLPack capsule's
 * `device_type == kDLCUDA`, non-zero `device_ptr`, expected `size`.
 *
 * Per the polyglot workflow this is "language-specific by construction"
 * (Python carries the model, Deno carries the structural gate) — NOT
 * "Deno deferred." If a JS-ecosystem `from_dlpack` consumer lands,
 * file a follow-up to plug it in.
 *
 * Validation result is emitted via stderr — streamlib's log pipeline
 * captures it. The Deno subprocess sandbox does not grant
 * `--allow-write`, so artifact files are out of reach; the polyglot
 * pipeline's visual gate for this example is the **Python** annotated
 * YOLO PNG written by the Python processor. The Deno run is verified
 * by grepping `[CudaInference/deno] DLPack capsule OK` from the
 * scenario log.
 *
 * Config keys: identical shape to the Python processor (output_path
 * is accepted but unused on the Deno side).
 */

import type {
  ReactiveProcessor,
  RuntimeContextFullAccess,
  RuntimeContextLimitedAccess,
} from "../../../libs/streamlib-deno/mod.ts";
import { CudaContext } from "../../../libs/streamlib-deno/adapters/cuda.ts";

interface CudaInferenceConfig {
  cuda_surface_id: bigint | number;
  width: number;
  height: number;
  channels: number;
  output_path: string;
}

const DEVICE_TYPE_CUDA = 2;
const DEVICE_TYPE_CUDA_HOST = 3;

export default class CudaInferenceProcessor implements ReactiveProcessor {
  private surfaceId: bigint = 0n;
  private width = 0;
  private height = 0;
  private channels = 0;
  private cuda: CudaContext | null = null;
  private validated = false;
  private lastError: string | null = null;

  setup(ctx: RuntimeContextFullAccess): void {
    const cfg = ctx.config as CudaInferenceConfig;
    const sidRaw = cfg.cuda_surface_id;
    if (typeof sidRaw !== "number" && typeof sidRaw !== "bigint") {
      throw new Error(
        `[CudaInference/deno] cuda_surface_id must be a number, got ${typeof sidRaw}`,
      );
    }
    this.surfaceId = typeof sidRaw === "bigint" ? sidRaw : BigInt(sidRaw);
    this.width = Number(cfg.width);
    this.height = Number(cfg.height);
    this.channels = Number(cfg.channels);
    if (this.channels !== 4) {
      throw new Error(
        `[CudaInference/deno] expected channels=4 (BGRA8), got ${this.channels}`,
      );
    }
    this.cuda = CudaContext.fromRuntime(ctx);
    console.error(
      `[CudaInference/deno] setup surface_id=${this.surfaceId} ` +
        `${this.width}x${this.height} BGRA8`,
    );
  }

  process(ctx: RuntimeContextLimitedAccess): void {
    // Drain the trigger frame so the upstream port doesn't backpressure.
    const _frame = ctx.inputs.read("video_in");
    if (!_frame) return;
    if (this.validated) return;
    if (!this.cuda) {
      throw new Error("[CudaInference/deno] cuda context not initialized");
    }

    try {
      this.runOnce();
      this.validated = true;
      console.error(
        "[CudaInference/deno] VALIDATION_PASSED — DLPack capsule shape " +
          "round-trips through the cdylib OPAQUE_FD path",
      );
    } catch (e) {
      this.lastError = String(e);
      console.error(`[CudaInference/deno] VALIDATION_FAILED: ${e}`);
    }
  }

  private runOnce(): void {
    const expectedSize = BigInt(this.width * this.height * this.channels);
    using guard = this.cuda!.acquireRead(this.surfaceId);
    const view = guard.view;

    if (view.size !== expectedSize) {
      throw new Error(
        `read view size mismatch — expected ${expectedSize} bytes ` +
          `(w*h*c), got ${view.size}. Host buffer dimensions disagree ` +
          "with this processor's config.",
      );
    }
    if (
      view.deviceType !== DEVICE_TYPE_CUDA &&
      view.deviceType !== DEVICE_TYPE_CUDA_HOST
    ) {
      throw new Error(
        `read view device_type=${view.deviceType} is neither kDLCUDA (2) ` +
          "nor kDLCUDAHost (3) — capsule is not on a CUDA device",
      );
    }
    if (view.devicePtr === 0n) {
      throw new Error(
        "read view devicePtr is null — cdylib failed to map the OPAQUE_FD memory into CUDA",
      );
    }

    console.error(
      `[CudaInference/deno] DLPack capsule OK — ` +
        `device_type=${view.deviceType} ` +
        `(${view.deviceType === 2 ? "kDLCUDA" : "kDLCUDAHost"}), ` +
        `device_id=${view.deviceId}, ` +
        `device_ptr=0x${view.devicePtr.toString(16)}, ` +
        `size=${view.size} bytes`,
    );
  }

  teardown(_ctx: RuntimeContextFullAccess): void {
    console.error(
      `[CudaInference/deno] teardown validated=${this.validated} ` +
        `error=${this.lastError}`,
    );
  }
}

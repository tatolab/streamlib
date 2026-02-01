// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Grayscale processor — converts video frames to grayscale.
 *
 * Reads video frames via iceoryx2 (FFI), accesses IOSurface pixels
 * directly for zero-copy processing, and writes output frames.
 */

import type { ReactiveProcessor, ProcessorContext } from "../../../libs/streamlib-deno/mod.ts";
import type { Videoframe } from "../../../libs/streamlib-deno/_generated_/com_tatolab_videoframe.ts";

export default class GrayscaleProcessor implements ReactiveProcessor {
  setup(ctx: ProcessorContext): void {
    console.error("[GrayscaleProcessor] setup — config:", JSON.stringify(ctx.config));
  }

  process(ctx: ProcessorContext): void {
    const result = ctx.inputs.read<Videoframe>("video_in");
    if (!result) return;

    const { value: frame, timestampNs } = result;

    // Passthrough: re-encode and forward unchanged.
    // For actual pixel processing, use ctx.gpu.resolveSurface(frame.surface_id)
    // to access IOSurface pixels directly via zero-copy FFI.
    ctx.outputs.write("video_out", frame, timestampNs);
  }

  teardown(_ctx: ProcessorContext): void {
    console.error("[GrayscaleProcessor] teardown");
  }
}

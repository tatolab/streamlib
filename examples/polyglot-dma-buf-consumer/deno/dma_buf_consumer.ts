// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Linux polyglot DMA-BUF consumer processor — Deno twin of
 * `examples/polyglot-dma-buf-consumer/python/dma_buf_consumer.py`.
 *
 * Subscribes to a video input port, resolves the upstream surface_id through
 * the surface-share service (DMA-BUF FD via SCM_RIGHTS, Vulkan-imported into
 * the subprocess), locks the resulting handle for read, and probes the first
 * byte of the imported buffer to confirm the cross-process import worked
 * end-to-end. Then forwards the frame unmodified to the downstream output so
 * the rest of the pipeline (e.g. display) keeps moving.
 *
 * Stays dep-light on purpose — only the SDK and `Deno.UnsafePointerView` for
 * the byte probe. The point is to exercise the control plane and CPU-mapped
 * readback, not to do anything with the pixels.
 *
 * Config keys (all optional):
 *     force_bad_surface_id (bool, default false)
 *         Negative test mode. Replaces the upstream surface_id with a synthetic
 *         UUID that the surface-share service won't resolve, exercising the
 *         consumer's failure-handling path. Frames still propagate downstream
 *         so the rest of the pipeline doesn't deadlock.
 *     log_every (number, default 60)
 *         Throttle for periodic resolve-success / resolve-failure log lines.
 */

import type {
  ReactiveProcessor,
  RuntimeContextFullAccess,
  RuntimeContextLimitedAccess,
} from "../../../libs/streamlib-deno/mod.ts";
import type { Videoframe } from "../../../libs/streamlib-deno/_generated_/com_tatolab_videoframe.ts";

const BOGUS_SURFACE_ID = "00000000-0000-0000-0000-000000000000";

export default class DmaBufConsumer implements ReactiveProcessor {
  private forceBadId = false;
  private logEvery = 60;
  private resolveCount = 0;
  private errorCount = 0;
  private firstByte: number | null = null;

  setup(ctx: RuntimeContextFullAccess): void {
    this.forceBadId = Boolean(ctx.config["force_bad_surface_id"] ?? false);
    const rawLogEvery = ctx.config["log_every"];
    this.logEvery = typeof rawLogEvery === "number" && rawLogEvery > 0
      ? Math.floor(rawLogEvery)
      : 60;
    const mode = this.forceBadId ? "negative (force_bad_surface_id)" : "normal";
    console.error(
      `[DmaBufConsumer] setup mode=${mode} log_every=${this.logEvery}`,
    );
  }

  process(ctx: RuntimeContextLimitedAccess): void {
    const result = ctx.inputs.read<Videoframe>("video_in");
    if (!result) return;
    const { value: frame, timestampNs } = result;

    const upstreamId = frame.surface_id;
    if (!upstreamId) return;

    const surfaceId = this.forceBadId ? BOGUS_SURFACE_ID : upstreamId;

    try {
      const handle = ctx.gpuLimitedAccess.resolveSurface(surfaceId);
      handle.lock(true);
      try {
        const buffer = handle.asBuffer();
        if (buffer.byteLength === 0) {
          throw new Error("base address mapped a zero-length buffer");
        }
        this.firstByte = new Uint8Array(buffer)[0];
        this.resolveCount += 1;
        if (
          this.resolveCount <= 3 ||
          this.resolveCount % this.logEvery === 0
        ) {
          const hex = this.firstByte.toString(16).padStart(2, "0");
          console.error(
            `[DmaBufConsumer] resolved surface ${handle.width}x${handle.height} ` +
              `stride=${handle.bytesPerRow} first_byte=0x${hex} ` +
              `count=${this.resolveCount}`,
          );
        }
      } finally {
        handle.unlock(true);
        handle.release();
      }
    } catch (e) {
      this.errorCount += 1;
      if (
        this.errorCount <= 3 ||
        this.errorCount % this.logEvery === 0
      ) {
        const msg = e instanceof Error ? e.message : String(e);
        console.error(
          `[DmaBufConsumer] resolve_surface failed for ` +
            `surface_id=${JSON.stringify(surfaceId)}: ${msg} ` +
            `count=${this.errorCount}`,
        );
      }
    }

    ctx.outputs.write("video_out", frame, timestampNs);
  }

  teardown(_ctx: RuntimeContextFullAccess): void {
    console.error(
      `[DmaBufConsumer] teardown resolves=${this.resolveCount} ` +
        `errors=${this.errorCount} last_first_byte=${this.firstByte}`,
    );
  }
}

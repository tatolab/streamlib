// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Polyglot continuous processor — Deno reference example for issue #542.
 *
 * Demonstrates `execution: continuous` with monotonic-clock-driven
 * pacing. The subprocess runner's continuous-mode dispatch was
 * reworked in this issue to use `MonotonicTimer` (timerfd) instead
 * of `setTimeout`. This processor is the in-tree exemplar that
 * exercises that path.
 *
 * Each `process()` call records `monotonicNowNs()`, increments a
 * tick counter, and updates first/last-tick timestamps — all
 * in-memory, NO per-tick IO. On `teardown()` the final stats are
 * written to a host-visible output file as JSON.
 *
 * Why no cpu-readback / per-tick IO: the goal here is to measure
 * the runner's pacing accuracy. Per-tick escalate IPC or GPU
 * readback adds ~1–2ms of overhead that masks the timerfd's
 * drift-free behavior in the measurements. A real polyglot
 * continuous processor doing GPU work would use the Vulkan or
 * OpenGL adapter, not cpu-readback — cpu-readback is a last-resort
 * tool, not a hot-path one.
 */

import {
  type ContinuousProcessor,
  log,
  monotonicNowNs,
  type RuntimeContextFullAccess,
  type RuntimeContextLimitedAccess,
} from "../../../libs/streamlib-deno/mod.ts";

export default class PolyglotContinuousProcessor
  implements ContinuousProcessor {
  private outputFile = "";
  private tickCount = 0;
  private firstTickNs = 0n;
  private lastTickNs = 0n;

  setup(ctx: RuntimeContextFullAccess): void {
    const cfg = ctx.config;
    const outRaw = cfg["output_file"];
    if (typeof outRaw !== "string") {
      throw new Error(
        `output_file must be a string, got ${typeof outRaw}`,
      );
    }
    this.outputFile = outRaw;
    // Initialize the file so the host always finds something to read.
    this.writeStats();
    log.info("PolyglotContinuousProcessor setup", {
      output_file: this.outputFile,
    });
  }

  process(_ctx: RuntimeContextLimitedAccess): void {
    // Hot path: pure in-memory state update. No IO, no IPC. The
    // whole point of the example is to measure the runner's
    // MonotonicTimer pacing accuracy without confounding overhead.
    const nowNs = monotonicNowNs();
    if (this.tickCount === 0) {
      this.firstTickNs = nowNs;
    }
    this.lastTickNs = nowNs;
    this.tickCount += 1;
  }

  teardown(_ctx: RuntimeContextFullAccess): void {
    this.writeStats();
    log.info("PolyglotContinuousProcessor teardown", {
      ticks: this.tickCount,
      first_ns: String(this.firstTickNs),
      last_ns: String(this.lastTickNs),
    });
  }

  private writeStats(): void {
    try {
      const payload = JSON.stringify({
        tick_count: this.tickCount,
        first_tick_ns: String(this.firstTickNs),
        last_tick_ns: String(this.lastTickNs),
      });
      const tmp = `${this.outputFile}.tmp`;
      Deno.writeTextFileSync(tmp, payload);
      Deno.renameSync(tmp, this.outputFile);
    } catch (e) {
      log.warn("PolyglotContinuousProcessor write failed", {
        error: String(e),
        tick: this.tickCount,
      });
    }
  }
}

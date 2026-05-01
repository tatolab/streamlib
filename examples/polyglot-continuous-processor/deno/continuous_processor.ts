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
 */

import {
  type ContinuousProcessor,
  log,
  monotonicNowNs,
  type RuntimeContextFullAccess,
  type RuntimeContextLimitedAccess,
} from "../../../libs/streamlib-deno/mod.ts";
import { CpuReadbackContext } from "../../../libs/streamlib-deno/adapters/cpu_readback.ts";

export default class PolyglotContinuousProcessor
  implements ContinuousProcessor {
  private surfaceId = 0n;
  private cpuReadback: CpuReadbackContext | null = null;
  private tickCount = 0;
  private firstTickNs = 0n;
  private lastTickNs = 0n;

  setup(ctx: RuntimeContextFullAccess): void {
    const cfg = ctx.config;
    const sidRaw = cfg["cpu_readback_surface_id"];
    if (typeof sidRaw !== "number" && typeof sidRaw !== "bigint") {
      throw new Error(
        `cpu_readback_surface_id must be a number, got ${typeof sidRaw}`,
      );
    }
    this.surfaceId = typeof sidRaw === "bigint" ? sidRaw : BigInt(sidRaw);
    this.cpuReadback = CpuReadbackContext.fromRuntime(ctx);
    log.info("PolyglotContinuousProcessor setup", {
      surface_id: String(this.surfaceId),
    });
  }

  async process(_ctx: RuntimeContextLimitedAccess): Promise<void> {
    const nowNs = monotonicNowNs();
    if (this.tickCount === 0) {
      this.firstTickNs = nowNs;
    }
    this.lastTickNs = nowNs;
    this.tickCount = (this.tickCount + 1) & 0xFFFFFFFF;

    if (!this.cpuReadback) return;
    try {
      await using guard = await this.cpuReadback.acquireWrite(this.surfaceId);
      const plane = guard.view.plane(0);
      // Layout: u32 count, _pad (4B), u64 first_ns, u64 last_ns.
      const view = new DataView(
        plane.bytes.buffer,
        plane.bytes.byteOffset,
        plane.bytes.byteLength,
      );
      view.setUint32(0, this.tickCount, true);
      view.setUint32(4, 0, true); // padding
      view.setBigUint64(8, this.firstTickNs, true);
      view.setBigUint64(16, this.lastTickNs, true);
    } catch (e) {
      log.warn("PolyglotContinuousProcessor write failed", {
        error: String(e),
        tick: this.tickCount,
      });
    }
  }

  teardown(_ctx: RuntimeContextFullAccess): void {
    log.info("PolyglotContinuousProcessor teardown", {
      ticks: this.tickCount,
      first_ns: String(this.firstTickNs),
      last_ns: String(this.lastTickNs),
    });
  }
}

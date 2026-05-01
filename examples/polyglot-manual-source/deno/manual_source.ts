// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Polyglot manual source — Deno reference example for issues #542 + #604.
 *
 * Demonstrates the canonical `execution: manual` worker-loop idiom in
 * Deno — symmetrical to `python/manual_source.py`:
 *
 * 1. `start()` spawns a worker via async IIFE and returns promptly so
 *    lifecycle commands (`stop`/`teardown`) can land.
 * 2. The worker uses `MonotonicTimer` for drift-free pacing
 *    (NOT `setTimeout`).
 * 3. Each tick, the worker calls `ctx.outputs.write(...)` to publish a
 *    `Videoframe` over iceoryx2. The Rust counting-sink plugin
 *    subscribes to the same iceoryx2 service and counts frames; the
 *    scenario binary reads the sink's stats file post-stop to verify
 *    frames flowed.
 * 4. `stop()` flips a shutdown flag and awaits the worker.
 *
 * Pre-#604, the Deno cdylib's `sldn_output_write` aliased `&mut`
 * against the context (mirror of the Python issue). #604 moves the
 * iceoryx2 publisher map behind a `Mutex` so concurrent
 * `sldn_output_write` calls serialize. Combined with the runner's
 * always-on `_stdinReader` (now wired in manual mode too), worker
 * tasks can publish without deadlocking against the FD demuxer.
 *
 * If `start()` instead held a synchronous CPU loop, the subprocess
 * runner's `_stdinReader` couldn't deliver lifecycle messages and
 * teardown would hang. That's the failure mode this example exists to
 * rule out.
 */

import {
  type ManualProcessor,
  monotonicNowNs,
  MonotonicTimer,
  log,
  type RuntimeContextFullAccess,
} from "../../../libs/streamlib-deno/mod.ts";
import type { OutputPorts } from "../../../libs/streamlib-deno/types.ts";

export default class PolyglotManualSource implements ManualProcessor {
  private intervalNs = 33n * 1_000_000n;
  private width = 32;
  private height = 32;
  private surfaceIdPrefix = "polyglot-manual-source-deno";
  private frameCount = 0;
  private stopFlag = false;
  private worker: Promise<void> | null = null;
  // Captured in setup() so the worker task can publish without the
  // lifecycle ctx. NativeOutputPorts is a thin FFI shim — capturing it
  // is safe across tasks.
  private outputs: OutputPorts | null = null;

  setup(ctx: RuntimeContextFullAccess): void {
    const cfg = ctx.config;
    const intMsRaw = cfg["interval_ms"];
    const intervalMs = typeof intMsRaw === "number" ? intMsRaw : 33;
    this.intervalNs = BigInt(intervalMs) * 1_000_000n;
    if (typeof cfg["width"] === "number") this.width = cfg["width"];
    if (typeof cfg["height"] === "number") this.height = cfg["height"];
    if (typeof cfg["surface_id_prefix"] === "string") {
      this.surfaceIdPrefix = cfg["surface_id_prefix"];
    }
    this.outputs = ctx.outputs;
    log.info("PolyglotManualSource setup", {
      interval_ns: String(this.intervalNs),
      width: this.width,
      height: this.height,
    });
  }

  start(_ctx: RuntimeContextFullAccess): void {
    // SHARP EDGE: this method MUST return promptly. The subprocess
    // runner awaits start() inline; a synchronous CPU loop here means
    // the `_stdinReader` IIFE that demuxes lifecycle commands never
    // sees them and shutdown hangs. Spawn an async worker, return
    // immediately.
    this.stopFlag = false;
    this.worker = this.workerLoop();
    log.info("PolyglotManualSource start: worker spawned");
  }

  private async workerLoop(): Promise<void> {
    // MonotonicTimer is the canonical drift-free pacing primitive.
    // Replaces `setTimeout(intervalMs)` which queues behind the JS
    // event loop and accumulates drift.
    using timer = MonotonicTimer.create(this.intervalNs);
    while (!this.stopFlag) {
      const expirations = await timer.wait(100);
      if (expirations < 0n) {
        log.error("MonotonicTimer wait failed; worker exiting");
        return;
      }
      if (expirations === 0n) continue;
      for (let i = 0n; i < expirations && !this.stopFlag; i++) {
        this.publishFrame();
      }
    }
  }

  private publishFrame(): void {
    /** Publish one Videoframe on the `frame_out` port from this worker
     * task. Exercises the Deno cdylib's `Mutex` around the iceoryx2
     * publisher map (#604). */
    if (this.outputs === null) return;
    this.frameCount += 1;
    const tsNs = monotonicNowNs();
    const frame = {
      surface_id: `${this.surfaceIdPrefix}-${this.frameCount}`,
      width: this.width,
      height: this.height,
      timestamp_ns: String(tsNs),
      frame_index: String(this.frameCount),
    };
    try {
      this.outputs.write("frame_out", frame, tsNs);
    } catch (e) {
      log.warn("PolyglotManualSource publish failed", {
        error: String(e),
        frame_count: this.frameCount,
      });
    }
  }

  async stop(_ctx: RuntimeContextFullAccess): Promise<void> {
    this.stopFlag = true;
    if (this.worker !== null) {
      try {
        await this.worker;
      } catch (e) {
        log.warn("PolyglotManualSource worker exited with error", {
          error: String(e),
        });
      }
    }
    log.info("PolyglotManualSource stop: worker joined", {
      frames_emitted: this.frameCount,
    });
  }
}

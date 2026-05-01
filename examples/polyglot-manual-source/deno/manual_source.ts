// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Polyglot manual source — Deno reference example for issue #542.
 *
 * Demonstrates the canonical `execution: manual` worker-loop idiom in
 * Deno — symmetrical to `python/manual_source.py`:
 *
 * 1. `start()` spawns a worker via async IIFE and returns promptly.
 * 2. The worker uses `MonotonicTimer` for drift-free pacing
 *    (NOT `setTimeout`).
 * 3. Each tick, the worker writes an incrementing frame counter
 *    into a host-visible output file (atomic: write tmp + rename).
 *    The host reads this file post-stop to verify frames flowed.
 * 4. `stop()` flips a shutdown flag and awaits the worker.
 *
 * If `start()` instead held a synchronous CPU loop, the subprocess
 * runner's _stdinReader couldn't deliver lifecycle messages and
 * teardown would hang. That's the failure mode this example exists
 * to rule out.
 *
 * Why a file and not the iceoryx2 output port: a polyglot source
 * that publishes Videoframe payloads needs to allocate pixel buffers
 * via escalate IPC `acquire_pixel_buffer` and call `outputs.write`
 * from a thread the host reads — concurrent escalate IPC from a
 * worker is not safe under the current bridge protocol. A
 * host-visible file sidesteps that out-of-scope plumbing and keeps
 * the example focused on the worker idiom.
 */

import {
  type ManualProcessor,
  MonotonicTimer,
  log,
  type RuntimeContextFullAccess,
} from "../../../libs/streamlib-deno/mod.ts";

export default class PolyglotManualSource implements ManualProcessor {
  private outputFile = "";
  private intervalNs = 33n * 1_000_000n;
  private frameCount = 0;
  private stopFlag = false;
  private worker: Promise<void> | null = null;

  setup(ctx: RuntimeContextFullAccess): void {
    const cfg = ctx.config;
    const outRaw = cfg["output_file"];
    if (typeof outRaw !== "string") {
      throw new Error(
        `output_file must be a string, got ${typeof outRaw}`,
      );
    }
    this.outputFile = outRaw;
    const intMsRaw = cfg["interval_ms"];
    const intervalMs = typeof intMsRaw === "number" ? intMsRaw : 33;
    this.intervalNs = BigInt(intervalMs) * 1_000_000n;
    // Initialize the file so the host always finds something to read.
    this.writeCountAtomic(0);
    log.info("PolyglotManualSource setup", {
      output_file: this.outputFile,
      interval_ns: String(this.intervalNs),
    });
  }

  start(_ctx: RuntimeContextFullAccess): void {
    // SHARP EDGE: this method MUST return promptly. The subprocess
    // runner's outer command loop only iterates after start() returns;
    // a long-running synchronous start() means lifecycle messages
    // queue up and shutdown never happens. Spawn an async worker,
    // return immediately.
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
        this.frameCount += 1;
        this.writeCountAtomic(this.frameCount);
      }
    }
  }

  private writeCountAtomic(count: number): void {
    try {
      const tmp = `${this.outputFile}.tmp`;
      Deno.writeTextFileSync(tmp, String(count));
      Deno.renameSync(tmp, this.outputFile);
    } catch (e) {
      log.warn("PolyglotManualSource write failed", {
        error: String(e),
        count,
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
    // Final write so the host always sees the last value.
    this.writeCountAtomic(this.frameCount);
    log.info("PolyglotManualSource stop: worker joined", {
      frames_emitted: this.frameCount,
    });
  }
}

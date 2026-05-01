// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Concurrency tests for [`EscalateChannel`] (#604).
 *
 * The Deno bug shape under the pre-#604 architecture was *deadlock*,
 * not data corruption (single-threaded async event loop, no sibling
 * reader to alias the demuxer). When a manual-mode worker awaited an
 * escalate response, no task pumped the FD reader and the response
 * sat in the socketpair buffer forever. The fix moves the
 * `_stdinReader` IIFE to run unconditionally (manual mode included),
 * but the channel itself needs to demux concurrent requests by
 * `request_id` regardless — these tests pin that contract directly,
 * with a fake writer driving synthetic responses through
 * `handleIncoming`.
 *
 * The FD path itself is exercised end-to-end by
 * `tests/polyglot_linux_execution_modes.rs` against a real subprocess.
 */

import { assert, assertEquals } from "@std/assert";
import {
  EscalateChannel,
  EscalateError,
  type EscalateOkResponse,
} from "./escalate.ts";

// ============================================================================
// Helpers
// ============================================================================

interface CapturedRequest {
  request_id: string;
  payload: Record<string, unknown>;
}

class FakeBridge {
  readonly captured: CapturedRequest[] = [];
  private resolveCount?: () => void;
  private targetCount = 0;

  async write(msg: Record<string, unknown>): Promise<void> {
    this.captured.push({
      request_id: msg.request_id as string,
      payload: { ...msg },
    });
    if (
      this.resolveCount !== undefined && this.captured.length >= this.targetCount
    ) {
      const fn = this.resolveCount;
      this.resolveCount = undefined;
      fn();
    }
  }

  /** Wait until `count` requests have been captured (or fail-fast). */
  async waitForCount(count: number, timeoutMs = 10_000): Promise<void> {
    if (this.captured.length >= count) return;
    this.targetCount = count;
    const captured = await new Promise<true>((resolve, reject) => {
      this.resolveCount = () => resolve(true);
      const timer = setTimeout(() => {
        this.resolveCount = undefined;
        reject(
          new Error(
            `timed out waiting for ${count} requests (got ${this.captured.length})`,
          ),
        );
      }, timeoutMs);
      // Resolve clears the timer.
      const wrapped = this.resolveCount;
      this.resolveCount = () => {
        clearTimeout(timer);
        wrapped();
      };
    });
    assert(captured);
  }
}

// ============================================================================
// Tests
// ============================================================================

Deno.test("EscalateChannel: 200 concurrent requests correlate by request_id", async () => {
  const bridge = new FakeBridge();
  const channel = new EscalateChannel((msg) => bridge.write(msg));

  const N = 200;

  // Fire all N requests truly concurrently.
  const promises: Promise<EscalateOkResponse | null>[] = [];
  for (let i = 0; i < N; i++) {
    promises.push(
      channel.request({
        op: "test_correlation",
        // Stash an identifier the responder can echo back.
        _worker_idx: i,
      } as unknown as Parameters<EscalateChannel["request"]>[0]),
    );
  }

  // Wait for all requests to land in the fake bridge before responding.
  await bridge.waitForCount(N);

  // Echo responses in REVERSE capture order to surface any FIFO
  // assumption regression in the demuxer.
  const reversed = [...bridge.captured].reverse();
  for (const req of reversed) {
    channel.handleIncoming({
      rpc: "escalate_response",
      request_id: req.request_id,
      result: "ok",
      echo_idx: (req.payload._worker_idx as number),
    });
  }

  const results = await Promise.all(promises);
  for (let i = 0; i < N; i++) {
    const r = results[i];
    assert(r !== null, `request ${i} resolved to null contended sentinel`);
    const echoed = (r as unknown as Record<string, unknown>).echo_idx;
    assertEquals(
      echoed,
      i,
      `cross-talk: request ${i} got echo_idx ${echoed}`,
    );
  }
});

Deno.test("EscalateChannel: handleIncoming ignores frames without request_id", () => {
  const bridge = new FakeBridge();
  const channel = new EscalateChannel((msg) => bridge.write(msg));

  // Lifecycle frames (cmd: ...) have no rpc field, so handleIncoming
  // returns false → caller routes to the lifecycle path.
  assertEquals(
    channel.handleIncoming({ cmd: "on_pause", capability: "limited" }),
    false,
  );
  // A malformed escalate response with no request_id is consumed but
  // doesn't throw — it should not propagate to the lifecycle dispatch.
  assertEquals(
    channel.handleIncoming({ rpc: "escalate_response" }),
    true,
  );
});

Deno.test("EscalateChannel: cancelAll wakes in-flight requests with EscalateError", async () => {
  const bridge = new FakeBridge();
  const channel = new EscalateChannel((msg) => bridge.write(msg));

  const promises: Promise<unknown>[] = [];
  for (let i = 0; i < 10; i++) {
    promises.push(
      channel.request({
        op: "stranded",
        _idx: i,
      } as unknown as Parameters<EscalateChannel["request"]>[0])
        .then(() => "resolved")
        .catch((e: unknown) => {
          if (e instanceof EscalateError) return e.message;
          return `unexpected: ${e}`;
        }),
    );
  }
  await bridge.waitForCount(10);

  channel.cancelAll("test shutdown");

  const outcomes = await Promise.all(promises);
  for (const o of outcomes) {
    assertEquals(o, "test shutdown");
  }
});

Deno.test("EscalateChannel: contended response on non-allowContended request rejects", async () => {
  const bridge = new FakeBridge();
  const channel = new EscalateChannel((msg) => bridge.write(msg));

  const p = channel.request({
    op: "expects_blocking",
  } as unknown as Parameters<EscalateChannel["request"]>[0]);
  await bridge.waitForCount(1);

  channel.handleIncoming({
    rpc: "escalate_response",
    request_id: bridge.captured[0].request_id,
    result: "contended",
  });

  let caught: unknown = null;
  try {
    await p;
  } catch (e) {
    caught = e;
  }
  assert(caught instanceof EscalateError, "expected EscalateError on contended");
});

Deno.test("EscalateChannel: contended response on allowContended request resolves to null", async () => {
  const bridge = new FakeBridge();
  const channel = new EscalateChannel((msg) => bridge.write(msg));

  const p = channel.request(
    { op: "try_op" } as unknown as Parameters<EscalateChannel["request"]>[0],
    { allowContended: true },
  );
  await bridge.waitForCount(1);

  channel.handleIncoming({
    rpc: "escalate_response",
    request_id: bridge.captured[0].request_id,
    result: "contended",
  });

  const result = await p;
  assertEquals(result, null);
});

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

import { assert, assertEquals, assertStringIncludes } from "@std/assert";
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

Deno.test("EscalateChannel: writer rejection surfaces as EscalateError", async () => {
  // Wrapping non-EscalateError exceptions matches the Python SDK's
  // contract: a single error type for every channel failure mode.
  const channel = new EscalateChannel(() => {
    return Promise.reject(new Error("simulated broken pipe"));
  });

  let caught: unknown = null;
  try {
    await channel.request(
      { op: "noop" } as unknown as Parameters<EscalateChannel["request"]>[0],
    );
  } catch (e) {
    caught = e;
  }
  assert(
    caught instanceof EscalateError,
    `expected EscalateError, got ${
      caught instanceof Error ? caught.constructor.name : typeof caught
    }: ${caught}`,
  );
  assertStringIncludes((caught as EscalateError).message, "send failed");
});

Deno.test("EscalateChannel: existing EscalateError from writer passes through unwrapped", async () => {
  // If the writer already raises EscalateError (e.g. the underlying
  // bridge layer has its own typed errors), preserve it — don't double-
  // wrap. This keeps surface area uniform without burying the inner
  // EscalateError's message under a generic "send failed: …" prefix.
  const inner = new EscalateError("inner error");
  const channel = new EscalateChannel(() => Promise.reject(inner));

  let caught: unknown = null;
  try {
    await channel.request(
      { op: "noop" } as unknown as Parameters<EscalateChannel["request"]>[0],
    );
  } catch (e) {
    caught = e;
  }
  assertEquals(caught, inner);
});

Deno.test("EscalateChannel: request times out via timeoutMs", async () => {
  const bridge = new FakeBridge();
  const channel = new EscalateChannel((msg) => bridge.write(msg));

  const start = performance.now();
  let caught: unknown = null;
  try {
    await channel.request(
      { op: "blocked" } as unknown as Parameters<EscalateChannel["request"]>[0],
      { timeoutMs: 100 },
    );
  } catch (e) {
    caught = e;
  }
  const elapsed = performance.now() - start;

  assert(caught instanceof EscalateError, "expected EscalateError on timeout");
  assertStringIncludes((caught as EscalateError).message, "timed out");
  // Tolerance: timer scheduling jitter on a busy box can extend the
  // wait, but we should never finish *before* the timeout. The upper
  // bound (1000ms) guards against the timeout being silently broken
  // (e.g. someone removing the setTimeout but tests still passing
  // because the channel drops responses).
  assert(
    elapsed >= 100,
    `request returned in ${elapsed}ms — timeout fired too early`,
  );
  assert(
    elapsed < 1000,
    `request waited ${elapsed}ms — timeout much longer than expected`,
  );
});

Deno.test("EscalateChannel: late response after timeout is dropped, doesn't poison next request", async () => {
  // Race-safety: if a response arrives after `setTimeout` fires (the
  // pending entry was popped by the timeout handler), `handleIncoming`
  // looks the entry up and finds nothing — returns true but takes no
  // action. The next request must see only its own response.
  const bridge = new FakeBridge();
  const channel = new EscalateChannel((msg) => bridge.write(msg));

  // First request — times out.
  let caught: unknown = null;
  try {
    await channel.request(
      { op: "first" } as unknown as Parameters<EscalateChannel["request"]>[0],
      { timeoutMs: 50 },
    );
  } catch (e) {
    caught = e;
  }
  assert(caught instanceof EscalateError);

  await bridge.waitForCount(1);
  // Late "stale" response arrives after the timeout.
  channel.handleIncoming({
    rpc: "escalate_response",
    request_id: bridge.captured[0].request_id,
    result: "ok",
    stale: true,
  });

  // Second request should resolve to its own (fresh) response.
  const p = channel.request(
    { op: "second" } as unknown as Parameters<EscalateChannel["request"]>[0],
  );
  await bridge.waitForCount(2);
  channel.handleIncoming({
    rpc: "escalate_response",
    request_id: bridge.captured[1].request_id,
    result: "ok",
    fresh: true,
  });
  const r = await p;
  assert(r !== null);
  const rec = r as unknown as Record<string, unknown>;
  assertEquals(rec.fresh, true);
  assertEquals(rec.stale, undefined, "stale response leaked into second request");
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

// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Unit tests for the polyglot logging producer + interceptors.
 *
 * Mirrors `libs/streamlib-python/python/streamlib/tests/test_log.py` —
 * the policy choices (queue capacity, drop-oldest, heartbeat cadence,
 * line-buffering, level routing) must match between Python and Deno.
 *
 * These tests cover the in-process path only: payload shape, queue
 * drops, ContextVar propagation via AsyncLocalStorage, and the JS-level
 * interceptors (console.* + Deno.stdout/stderr.write). End-to-end via a
 * real Deno subprocess + host JSONL is covered by the Rust E2E tests in
 * `subprocess_escalate.rs::tests::deno_subprocess`.
 *
 * Every test awaits `log._resetForTests()` first — `_resetForTests` is
 * async (it shuts down any prior writer task), and a non-awaited call
 * races with the test's own `install` / `info` calls.
 */

import { assert, assertEquals, assertStringIncludes } from "@std/assert";
import * as log from "./log.ts";
import * as interceptors from "./_log_interceptors.ts";

// ============================================================================
// API surface
// ============================================================================

Deno.test("log.info produces a wire payload with op=log, source=deno", async () => {
  await log._resetForTests();
  log.info("hello", { count: 7 });
  const records = log._drainForTests();
  assertEquals(records.length, 1);
  const r = records[0];
  assertEquals(r.op, "log");
  assertEquals(r.source, "deno");
  assertEquals(r.level, "info");
  assertEquals(r.message, "hello");
  assertEquals(r.attrs.count, 7);
  assertEquals(r.intercepted, false);
  assertEquals(r.channel, null);
});

Deno.test("level routing — trace/debug/info/warn/error each map 1:1", async () => {
  await log._resetForTests();
  log.trace("t");
  log.debug("d");
  log.info("i");
  log.warn("w");
  log.error("e");
  const levels = log._drainForTests().map((r) => r.level);
  assertEquals(levels, ["trace", "debug", "info", "warn", "error"]);
});

Deno.test("source_seq is monotonic within the subprocess", async () => {
  await log._resetForTests();
  for (let i = 0; i < 50; i++) log.info(`msg-${i}`);
  const records = log._drainForTests();
  const seqs = records.map((r) => BigInt(r.source_seq));
  for (let i = 1; i < seqs.length; i++) {
    assert(
      seqs[i] > seqs[i - 1],
      `expected strict monotonic increase: seqs[${i}] (${seqs[i]}) > seqs[${i - 1}] (${seqs[i - 1]})`,
    );
  }
});

Deno.test("source_ts is ISO8601 UTC", async () => {
  await log._resetForTests();
  log.info("hi");
  const r = log._drainForTests()[0];
  // Round-trip: the ISO string must parse back to a finite millisecond
  // timestamp roughly equal to "now".
  const parsed = Date.parse(r.source_ts);
  assert(Number.isFinite(parsed), `source_ts not parseable: ${r.source_ts}`);
  assert(
    Math.abs(Date.now() - parsed) < 5_000,
    `source_ts not within 5s of now: ${r.source_ts}`,
  );
  assert(r.source_ts.endsWith("Z"), `source_ts must be UTC: ${r.source_ts}`);
});

// ============================================================================
// Processor context propagation (AsyncLocalStorage + ambient)
// ============================================================================

Deno.test("ambient processor context is read on the hot path", async () => {
  await log._resetForTests();
  log.setProcessorContext({ processorId: "pr-amb", pipelineId: "pl-amb" });
  log.info("hi");
  const r = log._drainForTests()[0];
  assertEquals(r.processor_id, "pr-amb");
  assertEquals(r.pipeline_id, "pl-amb");
});

Deno.test("AsyncLocalStorage scope overrides ambient", async () => {
  await log._resetForTests();
  log.setProcessorContext({ processorId: "pr-amb", pipelineId: "pl-amb" });

  log.runWithProcessorContext({ processorId: "pr-scoped" }, () => {
    log.info("inside");
  });

  log.info("outside");

  const records = log._drainForTests();
  assertEquals(records[0].processor_id, "pr-scoped");
  assertEquals(records[0].pipeline_id, null);
  assertEquals(records[1].processor_id, "pr-amb");
  assertEquals(records[1].pipeline_id, "pl-amb");
});

// ============================================================================
// Bounded queue — drop-oldest
// ============================================================================

Deno.test(
  "queue overflow drops oldest and surfaces drop count via test helper",
  async () => {
    await log._resetForTests();
    // Use install with a tiny capacity. Pass a stub channel so the writer
    // stays inert during the synchronous burst.
    const sentChannel = {
      logFireAndForget: () => Promise.resolve(),
    } as unknown as Parameters<typeof log.install>[0];
    await log.install(sentChannel, {
      queueCapacity: 4,
      installInterceptors: false,
    });
    // Stop the writer so records pile up.
    await log.shutdown(500);

    for (let i = 0; i < 10; i++) log.info("msg", { i });

    // Queue is capped at 4, 6 should be dropped.
    assertEquals(log._queueSizeForTests(), 4);
    assertEquals(log._dropCountForTests(), 6);

    // The 4 retained records must be the most recent (i = 6..9).
    const indices = log
      ._drainForTests()
      .map((r) => (r.attrs as { i: number }).i);
    assertEquals(indices, [6, 7, 8, 9]);
  },
);

// ============================================================================
// emitIntercepted — captured-channel records carry intercepted=true
// ============================================================================

Deno.test("emitIntercepted tags channel and intercepted=true", async () => {
  await log._resetForTests();
  log.emitIntercepted("warn", "raw line", "stdout");
  const r = log._drainForTests()[0];
  assertEquals(r.intercepted, true);
  assertEquals(r.channel, "stdout");
  assertEquals(r.level, "warn");
  assertEquals(r.message, "raw line");
});

Deno.test("emitIntercepted falls back to warn when level is unknown", async () => {
  await log._resetForTests();
  log.emitIntercepted("verbose", "x", "logging");
  assertEquals(log._drainForTests()[0].level, "warn");
});

// ============================================================================
// console.* interceptors
// ============================================================================

Deno.test("console.log routes through streamlib.log with channel=console.log", async () => {
  await log._resetForTests();
  interceptors.install();
  try {
    console.log("hi from console");
  } finally {
    interceptors.uninstall();
  }
  const r = log._drainForTests()[0];
  assertEquals(r.intercepted, true);
  assertEquals(r.channel, "console.log");
  assertEquals(r.message, "hi from console");
});

Deno.test("console levels route to matching channel + streamlib level", async () => {
  await log._resetForTests();
  interceptors.install();
  try {
    console.warn("w");
    console.error("e");
    console.info("i");
    console.debug("d");
  } finally {
    interceptors.uninstall();
  }
  const records = log._drainForTests();
  const byChannel = Object.fromEntries(
    records.map((r) => [r.channel, r.level]),
  );
  assertEquals(byChannel["console.warn"], "warn");
  assertEquals(byChannel["console.error"], "error");
  assertEquals(byChannel["console.info"], "info");
  assertEquals(byChannel["console.debug"], "debug");
});

Deno.test("console.* override is restored on uninstall", async () => {
  await log._resetForTests();
  const before = console.log;
  interceptors.install();
  const installed = console.log;
  interceptors.uninstall();
  assert(installed !== before, "interceptor should replace console.log");
  assertEquals(console.log, before, "uninstall must restore the original");
});

// ============================================================================
// Deno.stdout/stderr.write interceptors
// ============================================================================

Deno.test("Deno.stdout.write surfaces a line-buffered intercepted record", async () => {
  await log._resetForTests();
  interceptors.install();
  try {
    await Deno.stdout.write(new TextEncoder().encode("hi from stdout\n"));
  } finally {
    interceptors.uninstall();
  }
  const r = log._drainForTests().find((r) => r.channel === "stdout");
  assert(r !== undefined, "expected a stdout record");
  assertEquals(r.intercepted, true);
  assertEquals(r.message, "hi from stdout");
});

Deno.test("Deno.stderr.write surfaces a line-buffered intercepted record", async () => {
  await log._resetForTests();
  interceptors.install();
  try {
    await Deno.stderr.write(new TextEncoder().encode("hi from stderr\n"));
  } finally {
    interceptors.uninstall();
  }
  const r = log._drainForTests().find((r) => r.channel === "stderr");
  assert(r !== undefined, "expected a stderr record");
  assertEquals(r.message, "hi from stderr");
});

Deno.test("multi-line write produces one record per newline", async () => {
  await log._resetForTests();
  interceptors.install();
  try {
    await Deno.stdout.write(new TextEncoder().encode("a\nb\nc\n"));
  } finally {
    interceptors.uninstall();
  }
  const messages = log
    ._drainForTests()
    .filter((r) => r.channel === "stdout")
    .map((r) => r.message);
  assertEquals(messages, ["a", "b", "c"]);
});

Deno.test("partial trailing line buffers until next newline", async () => {
  await log._resetForTests();
  interceptors.install();
  try {
    await Deno.stdout.write(new TextEncoder().encode("part-1 "));
    // Nothing yet — no newline.
    assertEquals(
      log._drainForTests().filter((r) => r.channel === "stdout").length,
      0,
    );
    await Deno.stdout.write(new TextEncoder().encode("part-2\n"));
  } finally {
    interceptors.uninstall();
  }
  const messages = log
    ._drainForTests()
    .filter((r) => r.channel === "stdout")
    .map((r) => r.message);
  assertEquals(messages, ["part-1 part-2"]);
});

Deno.test("flush on uninstall surfaces buffered partial line", async () => {
  await log._resetForTests();
  interceptors.install();
  try {
    await Deno.stdout.write(new TextEncoder().encode("orphan-no-newline"));
  } finally {
    interceptors.uninstall();
  }
  const messages = log
    ._drainForTests()
    .filter((r) => r.channel === "stdout")
    .map((r) => r.message);
  assertEquals(messages, ["orphan-no-newline"]);
});

// ============================================================================
// install() round-trip — writer drains queued records to the channel
// ============================================================================

Deno.test(
  "writer task drains queued records via channel.logFireAndForget",
  async () => {
    await log._resetForTests();
    const sent: unknown[] = [];
    const stubChannel = {
      logFireAndForget: (payload: unknown) => {
        sent.push(payload);
        return Promise.resolve();
      },
    } as unknown as Parameters<typeof log.install>[0];

    await log.install(stubChannel, { installInterceptors: false });
    log.info("drain me", { mark: "x" });

    // Wait briefly for the writer task to pick up the record.
    const deadline = Date.now() + 1000;
    while (sent.length === 0 && Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 5));
    }
    await log.shutdown(500);

    assert(sent.length >= 1, `expected at least one drained record, got ${sent.length}`);
    const payload = sent[0] as Record<string, unknown>;
    assertEquals(payload.op, "log");
    assertEquals(payload.message, "drain me");
    assertStringIncludes(JSON.stringify(payload.attrs), '"mark":"x"');
  },
);

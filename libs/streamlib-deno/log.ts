// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Polyglot unified logging for the Deno subprocess SDK.
 *
 * Public API:
 *
 *     import { log } from "@tatolab/streamlib-deno";
 *     log.info("captured frame", { frame_index: 42 });
 *     log.error("decode failed", { error: String(e) });
 *
 * Records are serialized as `{op: "log", ...}` escalate-IPC payloads and
 * enqueued into a bounded local queue. An async writer task drains the queue
 * and fires each payload over the escalate channel — fire-and-forget, no
 * correlated response. The host handler routes into the unified JSONL pathway
 * (see `streamlib::core::logging::polyglot_sink`).
 *
 * The hot path is intentionally cheap — a struct construction plus an
 * `Array.push` (drop-oldest if full) — so `streamlib.log.info(...)` from
 * inside `process()` doesn't stall the frame loop. ISO8601 formatting and
 * JSON encoding happen on the writer task.
 *
 * Mirrors `libs/streamlib-python/python/streamlib/log.py`. Divergence between
 * the two SDKs in queue capacity, drop-oldest policy, or heartbeat cadence
 * would be a bug — see issue #444.
 */

import { AsyncLocalStorage } from "node:async_hooks";

import type { EscalateChannel } from "./escalate.ts";
import type {
  EscalateRequestLog,
  EscalateRequestLogLevel,
  EscalateRequestLogSource,
} from "./_generated_/com_streamlib_escalate_request.ts";

// ============================================================================
// Public constants
// ============================================================================

/** Default bounded-queue capacity. Oldest record is dropped when full. */
export const DEFAULT_QUEUE_CAPACITY = 65536;

/** Emit a synthetic `dropped=N` heartbeat every N accumulated drops. */
const HEARTBEAT_DROP_THRESHOLD = 1000;

/**
 * Emit a synthetic `dropped=N` heartbeat at least once per this many
 * milliseconds while drops are outstanding.
 */
const HEARTBEAT_INTERVAL_MS = 1000;

const DRAIN_TICK_MS = 5;

const SOURCE_DENO: EscalateRequestLogSource = "deno" as EscalateRequestLogSource;

export type LogLevel = "trace" | "debug" | "info" | "warn" | "error";

const VALID_LEVELS: ReadonlySet<string> = new Set([
  "trace",
  "debug",
  "info",
  "warn",
  "error",
]);

// ============================================================================
// Processor-context — read on the hot path, set by subprocess_runner via
// `runWithProcessorContext`. AsyncLocalStorage is the Deno equivalent of
// Python's contextvars — it propagates through `await` boundaries without
// requiring callers to thread the values.
// ============================================================================

interface ProcessorContext {
  pipelineId: string | null;
  processorId: string | null;
}

const _als = new AsyncLocalStorage<ProcessorContext>();

let _ambientContext: ProcessorContext = { pipelineId: null, processorId: null };

/**
 * Run `fn` with the given processor / pipeline IDs visible to every
 * `streamlib.log.*` call inside it. Use this to scope IDs to a lifecycle
 * method's async tree without having to pass them explicitly.
 */
export function runWithProcessorContext<T>(
  ctx: { pipelineId?: string | null; processorId?: string | null },
  fn: () => T,
): T {
  return _als.run(
    {
      pipelineId: ctx.pipelineId ?? null,
      processorId: ctx.processorId ?? null,
    },
    fn,
  );
}

/** Set the ambient processor/pipeline IDs (fallback when ALS is not active). */
export function setProcessorContext(
  ctx: { pipelineId?: string | null; processorId?: string | null },
): void {
  _ambientContext = {
    pipelineId: ctx.pipelineId ?? null,
    processorId: ctx.processorId ?? null,
  };
}

function readProcessorContext(): ProcessorContext {
  return _als.getStore() ?? _ambientContext;
}

// ============================================================================
// Queued record — intermediate representation between hot path and writer
// ============================================================================

interface QueuedRecord {
  level: LogLevel;
  message: string;
  attrs: Record<string, unknown>;
  intercepted: boolean;
  channel: string | null;
  pipelineId: string | null;
  processorId: string | null;
  sourceSeq: bigint;
  sourceTsMs: number;
}

// ============================================================================
// Module state — bounded queue, counters, writer task handle
// ============================================================================

let _queue: QueuedRecord[] = [];
let _queueCapacity = DEFAULT_QUEUE_CAPACITY;
let _seqCounter: bigint = 0n;

let _dropCount = 0;
let _lastHeartbeatMs = 0;

let _writerActive = false;
let _writerStop = false;
let _writerDone: Promise<void> | null = null;
let _channel: EscalateChannel | null = null;

// Cancellable sleep handle. The writer's idle wait between drain ticks
// installs a resolver here so `shutdown()` can wake it immediately and
// avoid a timer leak (the Deno test runner flags any unresolved
// setTimeout as a leak).
let _writerSleepResolve: (() => void) | null = null;
let _writerSleepTimer: number = -1;

// ============================================================================
// Hot path — build payload and enqueue
// ============================================================================

function nextSeq(): bigint {
  const seq = _seqCounter;
  _seqCounter = _seqCounter + 1n;
  return seq;
}

function emit(
  level: LogLevel,
  message: string,
  attrs: Record<string, unknown>,
  options: { intercepted?: boolean; channel?: string | null } = {},
): void {
  const ctx = readProcessorContext();
  const rec: QueuedRecord = {
    level,
    message,
    attrs,
    intercepted: options.intercepted ?? false,
    channel: options.channel ?? null,
    pipelineId: ctx.pipelineId,
    processorId: ctx.processorId,
    sourceSeq: nextSeq(),
    sourceTsMs: Date.now(),
  };
  if (_queue.length >= _queueCapacity) {
    // Drop-oldest: pop the head, increment drop counter. Matches Python's
    // bounded queue.Queue with the lossy `put_nowait` + drop counter
    // pattern — the Deno queue is single-threaded so no lock is needed.
    _queue.shift();
    _dropCount += 1;
  }
  _queue.push(rec);
}

export function trace(message: string, attrs: Record<string, unknown> = {}): void {
  emit("trace", message, attrs);
}

export function debug(message: string, attrs: Record<string, unknown> = {}): void {
  emit("debug", message, attrs);
}

export function info(message: string, attrs: Record<string, unknown> = {}): void {
  emit("info", message, attrs);
}

export function warn(message: string, attrs: Record<string, unknown> = {}): void {
  emit("warn", message, attrs);
}

export function error(message: string, attrs: Record<string, unknown> = {}): void {
  emit("error", message, attrs);
}

/**
 * Enqueue a record captured by an interceptor (stdout/stderr/console/fd*).
 * Used by `_log_interceptors`; not part of the processor-author surface.
 */
export function emitIntercepted(
  level: LogLevel | string,
  message: string,
  channel: string,
  attrs: Record<string, unknown> = {},
): void {
  const lvl = (VALID_LEVELS.has(level) ? level : "warn") as LogLevel;
  emit(lvl, message, attrs, { intercepted: true, channel });
}

// ============================================================================
// Writer — drain queue, format payload, send over escalate IPC
// ============================================================================

function formatSourceTs(sourceTsMs: number): string {
  // Date.toISOString gives millisecond precision with `Z` suffix —
  // the Python side pads ns. Millisecond precision is sufficient for
  // human ordering; the host stamps `host_ts` as the authoritative key.
  return new Date(sourceTsMs).toISOString();
}

function buildPayload(rec: QueuedRecord): EscalateRequestLog {
  return {
    op: "log",
    source: SOURCE_DENO,
    source_seq: rec.sourceSeq.toString(),
    source_ts: formatSourceTs(rec.sourceTsMs),
    level: rec.level as EscalateRequestLogLevel,
    message: rec.message,
    attrs: rec.attrs,
    intercepted: rec.intercepted,
    channel: rec.channel,
    pipeline_id: rec.pipelineId,
    processor_id: rec.processorId,
  };
}

function buildDropHeartbeat(drops: number): EscalateRequestLog {
  const ctx = readProcessorContext();
  return {
    op: "log",
    source: SOURCE_DENO,
    source_seq: nextSeq().toString(),
    source_ts: formatSourceTs(Date.now()),
    level: "warn" as EscalateRequestLogLevel,
    message: `dropped ${drops} log records (subprocess queue saturated)`,
    attrs: { dropped: drops },
    intercepted: false,
    channel: null,
    pipeline_id: ctx.pipelineId,
    processor_id: ctx.processorId,
  };
}

async function sendDirect(payload: EscalateRequestLog): Promise<boolean> {
  const channel = _channel;
  if (!channel) return true;
  try {
    await channel.logFireAndForget(payload);
    return true;
  } catch {
    return false;
  }
}

async function maybeEmitHeartbeat(): Promise<void> {
  const drops = _dropCount;
  if (drops === 0) return;
  const now = Date.now();
  const elapsed = now - _lastHeartbeatMs;
  if (drops < HEARTBEAT_DROP_THRESHOLD && elapsed < HEARTBEAT_INTERVAL_MS) {
    return;
  }
  _dropCount = 0;
  _lastHeartbeatMs = now;
  await sendDirect(buildDropHeartbeat(drops));
}

function writerSleep(ms: number): Promise<void> {
  return new Promise<void>((resolve) => {
    _writerSleepResolve = () => {
      _writerSleepResolve = null;
      if (_writerSleepTimer >= 0) {
        clearTimeout(_writerSleepTimer);
        _writerSleepTimer = -1;
      }
      resolve();
    };
    _writerSleepTimer = setTimeout(() => {
      _writerSleepResolve = null;
      _writerSleepTimer = -1;
      resolve();
    }, ms);
  });
}

function wakeWriterSleep(): void {
  if (_writerSleepResolve) {
    _writerSleepResolve();
  }
}

async function writerLoop(): Promise<void> {
  while (!_writerStop) {
    if (_queue.length === 0) {
      await maybeEmitHeartbeat();
      if (_writerStop) break;
      await writerSleep(DRAIN_TICK_MS);
      continue;
    }
    const rec = _queue.shift()!;
    if (!(await sendDirect(buildPayload(rec)))) {
      // Bridge pipe broken — subprocess is going away. Stop trying so
      // teardown isn't blocked behind a broken stdout.
      _writerStop = true;
      break;
    }
    await maybeEmitHeartbeat();
  }
  // Drain remaining records on shutdown so flush() has the expected effect.
  while (_queue.length > 0) {
    const rec = _queue.shift()!;
    if (!(await sendDirect(buildPayload(rec)))) break;
  }
  await maybeEmitHeartbeat();
  _writerActive = false;
}

// ============================================================================
// Install / shutdown — called by subprocess_runner
// ============================================================================

export interface InstallOptions {
  installInterceptors?: boolean;
  queueCapacity?: number;
}

/**
 * Start the writer task and (optionally) install subprocess-side
 * interceptors. Idempotent — a second call is a no-op.
 *
 * Called by `subprocess_runner` after the escalate channel is wired.
 */
export async function install(
  channel: EscalateChannel,
  options: InstallOptions = {},
): Promise<void> {
  if (_writerActive) return;
  _channel = channel;
  _queueCapacity = options.queueCapacity ?? DEFAULT_QUEUE_CAPACITY;
  _lastHeartbeatMs = Date.now();
  _writerStop = false;
  _writerActive = true;
  _writerDone = writerLoop();

  if (options.installInterceptors ?? true) {
    const interceptors = await import("./_log_interceptors.ts");
    interceptors.install();
  }
}

/**
 * Stop the writer task and flush remaining records. Safe to call multiple
 * times.
 */
export async function shutdown(timeoutMs = 2000): Promise<void> {
  _writerStop = true;
  // Wake the writer's idle sleep so it sees the stop flag immediately
  // — this avoids leaving a setTimeout pending past shutdown, which
  // Deno's test runner reports as a leak.
  wakeWriterSleep();
  if (_writerDone) {
    let timeoutId = -1;
    const timer = new Promise<void>((resolve) => {
      timeoutId = setTimeout(resolve, timeoutMs);
    });
    await Promise.race([_writerDone, timer]);
    if (timeoutId >= 0) clearTimeout(timeoutId);
  }
  _writerDone = null;
  _writerActive = false;
  _channel = null;
}

// ============================================================================
// Test helpers — not part of the public API
// ============================================================================

/** Reset module state between Deno.test cases. NOT for production use. */
export async function _resetForTests(): Promise<void> {
  await shutdown(500);
  _queue = [];
  _queueCapacity = DEFAULT_QUEUE_CAPACITY;
  _seqCounter = 0n;
  _dropCount = 0;
  _lastHeartbeatMs = 0;
  _writerActive = false;
  _writerStop = false;
  _writerDone = null;
  _channel = null;
  _ambientContext = { pipelineId: null, processorId: null };
}

/** Current queue depth. NOT for production use. */
export function _queueSizeForTests(): number {
  return _queue.length;
}

/** Current drop count. NOT for production use. */
export function _dropCountForTests(): number {
  return _dropCount;
}

/** Drain the queue synchronously and return all pending records' payloads. */
export function _drainForTests(): EscalateRequestLog[] {
  const out = _queue.map(buildPayload);
  _queue = [];
  return out;
}

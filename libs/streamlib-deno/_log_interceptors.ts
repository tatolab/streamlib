// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
// streamlib:lint-logging:allow-file — installs the unified pathway; must monkey-patch console.* and Deno.stdout/Deno.stderr directly

/**
 * Subprocess-side interceptors that route `console.*` output and raw
 * `Deno.stdout` / `Deno.stderr` writes through `streamlib.log`.
 *
 * Two layers, independent and additive:
 *
 * 1. `globalThis.console.{log,info,debug,warn,error}` overridden to enqueue
 *    `intercepted=true, channel="console.<level>"` records.
 * 2. `Deno.stdout.write` / `Deno.stderr.write` (and their `Sync` siblings)
 *    monkey-patched in place. Records are line-buffered — one record per
 *    `\n`; the trailing partial stays buffered until the next write or
 *    until `restoreStdio()` flushes on shutdown.
 *
 * Parent-side fd-level capture lives on the host
 * (`spawn_deno_subprocess_op.rs::__generated_setup`): it reads fd2 lines
 * and emits `intercepted=true, channel="fd2", source="deno"` directly into
 * the host JSONL pipeline. fd1 is the IPC channel and is NOT intercepted —
 * any raw write to fd1 would corrupt the bridge framing. Tracked for
 * future relocation onto a dedicated fd pair on issue #451.
 */

import * as log from "./log.ts";

let _installed = false;

// ============================================================================
// console.* overrides
// ============================================================================

interface ConsoleSlot {
  level: log.LogLevel;
  channel: string;
  original: (...args: unknown[]) => void;
}

const _consoleSlots: ConsoleSlot[] = [];

const CONSOLE_LEVEL_MAP: Array<{
  method: "log" | "info" | "debug" | "warn" | "error";
  level: log.LogLevel;
}> = [
  { method: "log", level: "info" },
  { method: "info", level: "info" },
  { method: "debug", level: "debug" },
  { method: "warn", level: "warn" },
  { method: "error", level: "error" },
];

function formatConsoleArgs(args: unknown[]): string {
  return args
    .map((arg) => {
      if (typeof arg === "string") return arg;
      if (arg instanceof Error) return arg.stack ?? arg.message;
      try {
        return JSON.stringify(arg);
      } catch {
        return String(arg);
      }
    })
    .join(" ");
}

function installConsoleOverrides(): void {
  for (const { method, level } of CONSOLE_LEVEL_MAP) {
    const original = (globalThis.console as unknown as Record<string, unknown>)[
      method
    ] as (...args: unknown[]) => void;
    _consoleSlots.push({
      level,
      channel: `console.${method}`,
      original,
    });
    (globalThis.console as unknown as Record<string, unknown>)[method] = (
      ...args: unknown[]
    ) => {
      log.emitIntercepted(level, formatConsoleArgs(args), `console.${method}`);
    };
  }
}

function restoreConsoleOverrides(): void {
  for (let i = 0; i < _consoleSlots.length; i++) {
    const slot = _consoleSlots[i];
    const method = CONSOLE_LEVEL_MAP[i].method;
    (globalThis.console as unknown as Record<string, unknown>)[method] =
      slot.original;
  }
  _consoleSlots.length = 0;
}

// ============================================================================
// Deno.stdout / Deno.stderr write monkey-patches
// ============================================================================

interface StdioWrap {
  stream: typeof Deno.stdout | typeof Deno.stderr;
  channel: "stdout" | "stderr";
  buffer: string;
  originalWrite: (p: Uint8Array) => Promise<number>;
  originalWriteSync: (p: Uint8Array) => number;
}

const _stdioWraps: StdioWrap[] = [];

const _decoder = new TextDecoder("utf-8", { fatal: false });

function bufferAndEmit(wrap: StdioWrap, bytes: Uint8Array): void {
  const text = _decoder.decode(bytes, { stream: true });
  if (text.length === 0) return;
  const combined = wrap.buffer + text;
  const newlineIdx = combined.lastIndexOf("\n");
  if (newlineIdx === -1) {
    wrap.buffer = combined;
    return;
  }
  const completeBlock = combined.slice(0, newlineIdx);
  wrap.buffer = combined.slice(newlineIdx + 1);
  for (const line of completeBlock.split("\n")) {
    log.emitIntercepted("warn", line, wrap.channel);
  }
}

function installStdioWraps(): void {
  for (
    const stream of [
      { stream: Deno.stdout, channel: "stdout" as const },
      { stream: Deno.stderr, channel: "stderr" as const },
    ]
  ) {
    const target = stream.stream as unknown as {
      write(p: Uint8Array): Promise<number>;
      writeSync(p: Uint8Array): number;
    };
    const wrap: StdioWrap = {
      stream: stream.stream,
      channel: stream.channel,
      buffer: "",
      originalWrite: target.write.bind(target),
      originalWriteSync: target.writeSync.bind(target),
    };
    _stdioWraps.push(wrap);

    target.write = (p: Uint8Array) => {
      bufferAndEmit(wrap, p);
      return Promise.resolve(p.byteLength);
    };
    target.writeSync = (p: Uint8Array) => {
      bufferAndEmit(wrap, p);
      return p.byteLength;
    };
  }
}

function flushStdioWraps(): void {
  for (const wrap of _stdioWraps) {
    if (wrap.buffer.length > 0) {
      log.emitIntercepted("warn", wrap.buffer, wrap.channel);
      wrap.buffer = "";
    }
  }
}

function restoreStdioWraps(): void {
  flushStdioWraps();
  for (const wrap of _stdioWraps) {
    const target = wrap.stream as unknown as {
      write(p: Uint8Array): Promise<number>;
      writeSync(p: Uint8Array): number;
    };
    target.write = wrap.originalWrite;
    target.writeSync = wrap.originalWriteSync;
  }
  _stdioWraps.length = 0;
}

// ============================================================================
// Install / uninstall
// ============================================================================

/**
 * Override `globalThis.console.*` and monkey-patch `Deno.stdout/stderr.write`
 * to route through `streamlib.log`. Idempotent.
 */
export function install(): void {
  if (_installed) return;
  installConsoleOverrides();
  installStdioWraps();
  _installed = true;
}

/**
 * Restore the original `console` and `Deno.stdout/stderr.write` impls.
 * Used by tests and during subprocess shutdown.
 */
export function uninstall(): void {
  if (!_installed) return;
  restoreStdioWraps();
  restoreConsoleOverrides();
  _installed = false;
}

/** Test helper: flush partial stdio buffers without uninstalling. */
export function _flushForTests(): void {
  flushStdioWraps();
}

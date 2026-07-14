// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Canonical monotonic-clock timestamp source for the Deno SDK.
 *
 * Use `monotonicNowNs()` for any timestamp that needs to be compared
 * across processes — frame stamps, escalate request IDs, log
 * correlation tokens, anything that crosses the host/subprocess boundary.
 *
 * Wraps the cdylib's `sldn_monotonic_now_ns()` which calls
 * `clock_gettime(CLOCK_MONOTONIC)` — identical to the syscall Rust's
 * `std::time::Instant::now()` makes on Linux and to what
 * `streamlib-python-native`'s `slpn_monotonic_now_ns()` does. Values
 * from all three sources share the same kernel epoch and are directly
 * comparable.
 *
 * Wall-clock APIs (`Date.now`, `performance.now`) are NOT comparable
 * across processes — `Date.now` drifts under NTP, `performance.now` is
 * relative to each process's `performance.timeOrigin`. Use them only
 * when human-readable wall-clock time is genuinely required (e.g.
 * ISO8601 log formatting).
 */

import type { NativeLib } from "./native.ts";

let _lib: NativeLib | null = null;

/**
 * Wire the native FFI lib for `monotonicNowNs()`. Called by
 * `subprocess_runner` after `loadNativeLib()`. Idempotent.
 */
export function install(lib: NativeLib): void {
  _lib = lib;
}

/** Drop the cached lib reference. Test-only. */
export function _resetForTests(): void {
  _lib = null;
}

/**
 * Current monotonic time in nanoseconds via `clock_gettime(CLOCK_MONOTONIC)`.
 *
 * Comparable across processes on the same kernel — to host Rust
 * `Instant` reads and to the Python SDK's `streamlib.monotonic_now_ns()`.
 *
 * Throws if called before `install()` has wired the native lib —
 * subprocess_runner installs it during startup so processor code can
 * call this freely.
 */
export function monotonicNowNs(): bigint {
  if (!_lib) {
    throw new Error(
      "streamlib clock not installed — call clock.install(lib) before " +
        "monotonicNowNs() (subprocess_runner does this automatically)",
    );
  }
  return _lib.symbols.sldn_monotonic_now_ns() as bigint;
}

/**
 * Drift-free periodic timer backed by `timerfd_create(CLOCK_MONOTONIC)`.
 *
 * Mirrors the Rust `LinuxTimerFdAudioClock` — first absolute deadline is
 * `now + interval`, then `TFD_TIMER_ABSTIME` repeats avoid cumulative
 * drift. `wait()` is async (Deno FFI `nonblocking: true`) so the JS
 * event loop stays responsive while a worker thread blocks on
 * `epoll_wait`. Teardown latency is bounded by the `timeoutMs` argument
 * to `wait()`.
 *
 * Linux only. Construction throws on other platforms or kernels that
 * lack CLOCK_MONOTONIC / TFD_TIMER_ABSTIME.
 */
export class MonotonicTimer {
  #handle: Deno.PointerValue;
  readonly intervalNs: bigint;

  private constructor(handle: Deno.PointerValue, intervalNs: bigint) {
    this.#handle = handle;
    this.intervalNs = intervalNs;
  }

  /**
   * Create a timer that fires every `intervalNs` nanoseconds. Throws if
   * the cdylib isn't installed (call `clock.install(lib)` first) or if
   * `timerfd_create` fails (non-Linux platform / unsupported kernel).
   */
  static create(intervalNs: bigint | number): MonotonicTimer {
    if (!_lib) {
      throw new Error(
        "streamlib clock not installed — call clock.install(lib) before " +
          "MonotonicTimer.create() (subprocess_runner does this automatically)",
      );
    }
    const ns = typeof intervalNs === "bigint" ? intervalNs : BigInt(intervalNs);
    if (ns <= 0n) {
      throw new RangeError(`intervalNs must be > 0, got ${ns}`);
    }
    const handle = _lib.symbols.sldn_timerfd_create(ns);
    if (!handle) {
      throw new Error(
        `sldn_timerfd_create(${ns}) failed — timerfd is Linux-only and ` +
          "requires a kernel that supports CLOCK_MONOTONIC + TFD_TIMER_ABSTIME",
      );
    }
    return new MonotonicTimer(handle, ns);
  }

  /**
   * Wait up to `timeoutMs` for the next tick.
   *
   * Resolves to: positive expiration count if a tick fired, 0n on
   * timeout (no tick yet — caller should poll shutdown / stdin and
   * call again), -1n on error. The default 100ms timeout bounds
   * teardown latency without spinning.
   */
  async wait(timeoutMs: number = 100): Promise<bigint> {
    if (!_lib) return -1n;
    if (!this.#handle) return -1n;
    return await _lib.symbols.sldn_timerfd_wait(this.#handle, timeoutMs) as bigint;
  }

  close(): void {
    if (this.#handle && _lib) {
      _lib.symbols.sldn_timerfd_close(this.#handle);
      this.#handle = null;
    }
  }

  [Symbol.dispose](): void {
    this.close();
  }
}

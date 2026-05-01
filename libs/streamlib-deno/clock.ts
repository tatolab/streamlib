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

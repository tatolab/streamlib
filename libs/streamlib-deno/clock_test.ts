// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Tests for the canonical monotonic-clock timestamp source.
 *
 * Loads the actual streamlib-deno-native cdylib so the test exercises
 * the real `clock_gettime(CLOCK_MONOTONIC)` syscall path, not a mock.
 */

import {
  assert,
  assertEquals,
  assertGreater,
  assertGreaterOrEqual,
  assertThrows,
} from "@std/assert";

import * as clock from "./clock.ts";
import { loadNativeLib, type NativeLib } from "./native.ts";

function resolveNativeLibPath(): string {
  const fromEnv = Deno.env.get("STREAMLIB_NATIVE_LIB_PATH");
  if (fromEnv) return fromEnv;
  // Fallback to the standard cargo target dir at workspace root. The
  // tests assume the cdylib has been built (cargo build -p
  // streamlib-deno-native).
  const cwd = Deno.cwd();
  // streamlib-deno tests run with cwd at libs/streamlib-deno; walk up.
  return `${cwd}/../../target/debug/libstreamlib_deno_native.so`;
}

function withInstalledLib(): NativeLib {
  const lib = loadNativeLib(resolveNativeLibPath());
  clock.install(lib);
  return lib;
}

Deno.test("monotonicNowNs returns a positive bigint", () => {
  const lib = withInstalledLib();
  try {
    const value = clock.monotonicNowNs();
    assertEquals(typeof value, "bigint");
    assertGreater(value, 0n);
  } finally {
    clock._resetForTests();
    lib.close();
  }
});

Deno.test("monotonicNowNs is non-decreasing across N consecutive calls", () => {
  const lib = withInstalledLib();
  try {
    const samples: bigint[] = [];
    for (let i = 0; i < 1000; i++) samples.push(clock.monotonicNowNs());
    for (let i = 1; i < samples.length; i++) {
      assertGreaterOrEqual(
        samples[i],
        samples[i - 1],
        `clock went backwards at i=${i}: ${samples[i]} < ${samples[i - 1]}`,
      );
    }
  } finally {
    clock._resetForTests();
    lib.close();
  }
});

Deno.test("monotonicNowNs advances under CPU load", () => {
  const lib = withInstalledLib();
  try {
    const start = clock.monotonicNowNs();
    let acc = 0;
    for (let i = 0; i < 100_000; i++) acc += i;
    const end = clock.monotonicNowNs();
    assertGreater(
      end - start,
      1_000n,
      `clock barely moved: ${end - start} ns`,
    );
    assert(acc > 0); // keep the optimizer honest
  } finally {
    clock._resetForTests();
    lib.close();
  }
});

Deno.test("monotonicNowNs has sub-microsecond resolution", () => {
  const lib = withInstalledLib();
  try {
    let foundSubUs = false;
    for (let i = 0; i < 1000; i++) {
      const a = clock.monotonicNowNs();
      const b = clock.monotonicNowNs();
      const delta = b - a;
      if (delta > 0n && delta < 1_000n) {
        foundSubUs = true;
        break;
      }
    }
    assert(foundSubUs, "expected at least one sub-microsecond delta");
  } finally {
    clock._resetForTests();
    lib.close();
  }
});

Deno.test("monotonicNowNs throws before install()", () => {
  // Defensive — _resetForTests should have left the module uninstalled.
  clock._resetForTests();
  assertThrows(
    () => clock.monotonicNowNs(),
    Error,
    "streamlib clock not installed",
  );
});

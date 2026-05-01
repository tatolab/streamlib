// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * StreamLib Deno SDK — TypeScript processor framework for StreamLib.
 *
 * @module
 */

// Public type exports
export type {
  ContinuousProcessor,
  GpuContextFullAccess,
  GpuContextLimitedAccess,
  GpuSurface,
  InputPorts,
  ManualProcessor,
  OutputPorts,
  ProcessorLifecycle,
  ReactiveProcessor,
  RuntimeContextFullAccess,
  RuntimeContextLimitedAccess,
} from "./types.ts";

// Public implementation exports
export {
  NativeProcessorState,
  NativeRuntimeContextFullAccess,
  NativeRuntimeContextLimitedAccess,
} from "./context.ts";
export { cString, loadNativeLib } from "./native.ts";
export type { NativeLib } from "./native.ts";

// Unified polyglot logging — see issue #444 / parent #430.
export * as log from "./log.ts";

// Canonical monotonic timestamp source — `clock_gettime(CLOCK_MONOTONIC)`.
// Use for any timestamp that crosses the host/subprocess boundary or is
// compared against another runtime's stamps. `MonotonicTimer` is the
// drift-free periodic timer (timerfd) for continuous-mode dispatch.
export { MonotonicTimer, monotonicNowNs } from "./clock.ts";
export * as clock from "./clock.ts";

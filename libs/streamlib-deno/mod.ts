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

// Schema identity + processor / port decorators (#404 / #701).
// `SchemaIdent` is the source-side authoring surface — distinct from the
// wire-side `SchemaIdentEnvelope` in `subprocess_runner.ts` (which
// parses inbound compiler IPC). `processor`/`input`/`output` mirror the
// Rust `#[streamlib::processor(...)]` macro and Python's `@processor` /
// `@input` / `@output` decorators.
export { SchemaIdent } from "./schema_ident.ts";
export type { SchemaIdentWire } from "./schema_ident.ts";
export { input, output, processor } from "./decorators.ts";
export type {
  PortMetadata,
  PortOptions,
  SchemaCarrier,
  StreamlibClassMetadata,
} from "./decorators.ts";

// Unified polyglot logging — see issue #444 / parent #430.
export * as log from "./log.ts";

// Canonical monotonic timestamp source — `clock_gettime(CLOCK_MONOTONIC)`.
// Use for any timestamp that crosses the host/subprocess boundary or is
// compared against another runtime's stamps. `MonotonicTimer` is the
// drift-free periodic timer (timerfd) for continuous-mode dispatch.
export { MonotonicTimer, monotonicNowNs } from "./clock.ts";
export * as clock from "./clock.ts";

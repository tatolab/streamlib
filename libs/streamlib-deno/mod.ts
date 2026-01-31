// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * StreamLib Deno SDK — TypeScript processor framework for StreamLib.
 *
 * @module
 */

// Public type exports
export type {
  ProcessorContext,
  InputPorts,
  OutputPorts,
  GpuContext,
  GpuSurface,
  ProcessorLifecycle,
  ReactiveProcessor,
  ContinuousProcessor,
  ManualProcessor,
} from "./types.ts";

// Public implementation exports
export { NativeProcessorContext } from "./context.ts";
export { loadNativeLib, cString } from "./native.ts";
export type { NativeLib } from "./native.ts";

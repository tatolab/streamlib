// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Processor context providing access to ports, GPU surfaces, and timing.
 */
export interface ProcessorContext {
  readonly config: Record<string, unknown>;
  readonly inputs: InputPorts;
  readonly outputs: OutputPorts;
  readonly gpu: GpuContext;
  readonly timeNs: bigint;
}

/**
 * Input port access for reading data from upstream processors.
 */
export interface InputPorts {
  /** Read and decode data from a port. Returns null if no data available. */
  read<T = unknown>(portName: string): { value: T; timestampNs: bigint } | null;

  /** Read raw msgpack-encoded bytes from a port. Returns null if no data available. */
  readRaw(portName: string): { data: Uint8Array; timestampNs: bigint } | null;
}

/**
 * Output port access for writing data to downstream processors.
 */
export interface OutputPorts {
  /** Encode value and write to a port. */
  write(portName: string, value: unknown, timestampNs: bigint): void;

  /** Write raw bytes to a port. */
  writeRaw(portName: string, data: Uint8Array, timestampNs: bigint): void;
}

/**
 * GPU context for zero-copy surface access (macOS IOSurface).
 */
export interface GpuContext {
  /** Resolve an IOSurface by its ID. */
  resolveSurface(iosurfaceId: number): GpuSurface;
}

/**
 * Handle to a GPU surface for zero-copy pixel access.
 */
export interface GpuSurface {
  readonly width: number;
  readonly height: number;
  readonly bytesPerRow: number;
  readonly surfaceId: number;

  /** Lock the surface for CPU access. */
  lock(readOnly: boolean): void;

  /** Get the surface pixel data as an ArrayBuffer (only valid while locked). */
  asBuffer(): ArrayBuffer;

  /** Unlock the surface. */
  unlock(readOnly: boolean): void;

  /** Release the surface handle. */
  release(): void;
}

// ============================================================================
// Processor lifecycle interfaces
// ============================================================================

/**
 * Base lifecycle hooks shared by all processor types.
 */
export interface ProcessorLifecycle {
  setup?(ctx: ProcessorContext): void | Promise<void>;
  teardown?(ctx: ProcessorContext): void | Promise<void>;
  onPause?(ctx: ProcessorContext): void | Promise<void>;
  onResume?(ctx: ProcessorContext): void | Promise<void>;
  updateConfig?(config: Record<string, unknown>): void | Promise<void>;
}

/**
 * Reactive processor: process() is called when input data arrives.
 */
export interface ReactiveProcessor extends ProcessorLifecycle {
  process(ctx: ProcessorContext): void | Promise<void>;
}

/**
 * Continuous processor: process() is called in a loop.
 */
export interface ContinuousProcessor extends ProcessorLifecycle {
  process(ctx: ProcessorContext): void | Promise<void>;
}

/**
 * Manual processor: start()/stop() control execution.
 */
export interface ManualProcessor extends ProcessorLifecycle {
  start(ctx: ProcessorContext): void | Promise<void>;
  stop?(ctx: ProcessorContext): void | Promise<void>;
}

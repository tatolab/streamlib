// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Input port access for reading data from upstream processors.
 */
export interface InputPorts {
  /** Read and decode data from a port. Returns null if no data available. */
  read<T = unknown>(portName: string): { value: T; timestampNs: bigint } | null;

  /** Read raw msgpack-encoded bytes from a port. Returns null if no data available. */
  readRaw(
    portName: string,
  ): { data: Uint8Array<ArrayBuffer>; timestampNs: bigint } | null;
}

/**
 * Output port access for writing data to downstream processors.
 */
export interface OutputPorts {
  /** Encode value and write to a port. */
  write(portName: string, value: unknown, timestampNs: bigint): void;

  /** Write raw bytes to a port. */
  writeRaw(
    portName: string,
    data: Uint8Array<ArrayBuffer>,
    timestampNs: bigint,
  ): void;
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

/**
 * Non-allocating GPU capability — resolve existing surfaces from upstream
 * frames. Mirrors the Rust [`GpuContextLimitedAccess`] surface.
 */
export interface GpuContextLimitedAccess {
  /** Resolve a broker pool_id to a GPU surface handle. */
  resolveSurface(poolId: string): GpuSurface;
}

/**
 * Privileged GPU capability — includes limited-access ops plus allocations.
 * Mirrors the Rust [`GpuContextFullAccess`] surface.
 */
export interface GpuContextFullAccess extends GpuContextLimitedAccess {
  /** Create a new IOSurface, register with broker, return [poolId, surface]. */
  createSurface(width: number, height: number, format: string): { poolId: string; surface: GpuSurface };
}

// ============================================================================
// Capability-typed runtime context views
// ============================================================================

interface BaseRuntimeContext {
  readonly config: Record<string, unknown>;
  readonly inputs: InputPorts;
  readonly outputs: OutputPorts;
  readonly timeNs: bigint;
}

/**
 * Restricted-capability runtime context passed to `process` / `onPause` /
 * `onResume`. Exposes [`GpuContextLimitedAccess`] only — attempting to
 * reach `gpuFullAccess` is a TypeScript compile error.
 *
 * Mirrors the Rust [`RuntimeContextLimitedAccess`] view.
 */
export interface RuntimeContextLimitedAccess extends BaseRuntimeContext {
  readonly gpuLimitedAccess: GpuContextLimitedAccess;
}

/**
 * Privileged runtime context passed to `setup` / `teardown` and Manual-mode
 * `start` / `stop`. Exposes both [`GpuContextFullAccess`] (for allocations)
 * and [`GpuContextLimitedAccess`] (so the privileged method can hand a
 * stashable limited handle to downstream workers).
 *
 * Mirrors the Rust [`RuntimeContextFullAccess`] view.
 */
export interface RuntimeContextFullAccess extends BaseRuntimeContext {
  readonly gpuLimitedAccess: GpuContextLimitedAccess;
  readonly gpuFullAccess: GpuContextFullAccess;
}

// ============================================================================
// Processor lifecycle interfaces
// ============================================================================

/**
 * Base lifecycle hooks shared by all processor types.
 */
export interface ProcessorLifecycle {
  setup?(ctx: RuntimeContextFullAccess): void | Promise<void>;
  teardown?(ctx: RuntimeContextFullAccess): void | Promise<void>;
  onPause?(ctx: RuntimeContextLimitedAccess): void | Promise<void>;
  onResume?(ctx: RuntimeContextLimitedAccess): void | Promise<void>;
  updateConfig?(config: Record<string, unknown>): void | Promise<void>;
}

/**
 * Reactive processor: process() is called when input data arrives.
 */
export interface ReactiveProcessor extends ProcessorLifecycle {
  process(ctx: RuntimeContextLimitedAccess): void | Promise<void>;
}

/**
 * Continuous processor: process() is called in a loop.
 */
export interface ContinuousProcessor extends ProcessorLifecycle {
  process(ctx: RuntimeContextLimitedAccess): void | Promise<void>;
}

/**
 * Manual processor: start()/stop() control execution.
 */
export interface ManualProcessor extends ProcessorLifecycle {
  start(ctx: RuntimeContextFullAccess): void | Promise<void>;
  stop?(ctx: RuntimeContextFullAccess): void | Promise<void>;
}

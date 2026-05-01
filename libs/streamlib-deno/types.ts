// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * # Timestamps
 *
 * For any timestamp that crosses the host/subprocess boundary or is
 * compared against another runtime's stamps — frame stamps (the
 * `timestampNs` parameter on `OutputPorts.write` is the canonical case),
 * log correlation, escalate request IDs, anything similar — use
 * `monotonicNowNs()` from `mod.ts`. It calls
 * `clock_gettime(CLOCK_MONOTONIC)`, the same kernel syscall the host
 * Rust runtime and the Python SDK make, so values share a system-wide
 * epoch and are directly comparable.
 *
 * Do NOT use `Date.now()` or `performance.now()` for cross-process
 * timestamps. `Date.now()` is wall-clock and drifts under NTP;
 * `performance.now()` is relative to the Deno process's
 * `performance.timeOrigin` (process start), so two Deno subprocesses
 * spawned at different instants get different "0" points. Wall-clock
 * APIs remain appropriate for ISO8601 formatting and other genuinely
 * human-facing display.
 */

import type { EscalateOkResponse } from "./escalate.ts";

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

  /**
   * Raw `*mut SurfaceHandle` pointer (untyped) for in-tree adapter SDKs
   * that integrate with `streamlib-deno-native` via additional `sldn_*`
   * FFI ops (e.g. `sldn_opengl_register_surface`). Customer processors
   * should use `lock` / `asBuffer` above instead. Returns `null` when
   * the surface has been released.
   */
  readonly nativeHandlePtr: Deno.PointerObject | null;
}

/**
 * Non-allocating GPU capability — resolve existing surfaces from upstream
 * frames. Mirrors the Rust [`GpuContextLimitedAccess`] surface.
 */
export interface GpuContextLimitedAccess {
  /** Resolve a surface-share pool_id to a GPU surface handle. */
  resolveSurface(poolId: string): GpuSurface;

  /**
   * The cdylib handle this view's surfaces resolve against. Used by
   * in-tree adapter SDKs to call additional `sldn_*` FFI ops without
   * re-loading the cdylib.
   */
  readonly nativeLib: NativeLibSymbols;
}

/**
 * Structural type for the FFI symbols `streamlib-deno-native` exposes.
 * Adapter SDKs can call `nativeLib.sldn_opengl_*` once `nativeLib` is in
 * hand. The full type lives in `native.ts`; this lightweight subset
 * keeps `types.ts` from importing the FFI surface directly.
 */
// deno-lint-ignore no-explicit-any
export type NativeLibSymbols = { readonly symbols: any };

/**
 * Privileged GPU capability — includes limited-access ops plus allocations.
 * Mirrors the Rust [`GpuContextFullAccess`] surface.
 */
export interface GpuContextFullAccess extends GpuContextLimitedAccess {
  /** Create a new IOSurface, register with the surface-share service, return [poolId, surface]. */
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

  /**
   * Ask the host to allocate a pixel buffer on this subprocess's behalf.
   * Returns the host-assigned `handle_id`, which can then be passed to
   * [`GpuContextLimitedAccess.resolveSurface`] for zero-copy CPU access
   * and emitted downstream as `surface_id`.
   */
  escalateAcquirePixelBuffer(
    width: number,
    height: number,
    format?: string,
  ): Promise<EscalateOkResponse>;

  /**
   * Ask the host to allocate a pooled GPU texture on this subprocess's
   * behalf. `usage` is a non-empty list drawn from `copy_src`, `copy_dst`,
   * `texture_binding`, `storage_binding`, `render_attachment`.
   */
  escalateAcquireTexture(
    width: number,
    height: number,
    format: string,
    usage: readonly string[],
  ): Promise<EscalateOkResponse>;

  /** Drop the host's strong reference to a previously-escalated handle. */
  escalateReleaseHandle(handleId: string): Promise<EscalateOkResponse>;
}

/**
 * Restricted-capability runtime context passed to `process` / `onPause` /
 * `onResume`. Exposes [`GpuContextLimitedAccess`] only — attempting to
 * reach `gpuFullAccess` is a TypeScript compile error. Allocation goes
 * through `escalateAcquirePixelBuffer` / `escalateAcquireTexture`, which
 * route to the host's [`GpuContextFullAccess`].
 *
 * Mirrors the Rust [`RuntimeContextLimitedAccess`] view.
 */
export interface RuntimeContextLimitedAccess extends BaseRuntimeContext {
  readonly gpuLimitedAccess: GpuContextLimitedAccess;
}

/**
 * Privileged runtime context passed to `setup` / `teardown` and Manual-mode
 * `start` / `stop`. Exposes both [`GpuContextFullAccess`] (for direct
 * allocations) and [`GpuContextLimitedAccess`] (so the privileged method can
 * hand a stashable limited handle to downstream workers).
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
 * Continuous processor: process() is called at the manifest's
 * declared `interval_ms` cadence. The subprocess runner paces calls
 * through a monotonic-clock timerfd (`MonotonicTimer`) — do NOT use
 * `setTimeout` inside `process()` to extend the wait, return promptly
 * and let the runner drive the next tick.
 */
export interface ContinuousProcessor extends ProcessorLifecycle {
  process(ctx: RuntimeContextLimitedAccess): void | Promise<void>;
}

/**
 * Manual processor: `start()`/`stop()` control execution.
 *
 * **`start()` MUST return promptly.** The subprocess runner reads
 * lifecycle messages on a separate concurrent task, but only
 * yielding awaits give the JS event loop a chance to deliver them.
 * A synchronous CPU loop in `start()` will hang teardown until the
 * host SIGKILLs the subprocess. Spawn an async worker for any
 * ongoing work — see `examples/polyglot-manual-source/` for the
 * canonical pattern (`MonotonicTimer.create(...)` for drift-free
 * pacing inside the worker, `stop()` flips a shutdown flag and
 * awaits the worker).
 */
export interface ManualProcessor extends ProcessorLifecycle {
  start(ctx: RuntimeContextFullAccess): void | Promise<void>;
  stop?(ctx: RuntimeContextFullAccess): void | Promise<void>;
}

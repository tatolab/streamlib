// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Native-backed capability-typed runtime context views.
 *
 * Two concrete classes mirror the Rust capability split:
 * - [`NativeRuntimeContextLimitedAccess`] — passed to `process` / `onPause`
 *   / `onResume`. Carries no `gpuFullAccess` field, so TypeScript compile
 *   errors prevent reaching privileged ops from the hot path.
 * - [`NativeRuntimeContextFullAccess`] — passed to `setup` / `teardown` /
 *   Manual-mode `start` / `stop`. Exposes both limited and full GPU views.
 */

import * as msgpack from "@msgpack/msgpack";
import type { NativeLib } from "./native.ts";
import { cString } from "./native.ts";
import type {
  GpuContextFullAccess,
  GpuContextLimitedAccess,
  GpuSurface,
  InputPorts,
  OutputPorts,
  RuntimeContextFullAccess,
  RuntimeContextLimitedAccess,
} from "./types.ts";

const MAX_PAYLOAD_SIZE = 32768;

/**
 * Shared FFI-backed state reused by both capability views for a single
 * processor lifecycle. Construction is internal — subprocess_runner builds
 * one of these per `setup` and wraps it in the appropriate view per
 * lifecycle method.
 */
export class NativeProcessorState {
  readonly config: Record<string, unknown>;
  readonly inputs: NativeInputPorts;
  readonly outputs: NativeOutputPorts;

  private lib: NativeLib;
  private ctxPtr: Deno.PointerObject;
  private brokerPtr: Deno.PointerObject | null;

  constructor(
    lib: NativeLib,
    ctxPtr: Deno.PointerObject,
    config: Record<string, unknown>,
    brokerPtr: Deno.PointerObject | null = null,
  ) {
    this.lib = lib;
    this.ctxPtr = ctxPtr;
    this.config = config;
    this.brokerPtr = brokerPtr;
    this.inputs = new NativeInputPorts(lib, ctxPtr);
    this.outputs = new NativeOutputPorts(lib, ctxPtr);
  }

  get timeNs(): bigint {
    return this.lib.symbols.sldn_context_time_ns(this.ctxPtr) as bigint;
  }

  /** Construct a full-access GPU view (allocations + resolution). */
  gpuFullAccess(): GpuContextFullAccess {
    return new NativeGpuContextFullAccess(this.lib, this.brokerPtr);
  }

  /** Construct a limited-access GPU view (resolution only). */
  gpuLimitedAccess(): GpuContextLimitedAccess {
    return new NativeGpuContextLimitedAccess(this.lib, this.brokerPtr);
  }
}

/**
 * Limited runtime context implementation. Structurally lacks `gpuFullAccess`
 * at runtime — user code cannot reach privileged ops even via `as any`.
 */
export class NativeRuntimeContextLimitedAccess
  implements RuntimeContextLimitedAccess {
  readonly config: Record<string, unknown>;
  readonly inputs: InputPorts;
  readonly outputs: OutputPorts;
  readonly gpuLimitedAccess: GpuContextLimitedAccess;

  private state: NativeProcessorState;

  constructor(state: NativeProcessorState) {
    this.state = state;
    this.config = state.config;
    this.inputs = state.inputs;
    this.outputs = state.outputs;
    this.gpuLimitedAccess = state.gpuLimitedAccess();
  }

  get timeNs(): bigint {
    return this.state.timeNs;
  }
}

/**
 * Full runtime context implementation. Exposes both GPU capability views —
 * `gpuFullAccess` for allocations, `gpuLimitedAccess` so privileged methods
 * can hand a stashable limited handle to downstream workers.
 */
export class NativeRuntimeContextFullAccess implements RuntimeContextFullAccess {
  readonly config: Record<string, unknown>;
  readonly inputs: InputPorts;
  readonly outputs: OutputPorts;
  readonly gpuLimitedAccess: GpuContextLimitedAccess;
  readonly gpuFullAccess: GpuContextFullAccess;

  private state: NativeProcessorState;

  constructor(state: NativeProcessorState) {
    this.state = state;
    this.config = state.config;
    this.inputs = state.inputs;
    this.outputs = state.outputs;
    this.gpuLimitedAccess = state.gpuLimitedAccess();
    this.gpuFullAccess = state.gpuFullAccess();
  }

  get timeNs(): bigint {
    return this.state.timeNs;
  }
}

/**
 * Input ports backed by iceoryx2 subscribers via FFI.
 */
class NativeInputPorts implements InputPorts {
  private lib: NativeLib;
  private ctxPtr: Deno.PointerObject;
  private readBuf: Uint8Array;
  private outLen: Uint32Array;
  private outTs: BigInt64Array;

  constructor(lib: NativeLib, ctxPtr: Deno.PointerObject) {
    this.lib = lib;
    this.ctxPtr = ctxPtr;
    this.readBuf = new Uint8Array(MAX_PAYLOAD_SIZE);
    this.outLen = new Uint32Array(1);
    this.outTs = new BigInt64Array(1);
  }

  read<T = unknown>(
    portName: string,
  ): { value: T; timestampNs: bigint } | null {
    const raw = this.readRaw(portName);
    if (!raw) return null;
    const value = msgpack.decode(raw.data) as T;
    return { value, timestampNs: raw.timestampNs };
  }

  readRaw(portName: string): { data: Uint8Array; timestampNs: bigint } | null {
    const portNameBuf = cString(portName);
    const outLenPtr = Deno.UnsafePointer.of(this.outLen);
    const outTsPtr = Deno.UnsafePointer.of(this.outTs);
    const readBufPtr = Deno.UnsafePointer.of(this.readBuf);

    const result = this.lib.symbols.sldn_input_read(
      this.ctxPtr,
      portNameBuf,
      readBufPtr!,
      MAX_PAYLOAD_SIZE,
      outLenPtr!,
      outTsPtr!,
    );

    if (result !== 0 || this.outLen[0] === 0) {
      return null;
    }

    const len = this.outLen[0];
    const data = new Uint8Array(len);
    data.set(this.readBuf.subarray(0, len));
    return { data, timestampNs: this.outTs[0] };
  }
}

/**
 * Output ports backed by iceoryx2 publishers via FFI.
 */
class NativeOutputPorts implements OutputPorts {
  private lib: NativeLib;
  private ctxPtr: Deno.PointerObject;

  constructor(lib: NativeLib, ctxPtr: Deno.PointerObject) {
    this.lib = lib;
    this.ctxPtr = ctxPtr;
  }

  write(portName: string, value: unknown, timestampNs: bigint): void {
    const data = msgpack.encode(value);
    this.writeRaw(portName, new Uint8Array(data), timestampNs);
  }

  writeRaw(portName: string, data: Uint8Array, timestampNs: bigint): void {
    const portNameBuf = cString(portName);
    const dataPtr = Deno.UnsafePointer.of(data);

    const result = this.lib.symbols.sldn_output_write(
      this.ctxPtr,
      portNameBuf,
      dataPtr!,
      data.length,
      timestampNs,
    );

    if (result !== 0) {
      console.error(`[streamlib-deno] Failed to write to port '${portName}'`);
    }
  }
}

/**
 * Limited GPU capability — resolve existing surfaces, no allocations.
 */
class NativeGpuContextLimitedAccess implements GpuContextLimitedAccess {
  protected lib: NativeLib;
  protected brokerPtr: Deno.PointerObject | null;

  constructor(lib: NativeLib, brokerPtr: Deno.PointerObject | null) {
    this.lib = lib;
    this.brokerPtr = brokerPtr;
  }

  resolveSurface(poolId: string): GpuSurface {
    if (this.brokerPtr) {
      // Broker-backed resolution: pool_id → XPC lookup → IOSurface
      const poolIdBuf = cString(poolId);
      const handlePtr = this.lib.symbols.sldn_broker_resolve_surface(
        this.brokerPtr,
        poolIdBuf,
      );
      if (handlePtr === null) {
        throw new Error(`Broker failed to resolve surface: ${poolId}`);
      }
      const surfaceId = this.lib.symbols.sldn_gpu_surface_get_id(handlePtr);
      return new NativeGpuSurface(this.lib, handlePtr, surfaceId);
    }

    // Fallback: treat poolId as a numeric IOSurface ID (no broker)
    const iosurfaceId = parseInt(poolId, 10);
    const handlePtr = this.lib.symbols.sldn_gpu_surface_lookup(iosurfaceId);
    if (handlePtr === null) {
      throw new Error(`IOSurface not found: ${poolId}`);
    }
    return new NativeGpuSurface(this.lib, handlePtr, iosurfaceId);
  }
}

/**
 * Full GPU capability — limited ops plus IOSurface allocation.
 */
class NativeGpuContextFullAccess extends NativeGpuContextLimitedAccess
  implements GpuContextFullAccess {
  createSurface(
    width: number,
    height: number,
    _format: string,
  ): { poolId: string; surface: GpuSurface } {
    const bytesPerElement = 4; // BGRA

    if (this.brokerPtr) {
      // Broker-backed: create IOSurface + register with broker
      const poolIdBuf = new Uint8Array(256);
      const poolIdBufPtr = Deno.UnsafePointer.of(poolIdBuf);
      const handlePtr = this.lib.symbols.sldn_broker_acquire_surface(
        this.brokerPtr,
        width,
        height,
        bytesPerElement,
        poolIdBufPtr!,
        256,
      );
      if (handlePtr === null) {
        throw new Error(`Broker failed to acquire surface: ${width}x${height}`);
      }
      const nullIdx = poolIdBuf.indexOf(0);
      const poolId = new TextDecoder().decode(
        poolIdBuf.subarray(0, nullIdx === -1 ? poolIdBuf.length : nullIdx),
      );
      const surfaceId = this.lib.symbols.sldn_gpu_surface_get_id(handlePtr);
      return {
        poolId,
        surface: new NativeGpuSurface(this.lib, handlePtr, surfaceId),
      };
    }

    // Fallback: create IOSurface without broker registration
    const handlePtr = this.lib.symbols.sldn_gpu_surface_create(
      width,
      height,
      bytesPerElement,
    );
    if (handlePtr === null) {
      throw new Error(`Failed to create IOSurface: ${width}x${height}`);
    }
    const surfaceId = this.lib.symbols.sldn_gpu_surface_get_id(handlePtr);
    const poolId = String(surfaceId);
    return {
      poolId,
      surface: new NativeGpuSurface(this.lib, handlePtr, surfaceId),
    };
  }
}

/**
 * GPU surface handle backed by IOSurface via FFI.
 */
class NativeGpuSurface implements GpuSurface {
  private lib: NativeLib;
  private handlePtr: Deno.PointerObject;
  readonly surfaceId: number;

  constructor(lib: NativeLib, handlePtr: Deno.PointerObject, surfaceId: number) {
    this.lib = lib;
    this.handlePtr = handlePtr;
    this.surfaceId = surfaceId;
  }

  get width(): number {
    return this.lib.symbols.sldn_gpu_surface_width(this.handlePtr);
  }

  get height(): number {
    return this.lib.symbols.sldn_gpu_surface_height(this.handlePtr);
  }

  get bytesPerRow(): number {
    return this.lib.symbols.sldn_gpu_surface_bytes_per_row(this.handlePtr);
  }

  lock(readOnly: boolean): void {
    const result = this.lib.symbols.sldn_gpu_surface_lock(
      this.handlePtr,
      readOnly ? 1 : 0,
    );
    if (result !== 0) {
      throw new Error(`Failed to lock IOSurface ${this.surfaceId}`);
    }
  }

  asBuffer(): ArrayBuffer {
    const baseAddr = this.lib.symbols.sldn_gpu_surface_base_address(this.handlePtr);
    if (baseAddr === null) {
      throw new Error(`IOSurface ${this.surfaceId} base address is null (not locked?)`);
    }
    const totalBytes = this.bytesPerRow * this.height;
    return Deno.UnsafePointerView.getArrayBuffer(baseAddr, totalBytes);
  }

  unlock(readOnly: boolean): void {
    this.lib.symbols.sldn_gpu_surface_unlock(
      this.handlePtr,
      readOnly ? 1 : 0,
    );
  }

  release(): void {
    this.lib.symbols.sldn_gpu_surface_release(this.handlePtr);
  }
}

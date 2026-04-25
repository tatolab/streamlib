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
import type { EscalateChannel, EscalateOkResponse } from "./escalate.ts";
import * as log from "./log.ts";
import type {
  GpuContextFullAccess,
  GpuContextLimitedAccess,
  GpuSurface,
  InputPorts,
  OutputPorts,
  RuntimeContextFullAccess,
  RuntimeContextLimitedAccess,
} from "./types.ts";

/**
 * Default read-buffer capacity when the host sends no per-input
 * `max_payload_bytes`. Matches Rust's `streamlib_ipc_types::MAX_PAYLOAD_SIZE`.
 */
export const DEFAULT_READ_BUF_BYTES = 65536;

/**
 * Size the input read buffer to the largest per-port `max_payload_bytes` the
 * host declared, with [`DEFAULT_READ_BUF_BYTES`] as a floor. A fixed smaller
 * buffer silently truncates payloads larger than it — including encoded video
 * frames, which can be arbitrarily large depending on how the schema is
 * configured.
 */
export function computeReadBufBytes(
  inputs: readonly { max_payload_bytes?: number }[],
): number {
  return inputs.reduce(
    (acc, port) => Math.max(acc, port.max_payload_bytes ?? 0),
    DEFAULT_READ_BUF_BYTES,
  );
}

/**
 * Build the return value of a single FFI read given the output-parameter state
 * after `sldn_input_read` has populated it. Extracted for testing so the
 * empty / happy / truncated branches can be exercised without spinning up
 * iceoryx2.
 *
 * Returns `null` when the read produced no data or when the payload the native
 * side reported exceeds the caller's read buffer (truncation). Otherwise
 * returns an owned copy of the first `outLen[0]` bytes of `readBuf`.
 */
export function decodeReadResult(
  readBuf: Uint8Array<ArrayBuffer>,
  outLen: Uint32Array<ArrayBuffer>,
  outTs: BigInt64Array<ArrayBuffer>,
  readBufBytes: number,
  portName: string,
): { data: Uint8Array<ArrayBuffer>; timestampNs: bigint } | null {
  if (outLen[0] === 0) {
    return null;
  }
  const len = outLen[0];
  if (len > readBufBytes) {
    log.warn("payload truncated on port", {
      port: portName,
      reported_bytes: len,
      read_buf_bytes: readBufBytes,
    });
    return null;
  }
  const data = new Uint8Array(new ArrayBuffer(len));
  data.set(readBuf.subarray(0, len));
  return { data, timestampNs: outTs[0] };
}

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
  readonly escalate: EscalateChannel | null;

  private lib: NativeLib;
  private ctxPtr: Deno.PointerObject;
  private surfaceHandlePtr: Deno.PointerObject | null;

  constructor(
    lib: NativeLib,
    ctxPtr: Deno.PointerObject,
    config: Record<string, unknown>,
    surfaceHandlePtr: Deno.PointerObject | null = null,
    escalate: EscalateChannel | null = null,
    readBufBytes: number = DEFAULT_READ_BUF_BYTES,
  ) {
    this.lib = lib;
    this.ctxPtr = ctxPtr;
    this.config = config;
    this.surfaceHandlePtr = surfaceHandlePtr;
    this.inputs = new NativeInputPorts(lib, ctxPtr, readBufBytes);
    this.outputs = new NativeOutputPorts(lib, ctxPtr);
    this.escalate = escalate;
  }

  /**
   * Ask the host to acquire a new-shape pixel buffer on the subprocess's
   * behalf. Throws if no escalate channel is wired (i.e. outside the
   * subprocess_runner lifecycle).
   */
  async escalateAcquirePixelBuffer(
    width: number,
    height: number,
    format = "bgra",
  ): Promise<EscalateOkResponse> {
    if (!this.escalate) {
      throw new Error(
        "escalate channel not installed — escalateAcquirePixelBuffer is only available inside the subprocess lifecycle",
      );
    }
    return this.escalate.acquirePixelBuffer(width, height, format);
  }

  /**
   * Ask the host to acquire a pooled GPU texture on the subprocess's behalf.
   * `usage` is a non-empty list of tokens drawn from `copy_src`, `copy_dst`,
   * `texture_binding`, `storage_binding`, `render_attachment`.
   */
  async escalateAcquireTexture(
    width: number,
    height: number,
    format: string,
    usage: readonly string[],
  ): Promise<EscalateOkResponse> {
    if (!this.escalate) {
      throw new Error(
        "escalate channel not installed — escalateAcquireTexture is only available inside the subprocess lifecycle",
      );
    }
    return this.escalate.acquireTexture(width, height, format, usage);
  }

  /** Drop the host's strong reference to a previously-escalated handle. */
  escalateReleaseHandle(handleId: string): Promise<EscalateOkResponse> {
    if (!this.escalate) {
      throw new Error(
        "escalate channel not installed — escalateReleaseHandle is only available inside the subprocess lifecycle",
      );
    }
    return this.escalate.releaseHandle(handleId);
  }

  get timeNs(): bigint {
    return this.lib.symbols.sldn_context_time_ns(this.ctxPtr) as bigint;
  }

  /** Construct a full-access GPU view (allocations + resolution). */
  gpuFullAccess(): GpuContextFullAccess {
    return new NativeGpuContextFullAccess(this.lib, this.surfaceHandlePtr);
  }

  /** Construct a limited-access GPU view (resolution only). */
  gpuLimitedAccess(): GpuContextLimitedAccess {
    return new NativeGpuContextLimitedAccess(this.lib, this.surfaceHandlePtr);
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

  escalateAcquirePixelBuffer(
    width: number,
    height: number,
    format = "bgra",
  ): Promise<EscalateOkResponse> {
    return this.state.escalateAcquirePixelBuffer(width, height, format);
  }

  escalateAcquireTexture(
    width: number,
    height: number,
    format: string,
    usage: readonly string[],
  ): Promise<EscalateOkResponse> {
    return this.state.escalateAcquireTexture(width, height, format, usage);
  }

  escalateReleaseHandle(handleId: string): Promise<EscalateOkResponse> {
    return this.state.escalateReleaseHandle(handleId);
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

  escalateAcquirePixelBuffer(
    width: number,
    height: number,
    format = "bgra",
  ): Promise<EscalateOkResponse> {
    return this.state.escalateAcquirePixelBuffer(width, height, format);
  }

  escalateAcquireTexture(
    width: number,
    height: number,
    format: string,
    usage: readonly string[],
  ): Promise<EscalateOkResponse> {
    return this.state.escalateAcquireTexture(width, height, format, usage);
  }

  escalateReleaseHandle(handleId: string): Promise<EscalateOkResponse> {
    return this.state.escalateReleaseHandle(handleId);
  }
}

/**
 * Input ports backed by iceoryx2 subscribers via FFI.
 */
class NativeInputPorts implements InputPorts {
  private lib: NativeLib;
  private ctxPtr: Deno.PointerObject;
  private readBuf: Uint8Array<ArrayBuffer>;
  private readBufBytes: number;
  private outLen: Uint32Array<ArrayBuffer>;
  private outTs: BigInt64Array<ArrayBuffer>;

  constructor(lib: NativeLib, ctxPtr: Deno.PointerObject, readBufBytes: number) {
    this.lib = lib;
    this.ctxPtr = ctxPtr;
    this.readBufBytes = readBufBytes;
    this.readBuf = new Uint8Array(new ArrayBuffer(readBufBytes));
    this.outLen = new Uint32Array(new ArrayBuffer(4));
    this.outTs = new BigInt64Array(new ArrayBuffer(8));
  }

  read<T = unknown>(
    portName: string,
  ): { value: T; timestampNs: bigint } | null {
    const raw = this.readRaw(portName);
    if (!raw) return null;
    const value = msgpack.decode(raw.data) as T;
    return { value, timestampNs: raw.timestampNs };
  }

  readRaw(
    portName: string,
  ): { data: Uint8Array<ArrayBuffer>; timestampNs: bigint } | null {
    const portNameBuf = cString(portName);
    const outLenPtr = Deno.UnsafePointer.of(this.outLen);
    const outTsPtr = Deno.UnsafePointer.of(this.outTs);
    const readBufPtr = Deno.UnsafePointer.of(this.readBuf);

    const result = this.lib.symbols.sldn_input_read(
      this.ctxPtr,
      portNameBuf,
      readBufPtr!,
      this.readBufBytes,
      outLenPtr!,
      outTsPtr!,
    );

    if (result !== 0) {
      return null;
    }
    return decodeReadResult(
      this.readBuf,
      this.outLen,
      this.outTs,
      this.readBufBytes,
      portName,
    );
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
    const encoded = msgpack.encode(value);
    const buf = new Uint8Array(new ArrayBuffer(encoded.byteLength));
    buf.set(encoded);
    this.writeRaw(portName, buf, timestampNs);
  }

  writeRaw(
    portName: string,
    data: Uint8Array<ArrayBuffer>,
    timestampNs: bigint,
  ): void {
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
      log.error("Failed to write to port", { port: portName });
    }
  }
}

/**
 * Limited GPU capability — resolve existing surfaces, no allocations.
 */
class NativeGpuContextLimitedAccess implements GpuContextLimitedAccess {
  protected lib: NativeLib;
  protected surfaceHandlePtr: Deno.PointerObject | null;

  constructor(lib: NativeLib, surfaceHandlePtr: Deno.PointerObject | null) {
    this.lib = lib;
    this.surfaceHandlePtr = surfaceHandlePtr;
  }

  resolveSurface(poolId: string): GpuSurface {
    if (this.surfaceHandlePtr) {
      // Surface-share-backed resolution: pool_id → XPC lookup → IOSurface
      const poolIdBuf = cString(poolId);
      const handlePtr = (this.lib.symbols.sldn_surface_resolve_surface ?? this.lib.symbols.sldn_broker_resolve_surface)!(
        this.surfaceHandlePtr,
        poolIdBuf,
      );
      if (handlePtr === null) {
        throw new Error(`Surface-share service failed to resolve surface: ${poolId}`);
      }
      const surfaceId = this.lib.symbols.sldn_gpu_surface_get_id(handlePtr);
      return new NativeGpuSurface(this.lib, handlePtr, surfaceId);
    }

    // Fallback: treat poolId as a numeric IOSurface ID (no surface-share service)
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

    if (this.surfaceHandlePtr) {
      // Surface-share-backed: create IOSurface + register with service
      const poolIdBuf = new Uint8Array(256);
      const poolIdBufPtr = Deno.UnsafePointer.of(poolIdBuf);
      const handlePtr = (this.lib.symbols.sldn_surface_acquire_surface ?? this.lib.symbols.sldn_broker_acquire_surface)!(
        this.surfaceHandlePtr,
        width,
        height,
        bytesPerElement,
        poolIdBufPtr!,
        256,
      );
      if (handlePtr === null) {
        throw new Error(`Surface-share service failed to acquire surface: ${width}x${height}`);
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

    // Fallback: create IOSurface without surface-share registration
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

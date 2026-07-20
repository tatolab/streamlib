// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Unit tests for the subprocess read path's grow-and-retry buffer sizing (#1421).
 *
 * Under PowerOfTwo publisher growth a frame can exceed any fixed receive
 * buffer; the native `sldn_input_read` reports the required size and holds the
 * frame, and the caller grows and retries. These tests cover:
 *
 *   A) `decodeReadResult` — the pure post-FFI decode step for a fitting read.
 *   B) `NativeInputPorts.readRaw` — the grow-and-retry loop that resizes on
 *      `SLDN_READ_NEEDS_LARGER_BUFFER` and delivers the oversized frame intact
 *      (parity with the Python `test_read_raw_grows_and_delivers_oversized_frame`
 *      loop test) — driven with a fake native lib, no iceoryx2, no subprocess.
 */

import { assertEquals } from "@std/assert";
import type { NativeLib } from "./native.ts";
import {
  decodeReadResult,
  DEFAULT_READ_BUF_BYTES,
  NativeProcessorState,
  SLDN_READ_NEEDS_LARGER_BUFFER,
} from "./context.ts";

/** Build the scratch state a decode call reads from. */
function makeFfiResult(
  readBufBytes: number,
  data: Uint8Array,
  timestampNs: bigint,
): {
  readBuf: Uint8Array<ArrayBuffer>;
  outLen: Uint32Array<ArrayBuffer>;
  outTs: BigInt64Array<ArrayBuffer>;
} {
  const readBuf = new Uint8Array(new ArrayBuffer(readBufBytes));
  const copyLen = Math.min(data.length, readBufBytes);
  readBuf.set(data.subarray(0, copyLen));
  const outLen = new Uint32Array(new ArrayBuffer(4));
  outLen[0] = data.length;
  const outTs = new BigInt64Array(new ArrayBuffer(8));
  outTs[0] = timestampNs;
  return { readBuf, outLen, outTs };
}

/** Build a `size`-byte buffer with a deterministic, non-trivial pattern. */
function patternBytes(size: number): Uint8Array {
  const out = new Uint8Array(size);
  for (let i = 0; i < size; i++) {
    out[i] = i % 251; // prime modulus
  }
  return out;
}

// ============================================================================
// decodeReadResult — pure decode of a fitting read
// ============================================================================

Deno.test("decodeReadResult: zero-length read returns null", () => {
  const { readBuf, outLen, outTs } = makeFfiResult(
    DEFAULT_READ_BUF_BYTES,
    new Uint8Array(0),
    123n,
  );
  assertEquals(
    decodeReadResult(readBuf, outLen, outTs, DEFAULT_READ_BUF_BYTES, "p"),
    null,
  );
});

for (
  const dataLen of [1, 1024, 32 * 1024, DEFAULT_READ_BUF_BYTES]
) {
  Deno.test(`decodeReadResult: ${dataLen}B fitting read returns bytes`, () => {
    const payload = patternBytes(dataLen);
    const { readBuf, outLen, outTs } = makeFfiResult(
      DEFAULT_READ_BUF_BYTES,
      payload,
      77n,
    );
    const result = decodeReadResult(
      readBuf,
      outLen,
      outTs,
      DEFAULT_READ_BUF_BYTES,
      "p",
    );
    assertEquals(result?.data, payload);
    assertEquals(result?.timestampNs, 77n);
  });
}

Deno.test("decodeReadResult: still-too-large len is treated as no data (grow-and-retry owns the resize)", () => {
  // The caller resizes on SLDN_READ_NEEDS_LARGER_BUFFER before decode; a
  // len > readBufBytes reaching decode would be a bug, so it yields null
  // rather than a truncated/corrupt payload.
  const payload = patternBytes(DEFAULT_READ_BUF_BYTES + 1);
  const { readBuf, outLen, outTs } = makeFfiResult(
    DEFAULT_READ_BUF_BYTES,
    payload,
    9n,
  );
  assertEquals(
    decodeReadResult(readBuf, outLen, outTs, DEFAULT_READ_BUF_BYTES, "p"),
    null,
  );
});

// ============================================================================
// B) NativeInputPorts.readRaw — grow-and-retry loop (parity with Python)
// ============================================================================

/** The subset of `NativeInputPorts` scratch state a fake native writes into. */
interface InputPortsScratchState {
  readBuf: Uint8Array<ArrayBuffer>;
  readBufBytes: number;
  outLen: Uint32Array<ArrayBuffer>;
  outTs: BigInt64Array<ArrayBuffer>;
}

/**
 * A stand-in native lib whose `sldn_input_read` first reports the frame is too
 * big (`SLDN_READ_NEEDS_LARGER_BUFFER`, outLen = full size) and, once the
 * caller's buffer has grown large enough, copies the payload and returns 0.
 *
 * Deno FFI pointers are opaque for writing from JS (`Deno.UnsafePointerView` is
 * read-only), so — unlike Python's ctypes fake writing through `out_buf` — this
 * fake writes into the bound port's scratch typed arrays directly, which is the
 * same memory those out-pointers address. Mirrors the real native contract: the
 * oversized frame is held across the two calls, so the second read delivers it.
 */
class FakeReadNativeLib {
  private scratch: InputPortsScratchState | null = null;

  constructor(
    private readonly payload: Uint8Array,
    private readonly timestampNs: bigint,
  ) {}

  /** Bind the port instance whose scratch out-params this fake fills. */
  bind(inputs: unknown): void {
    this.scratch = inputs as InputPortsScratchState;
  }

  readonly symbols = {
    sldn_input_read: (
      _ctx: unknown,
      _port: unknown,
      _outBuf: unknown,
      bufLen: number,
      _outLen: unknown,
      _outTs: unknown,
    ): number => {
      const scratch = this.scratch!;
      const required = this.payload.length;
      scratch.outLen[0] = required;
      if (required > bufLen) {
        return SLDN_READ_NEEDS_LARGER_BUFFER;
      }
      scratch.readBuf.set(this.payload);
      scratch.outTs[0] = this.timestampNs;
      return 0;
    },
  };
}

/** A dummy non-null context pointer; the fake native ignores it. */
function dummyCtxPtr(): Deno.PointerObject {
  return Deno.UnsafePointer.of(new Uint8Array(8))!;
}

for (
  const dataLen of [
    DEFAULT_READ_BUF_BYTES + 1, // one byte over the starting buffer
    256 * 1024, // a 256 KiB grown frame
    4 * 1024 * 1024, // a 4 MiB keyframe-sized frame
  ]
) {
  // Needs `--allow-ffi`: readRaw creates FFI pointers over its scratch buffers
  // on every attempt (exercising the real native call path).
  Deno.test(`readRaw: ${dataLen}B oversized frame grows and is delivered intact`, () => {
    // Fail-without-fix: revert `readRaw` to a single fixed-buffer read (no
    // SLDN_READ_NEEDS_LARGER_BUFFER handling) and this oversized frame is
    // dropped (returns null), failing the byte-for-byte assertion.
    const payload = patternBytes(dataLen);
    const fake = new FakeReadNativeLib(payload, 4242n);
    const state = new NativeProcessorState(
      fake as unknown as NativeLib,
      dummyCtxPtr(),
      {},
    );
    fake.bind(state.inputs);

    const raw = state.inputs.readRaw("video_in");

    assertEquals(raw?.data, payload, "grown frame must be delivered byte-for-byte");
    assertEquals(raw?.timestampNs, 4242n);
    assertEquals(
      (state.inputs as unknown as InputPortsScratchState).readBufBytes >= dataLen,
      true,
      "buffer must have grown to fit",
    );
  });
}

// Needs `--allow-ffi` for the same reason as the grow tests above.
Deno.test("readRaw: native no-data return yields null", () => {
  const noData = {
    symbols: {
      sldn_input_read: (
        _ctx: unknown,
        _port: unknown,
        _outBuf: unknown,
        _bufLen: number,
        _outLen: unknown,
        _outTs: unknown,
      ): number => 1, // native "no data available"
    },
  };
  const state = new NativeProcessorState(
    noData as unknown as NativeLib,
    dummyCtxPtr(),
    {},
  );
  assertEquals(state.inputs.readRaw("p"), null);
});

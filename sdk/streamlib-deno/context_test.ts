// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Unit tests for the subprocess read path's grow-and-retry decode (#1421).
 *
 * Under PowerOfTwo publisher growth a frame can exceed any fixed receive
 * buffer; the native `sldn_input_read` reports the required size and holds the
 * frame, and the caller grows and retries. These pure tests exercise
 * `decodeReadResult` (a fitting read) without spinning up iceoryx2 or a
 * subprocess; the grow loop itself lives on `NativeInputPorts.readRaw` and is
 * covered end-to-end by the Rust integration test.
 */

import { assertEquals } from "@std/assert";
import {
  decodeReadResult,
  DEFAULT_READ_BUF_BYTES,
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

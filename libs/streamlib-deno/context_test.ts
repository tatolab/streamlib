// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Unit tests for buffer-sizing + truncation-detection logic.
 *
 * These cover every branch of the read-path size check:
 *
 *   A) `computeReadBufBytes` — picks the largest declared input size with a
 *      default floor. Happy paths for every kind of input shape a host might
 *      emit (empty, missing field, below default, above default, multiple).
 *
 *   B) `decodeReadResult` — the pure post-FFI decode step. Runs the full
 *      matrix of (data_len, read_buf_bytes) cases to confirm:
 *        - Zero-length reads return null without logging.
 *        - Reads where `data_len <= read_buf_bytes` return the first
 *          `data_len` bytes of the buffer exactly, with the reported
 *          timestamp.
 *        - Reads where `data_len > read_buf_bytes` (the truncation case the
 *          pre-fix 32 KB hard-coded buffer triggered) return null and log a
 *          descriptive error.
 *
 * The iceoryx2 / FFI wire itself is covered by the Rust integration test
 * `test_frame_header_plus_256kb_roundtrip_through_slice_service`; this suite
 * is deliberately pure so it runs without spawning a subprocess or loading
 * the cdylib.
 */

import { assert, assertEquals, assertStringIncludes } from "@std/assert";
import {
  computeReadBufBytes,
  decodeReadResult,
  DEFAULT_READ_BUF_BYTES,
} from "./context.ts";

// ============================================================================
// Helpers
// ============================================================================

/**
 * Build the scratch state `NativeInputPorts` owns: a read buffer, a one-slot
 * Uint32Array for the reported length, and a one-slot BigInt64Array for the
 * timestamp. Simulates an FFI read completing by writing `data` into the
 * first `data.length` bytes of the buffer and populating the length/timestamp
 * the way `sldn_input_read` does — including reporting the original
 * (pre-truncation) length when `data` is larger than `readBufBytes`.
 */
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
  // Native does `copy_nonoverlapping(data, out_buf, min(data.len, buf_len))`.
  const copyLen = Math.min(data.length, readBufBytes);
  readBuf.set(data.subarray(0, copyLen));
  const outLen = new Uint32Array(new ArrayBuffer(4));
  // Native writes the original data.len, not copyLen — that's what makes the
  // `data_len > read_buf_bytes` branch observable to the caller.
  outLen[0] = data.length;
  const outTs = new BigInt64Array(new ArrayBuffer(8));
  outTs[0] = timestampNs;
  return { readBuf, outLen, outTs };
}

/** Build a `data_len`-byte buffer with a deterministic, non-trivial pattern. */
function patternBytes(size: number): Uint8Array {
  const out = new Uint8Array(size);
  for (let i = 0; i < size; i++) {
    out[i] = i % 251; // prime modulus — non-trivial, easy to regenerate
  }
  return out;
}

/**
 * Capture stderr writes for the duration of `fn` so we can assert that
 * truncation paths log without polluting the test runner output.
 */
function withStderrCapture(fn: () => void): string {
  const buffered: string[] = [];
  const original = console.error;
  console.error = (...args: unknown[]) => {
    buffered.push(args.map((a) => String(a)).join(" "));
  };
  try {
    fn();
  } finally {
    console.error = original;
  }
  return buffered.join("\n");
}

// ============================================================================
// A) computeReadBufBytes — host-declared size derivation
// ============================================================================

Deno.test("computeReadBufBytes: no inputs returns default floor", () => {
  assertEquals(computeReadBufBytes([]), DEFAULT_READ_BUF_BYTES);
});

Deno.test("computeReadBufBytes: input missing max_payload_bytes falls back to default", () => {
  assertEquals(
    computeReadBufBytes([{}]),
    DEFAULT_READ_BUF_BYTES,
  );
});

Deno.test("computeReadBufBytes: declared size below default clamps up to default", () => {
  // A schema may legitimately declare something small (say 16 KB for an
  // audio-only port). We still ceiling the buffer at the default so shared
  // code paths have a consistent minimum.
  const smallSize = 16 * 1024;
  assert(smallSize < DEFAULT_READ_BUF_BYTES);
  assertEquals(
    computeReadBufBytes([{ max_payload_bytes: smallSize }]),
    DEFAULT_READ_BUF_BYTES,
  );
});

Deno.test("computeReadBufBytes: declared size equal to default returns default", () => {
  assertEquals(
    computeReadBufBytes([{ max_payload_bytes: DEFAULT_READ_BUF_BYTES }]),
    DEFAULT_READ_BUF_BYTES,
  );
});

Deno.test("computeReadBufBytes: declared size above default wins", () => {
  const oneMb = 1 * 1024 * 1024;
  assertEquals(
    computeReadBufBytes([{ max_payload_bytes: oneMb }]),
    oneMb,
  );
});

Deno.test("computeReadBufBytes: multi-input picks the max across ports", () => {
  const small = 16 * 1024;
  const medium = 128 * 1024;
  const large = 512 * 1024;
  assertEquals(
    computeReadBufBytes([
      { max_payload_bytes: small },
      {},
      { max_payload_bytes: medium },
      { max_payload_bytes: large },
    ]),
    large,
  );
});

Deno.test("computeReadBufBytes: multi-input all below default clamps to default", () => {
  assertEquals(
    computeReadBufBytes([
      { max_payload_bytes: 1024 },
      { max_payload_bytes: 8192 },
      { max_payload_bytes: 16384 },
    ]),
    DEFAULT_READ_BUF_BYTES,
  );
});

// ============================================================================
// B) decodeReadResult — post-FFI decode matrix
// ============================================================================

Deno.test("decodeReadResult: zero-length read returns null without logging", () => {
  const { readBuf, outLen, outTs } = makeFfiResult(
    DEFAULT_READ_BUF_BYTES,
    new Uint8Array(0),
    123n,
  );
  const log = withStderrCapture(() => {
    const result = decodeReadResult(
      readBuf,
      outLen,
      outTs,
      DEFAULT_READ_BUF_BYTES,
      "port_a",
    );
    assertEquals(result, null);
  });
  assertEquals(log, "");
});

// Happy paths — parameterize over a matrix of (read_buf_bytes, data_len)
// chosen to exercise several boundary conditions:
//
//   - 1 KB data in a default-sized buffer          (tiny payload, default buf)
//   - 32 KB data in a default-sized buffer         (former hard-coded limit; must still work)
//   - 32 KB + 1 byte data in a default-sized buffer (proves old cap is gone)
//   - `DEFAULT_READ_BUF_BYTES` exactly in a default buffer  (boundary)
//   - 256 KB data in a 1 MB buffer                 (grown buffer via schema)
//   - 1 MB data in a 1 MB buffer                   (exact fit at the top end)
const happyPathMatrix: {
  label: string;
  readBufBytes: number;
  dataLen: number;
}[] = [
  {
    label: "1 KB data in default-sized buffer",
    readBufBytes: DEFAULT_READ_BUF_BYTES,
    dataLen: 1024,
  },
  {
    label: "32 KB data in default-sized buffer",
    readBufBytes: DEFAULT_READ_BUF_BYTES,
    dataLen: 32 * 1024,
  },
  {
    label: "32 KB + 1 B in default-sized buffer",
    readBufBytes: DEFAULT_READ_BUF_BYTES,
    dataLen: 32 * 1024 + 1,
  },
  {
    label: "exact-default in default-sized buffer",
    readBufBytes: DEFAULT_READ_BUF_BYTES,
    dataLen: DEFAULT_READ_BUF_BYTES,
  },
  {
    label: "256 KB data in 1 MB buffer",
    readBufBytes: 1024 * 1024,
    dataLen: 256 * 1024,
  },
  {
    label: "1 MB data in 1 MB buffer (exact fit)",
    readBufBytes: 1024 * 1024,
    dataLen: 1024 * 1024,
  },
];

for (const { label, readBufBytes, dataLen } of happyPathMatrix) {
  Deno.test(`decodeReadResult: happy path — ${label}`, () => {
    const data = patternBytes(dataLen);
    const ts = BigInt(dataLen) * 1000n;
    const { readBuf, outLen, outTs } = makeFfiResult(readBufBytes, data, ts);

    const log = withStderrCapture(() => {
      const result = decodeReadResult(
        readBuf,
        outLen,
        outTs,
        readBufBytes,
        "happy_port",
      );
      assert(result !== null, "happy-path read must return a value");
      assertEquals(result.data.length, dataLen);
      assertEquals(
        result.data,
        data,
        "decoded bytes should match source payload byte-for-byte",
      );
      assertEquals(result.timestampNs, ts);
      // The returned buffer must be an independent allocation so mutating the
      // scratch readBuf afterwards can't corrupt it.
      assert(
        result.data.buffer !== readBuf.buffer,
        "returned Uint8Array should own its own ArrayBuffer",
      );
    });
    assertEquals(log, "", "happy path must not log truncation warnings");
  });
}

// Truncation paths — native reported more bytes than the read buffer can hold.
// This is the exact shape the pre-fix 32 KB hard-coded buffer triggered when
// a publisher sent encoded-video-sized frames.
const truncationMatrix: {
  label: string;
  readBufBytes: number;
  dataLen: number;
}[] = [
  {
    label: "1 B over default",
    readBufBytes: DEFAULT_READ_BUF_BYTES,
    dataLen: DEFAULT_READ_BUF_BYTES + 1,
  },
  {
    label: "old 32 KB buffer vs 65 KB payload",
    readBufBytes: 32 * 1024,
    dataLen: DEFAULT_READ_BUF_BYTES,
  },
  {
    label: "256 KB payload in default 64 KB buffer",
    readBufBytes: DEFAULT_READ_BUF_BYTES,
    dataLen: 256 * 1024,
  },
  {
    label: "1 MB payload in 512 KB buffer",
    readBufBytes: 512 * 1024,
    dataLen: 1024 * 1024,
  },
];

for (const { label, readBufBytes, dataLen } of truncationMatrix) {
  Deno.test(`decodeReadResult: truncation — ${label}`, () => {
    const data = patternBytes(dataLen);
    const { readBuf, outLen, outTs } = makeFfiResult(readBufBytes, data, 42n);

    const log = withStderrCapture(() => {
      const result = decodeReadResult(
        readBuf,
        outLen,
        outTs,
        readBufBytes,
        "truncated_port",
      );
      assertEquals(
        result,
        null,
        "truncation must surface as null, not a short/corrupt payload",
      );
    });
    assertStringIncludes(
      log,
      "payload truncated on port 'truncated_port'",
      "truncation must log a descriptive error identifying the port",
    );
    assertStringIncludes(log, String(dataLen));
    assertStringIncludes(log, String(readBufBytes));
  });
}

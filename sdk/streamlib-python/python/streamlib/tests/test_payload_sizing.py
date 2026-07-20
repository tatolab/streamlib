# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Unit tests for the subprocess read path's grow-and-retry buffer sizing (#1421).

Under PowerOfTwo publisher growth a frame can exceed any fixed receive buffer.
The native `slpn_input_read` then returns `SLPN_READ_NEEDS_LARGER_BUFFER` with
`out_len` set to the required size and holds the frame; `NativeInputs._read_raw`
grows its buffer and reads again, so nothing is dropped. These tests drive that
loop with a fake native lib — no iceoryx2, no subprocess.

  A) ``decode_read_result`` — the pure post-FFI decode step for a fitting read.
  B) ``NativeInputs._read_raw`` — the grow-and-retry loop that resizes on
     ``SLPN_READ_NEEDS_LARGER_BUFFER`` and delivers the oversized frame intact
     (the fail-without-fix growth case).
"""

from __future__ import annotations

import ctypes

import pytest

from streamlib.processor_context import (
    DEFAULT_READ_BUF_BYTES,
    SLPN_READ_NEEDS_LARGER_BUFFER,
    NativeInputs,
    decode_read_result,
)


def pattern_bytes(size: int) -> bytes:
    """Return a ``size``-byte buffer with a deterministic, non-trivial pattern."""
    return bytes(i % 251 for i in range(size))  # prime modulus


# ============================================================================
# A) decode_read_result — pure decode of a fitting read
# ============================================================================


def test_decode_read_result_zero_length_returns_none():
    read_buf = (ctypes.c_uint8 * DEFAULT_READ_BUF_BYTES)()
    data, ts = decode_read_result(read_buf, DEFAULT_READ_BUF_BYTES, 0, 123, "port_a")
    assert data is None
    assert ts is None


@pytest.mark.parametrize(
    "data_len",
    [1, 1024, 32 * 1024, DEFAULT_READ_BUF_BYTES],
)
def test_decode_read_result_fitting_read_returns_bytes(data_len):
    payload = pattern_bytes(data_len)
    read_buf = (ctypes.c_uint8 * DEFAULT_READ_BUF_BYTES)()
    ctypes.memmove(read_buf, payload, data_len)
    data, ts = decode_read_result(read_buf, DEFAULT_READ_BUF_BYTES, data_len, 77, "p")
    assert data == payload
    assert ts == 77


# ============================================================================
# B) NativeInputs._read_raw — grow-and-retry loop
# ============================================================================


class _FakeReadLib:
    """A stand-in native lib whose ``slpn_input_read`` first reports the frame is
    too big (``SLPN_READ_NEEDS_LARGER_BUFFER``, out_len = full size) and, once the
    caller's buffer is large enough, copies the payload and returns 0.

    Mirrors the real native contract: the oversized frame is held across the two
    calls, so the second read delivers it intact.
    """

    def __init__(self, payload: bytes, timestamp_ns: int):
        self._payload = payload
        self._timestamp_ns = timestamp_ns

    def slpn_input_read(self, _ctx, _port, out_buf, buf_len, out_len, out_ts):
        required = len(self._payload)
        out_len._obj.value = required
        if required > buf_len:
            return SLPN_READ_NEEDS_LARGER_BUFFER
        # Buffer is now large enough — deliver the frame.
        ctypes.memmove(out_buf, self._payload, required)
        out_ts._obj.value = self._timestamp_ns
        return 0


@pytest.mark.parametrize(
    "data_len",
    [
        DEFAULT_READ_BUF_BYTES + 1,   # one byte over the starting buffer
        256 * 1024,                   # a 256 KiB grown frame
        4 * 1024 * 1024,              # a 4 MiB keyframe-sized frame
    ],
)
def test_read_raw_grows_and_delivers_oversized_frame(data_len):
    # A frame larger than the DEFAULT starting buffer must still be delivered
    # intact via grow-and-retry. Fail-without-fix: revert `_read_raw` to a single
    # fixed-buffer read (no SLPN_READ_NEEDS_LARGER_BUFFER handling) and this frame
    # is dropped (returns None), failing the byte-for-byte assertion.
    payload = pattern_bytes(data_len)
    inputs = NativeInputs(_FakeReadLib(payload, timestamp_ns=4242), ctx_ptr=0)
    assert inputs._read_buf_bytes == DEFAULT_READ_BUF_BYTES

    data, ts = inputs._read_raw("video_in")

    assert data == payload, "grown frame must be delivered byte-for-byte"
    assert ts == 4242
    assert inputs._read_buf_bytes >= data_len, "buffer must have grown to fit"


def test_read_raw_returns_none_when_no_data():
    class _NoData:
        def slpn_input_read(self, _ctx, _port, _out_buf, _buf_len, out_len, _out_ts):
            out_len._obj.value = 0
            return 1  # native "no data available"

    inputs = NativeInputs(_NoData(), ctx_ptr=0)
    assert inputs._read_raw("p") == (None, None)

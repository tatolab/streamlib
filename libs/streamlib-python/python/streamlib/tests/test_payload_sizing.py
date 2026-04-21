# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Unit tests for buffer-sizing + truncation-detection logic.

These cover every branch of the read-path size check:

  A) ``compute_read_buf_bytes`` — picks the largest declared input size with
     a default floor. Happy paths for every kind of input shape a host might
     emit (empty, missing field, below default, above default, multiple).

  B) ``decode_read_result`` — the pure post-FFI decode step. Runs the full
     matrix of (data_len, read_buf_bytes) cases to confirm:

     - Zero-length reads return ``(None, None)`` without logging.
     - Reads where ``data_len <= read_buf_bytes`` return the first
       ``data_len`` bytes of the buffer exactly, with the reported
       timestamp.
     - Reads where ``data_len > read_buf_bytes`` (the truncation case the
       pre-fix 32 KB hard-coded buffer triggered) return
       ``(None, None)`` and log a descriptive error.

The iceoryx2 / FFI wire itself is covered by the Rust integration test
``test_frame_header_plus_256kb_roundtrip_through_slice_service``; this suite
is deliberately pure so it runs without spawning a subprocess or loading
the cdylib.
"""

from __future__ import annotations

import ctypes
import sys

import pytest

from streamlib.processor_context import (
    DEFAULT_READ_BUF_BYTES,
    compute_read_buf_bytes,
    decode_read_result,
)


# ============================================================================
# Helpers
# ============================================================================


def make_ffi_result(read_buf_bytes: int, data: bytes):
    """Build the scratch state ``NativeInputs`` owns and simulate an FFI read
    completing: copy ``data`` into the first ``min(len(data), read_buf_bytes)``
    bytes of the buffer and return the reported data length.

    Mirrors what ``slpn_input_read`` does: it reports the ORIGINAL payload
    length even when ``len(data) > read_buf_bytes``, leaving the caller to
    detect truncation.
    """
    read_buf = (ctypes.c_uint8 * read_buf_bytes)()
    copy_len = min(len(data), read_buf_bytes)
    ctypes.memmove(read_buf, data, copy_len)
    return read_buf, len(data)


def pattern_bytes(size: int) -> bytes:
    """Return a ``size``-byte buffer with a deterministic, non-trivial pattern."""
    return bytes(i % 251 for i in range(size))  # prime modulus


# ============================================================================
# A) compute_read_buf_bytes — host-declared size derivation
# ============================================================================


def test_compute_read_buf_bytes_no_inputs_returns_default():
    assert compute_read_buf_bytes([]) == DEFAULT_READ_BUF_BYTES


def test_compute_read_buf_bytes_missing_field_falls_back_to_default():
    assert compute_read_buf_bytes([{}]) == DEFAULT_READ_BUF_BYTES


def test_compute_read_buf_bytes_declared_below_default_clamps_up():
    # A schema may legitimately declare something small (say 16 KB for an
    # audio-only port). We still ceiling the buffer at the default so shared
    # code paths have a consistent minimum.
    small = 16 * 1024
    assert small < DEFAULT_READ_BUF_BYTES
    assert (
        compute_read_buf_bytes([{"max_payload_bytes": small}])
        == DEFAULT_READ_BUF_BYTES
    )


def test_compute_read_buf_bytes_declared_equal_to_default():
    assert (
        compute_read_buf_bytes([{"max_payload_bytes": DEFAULT_READ_BUF_BYTES}])
        == DEFAULT_READ_BUF_BYTES
    )


def test_compute_read_buf_bytes_declared_above_default_wins():
    one_mb = 1 * 1024 * 1024
    assert compute_read_buf_bytes([{"max_payload_bytes": one_mb}]) == one_mb


def test_compute_read_buf_bytes_multi_input_picks_max():
    small = 16 * 1024
    medium = 128 * 1024
    large = 512 * 1024
    assert compute_read_buf_bytes(
        [
            {"max_payload_bytes": small},
            {},
            {"max_payload_bytes": medium},
            {"max_payload_bytes": large},
        ]
    ) == large


def test_compute_read_buf_bytes_multi_input_all_below_default_clamps():
    assert compute_read_buf_bytes(
        [
            {"max_payload_bytes": 1024},
            {"max_payload_bytes": 8192},
            {"max_payload_bytes": 16384},
        ]
    ) == DEFAULT_READ_BUF_BYTES


# ============================================================================
# B) decode_read_result — post-FFI decode matrix
# ============================================================================


def test_decode_read_result_zero_length_returns_none_without_logging(capsys):
    read_buf, _ = make_ffi_result(DEFAULT_READ_BUF_BYTES, b"")
    data, ts = decode_read_result(
        read_buf, DEFAULT_READ_BUF_BYTES, 0, 123, "port_a"
    )
    assert data is None
    assert ts is None
    captured = capsys.readouterr()
    assert captured.err == ""


# Happy paths — parameterize over a matrix of (read_buf_bytes, data_len)
# chosen to exercise several boundary conditions:
#
#   - 1 KB data in a default-sized buffer             (tiny payload, default buf)
#   - 32 KB data in a default-sized buffer            (former hard-coded limit; must still work)
#   - 32 KB + 1 B data in a default-sized buffer      (proves old cap is gone)
#   - DEFAULT_READ_BUF_BYTES exactly in a default buf (boundary)
#   - 256 KB data in a 1 MB buffer                    (grown buffer via schema)
#   - 1 MB data in a 1 MB buffer                      (exact fit at the top end)
HAPPY_PATH_MATRIX = [
    pytest.param(DEFAULT_READ_BUF_BYTES, 1024,                      id="1KB-in-default-buf"),
    pytest.param(DEFAULT_READ_BUF_BYTES, 32 * 1024,                 id="32KB-in-default-buf"),
    pytest.param(DEFAULT_READ_BUF_BYTES, 32 * 1024 + 1,             id="32KB+1B-in-default-buf"),
    pytest.param(DEFAULT_READ_BUF_BYTES, DEFAULT_READ_BUF_BYTES,    id="exact-default-in-default-buf"),
    pytest.param(1024 * 1024, 256 * 1024,                           id="256KB-in-1MB-buf"),
    pytest.param(1024 * 1024, 1024 * 1024,                          id="1MB-in-1MB-buf-exact-fit"),
]


@pytest.mark.parametrize(("read_buf_bytes", "data_len"), HAPPY_PATH_MATRIX)
def test_decode_read_result_happy_path(capsys, read_buf_bytes, data_len):
    payload = pattern_bytes(data_len)
    read_buf, reported_len = make_ffi_result(read_buf_bytes, payload)
    ts = data_len * 1000

    data, out_ts = decode_read_result(
        read_buf, read_buf_bytes, reported_len, ts, "happy_port"
    )

    assert data is not None
    assert len(data) == data_len
    assert data == payload, (
        "decoded bytes should match source payload byte-for-byte"
    )
    assert out_ts == ts
    # Ensure decode_read_result hands back a distinct `bytes` — mutating the
    # scratch read buffer after the call must not affect the returned value.
    read_buf[0] = (read_buf[0] + 1) % 256
    assert data[0] == payload[0]

    captured = capsys.readouterr()
    assert captured.err == "", "happy path must not log truncation warnings"


# Truncation paths — native reported more bytes than the read buffer can hold.
# This is the exact shape the pre-fix 32 KB hard-coded buffer triggered when a
# publisher sent encoded-video-sized frames.
TRUNCATION_MATRIX = [
    pytest.param(DEFAULT_READ_BUF_BYTES, DEFAULT_READ_BUF_BYTES + 1,  id="1B-over-default"),
    pytest.param(32 * 1024, DEFAULT_READ_BUF_BYTES,                   id="32KB-buf-vs-65KB-payload"),
    pytest.param(DEFAULT_READ_BUF_BYTES, 256 * 1024,                  id="256KB-in-default-buf"),
    pytest.param(512 * 1024, 1024 * 1024,                             id="1MB-in-512KB-buf"),
]


@pytest.mark.parametrize(("read_buf_bytes", "data_len"), TRUNCATION_MATRIX)
def test_decode_read_result_truncation(capsys, read_buf_bytes, data_len):
    payload = pattern_bytes(data_len)
    read_buf, reported_len = make_ffi_result(read_buf_bytes, payload)

    data, out_ts = decode_read_result(
        read_buf, read_buf_bytes, reported_len, 42, "truncated_port"
    )

    assert data is None, "truncation must surface as None, not a short/corrupt payload"
    assert out_ts is None

    captured = capsys.readouterr()
    assert "payload truncated on port 'truncated_port'" in captured.err
    assert str(data_len) in captured.err
    assert str(read_buf_bytes) in captured.err

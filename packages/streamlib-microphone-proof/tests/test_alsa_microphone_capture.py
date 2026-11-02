# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native capture path end to end on ALSA's `null` PCM, which needs no device."""

import ctypes.util

import pytest
from tatolab.stream.audio_block import AudioBlock

from tatolab.microphone_proof import _native

pytestmark = pytest.mark.skipif(
    ctypes.util.find_library("asound") is None, reason="no libasound on this machine"
)


def test_a_captured_block_is_the_built_ins_audio_block_message() -> None:
    capture = _native.AlsaMicrophoneCapture("null", 48_000, 2, 256, 4)
    before = _native.monotonic_now_ns()
    capture.start()
    try:
        message = capture.next_message(2_000)
    finally:
        capture.stop()

    assert message is not None
    block = AudioBlock.from_bytes(message.payload)
    assert (block.sample_rate, block.channels, block.sample_count, block.dtype) == (
        48_000,
        2,
        256,
        "f32",
    )
    assert block.first_sample_timestamp_ns == message.first_sample_timestamp_ns
    assert before - 1_000_000_000 < block.first_sample_timestamp_ns <= _native.monotonic_now_ns()


def test_a_stopped_capture_reports_it_has_ended_and_yields_nothing_new() -> None:
    capture = _native.AlsaMicrophoneCapture("null", 48_000, 1, 256, 2)
    capture.start()
    capture.stop()

    assert capture.capture_has_ended
    assert capture.failure_that_ended_the_capture is None
    while capture.next_message(0) is not None:
        pass
    assert capture.next_message(50) is None


def test_a_pcm_alsa_cannot_open_is_refused_by_name() -> None:
    with pytest.raises(OSError, match="no-such-pcm"):
        _native.AlsaMicrophoneCapture("no-such-pcm", 48_000, 1, 256, 2)


def test_a_capture_starts_once() -> None:
    capture = _native.AlsaMicrophoneCapture("null", 48_000, 1, 256, 2)
    capture.start()
    try:
        with pytest.raises(RuntimeError, match="already started"):
            capture.start()
    finally:
        capture.stop()

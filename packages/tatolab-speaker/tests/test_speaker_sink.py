# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`SpeakerSink` over a wired link with no runtime: the node's helper-side
context opened directly, as a processor interpreter opens it.

Whatever audio arm this machine probes plays — a device, or the null backend
discarding — so each test runs on a workstation and in a container alike. With
no runtime there is no read-side window stage, so the writer here writes blocks
already in the format the speaker plays. The tests import `tatolab.runtime`
from its lend, on `PYTHONPATH`.
"""

import os
import time

import pytest

from helper_process_link_wiring import wire_one_link
from tatolab.runtime import _engine
from tatolab.speaker import (
    AUDIO_INPUT_PORT,
    SPEAKER_CHANNELS,
    SPEAKER_SAMPLE_FORMAT,
    SPEAKER_SAMPLE_RATE,
    SPEAKER_WINDOW_SIZE,
    SpeakerSink,
    SpeakerSinkConfig,
)
from tatolab.stream import RuntimeContextFullAccess, log
from tatolab.stream.audio_block import AudioBlock

#: Long enough for any arm to drain a few blocks, which first opens a device or
#: starts the null backend's clock.
PLAYED_BLOCKS_DEADLINE_SECONDS = 5.0

BLOCKS_WRITTEN_PER_TEST = 5


def a_block_in_the_speakers_format(first_sample_timestamp_ns: int) -> bytes:
    return AudioBlock(
        samples=bytes(SPEAKER_WINDOW_SIZE * SPEAKER_CHANNELS * 4),
        sample_rate=SPEAKER_SAMPLE_RATE,
        channels=SPEAKER_CHANNELS,
        sample_count=SPEAKER_WINDOW_SIZE,
        first_sample_timestamp_ns=first_sample_timestamp_ns,
        dtype=SPEAKER_SAMPLE_FORMAT,
    ).to_bytes()


class SpeakerWiredToAWriter:
    """A `SpeakerSink`'s `audio` port wired from an output written here."""

    def __init__(self, request: pytest.FixtureRequest) -> None:
        self.log_records: "list[str]" = []
        self.next_first_sample_timestamp_ns = 1
        unique = f"spk{os.getpid()}_{request.node.name}"[:60]
        writer_outputs = _engine.open_node_link_data_access_for_helper_process()
        speaker_inputs = _engine.open_node_link_data_access_for_helper_process()
        writer_outputs.declare_ports([], [AUDIO_INPUT_PORT])
        speaker_inputs.declare_ports([AUDIO_INPUT_PORT], [])
        wire_one_link(
            writer_outputs,
            AUDIO_INPUT_PORT,
            speaker_inputs,
            AUDIO_INPUT_PORT,
            f"{unique}/{AUDIO_INPUT_PORT}",
            f"{unique}_speaker/notify",
        )
        self.writer: RuntimeContextFullAccess = (
            _engine.open_runtime_context_full_access_for_helper_process(
                {}, writer_outputs, "runtime-under-test", "writer-under-test"
            )
        )
        self.speaker_context: RuntimeContextFullAccess = (
            _engine.open_runtime_context_full_access_for_helper_process(
                {}, speaker_inputs, "runtime-under-test", "speaker-under-test"
            )
        )

    def write(self, payload: bytes) -> None:
        self.writer.outputs.write(AUDIO_INPUT_PORT, payload)

    def write_blocks_in_the_speakers_format(self, block_count: int) -> None:
        for _ in range(block_count):
            self.write(a_block_in_the_speakers_format(self.next_first_sample_timestamp_ns))
            self.next_first_sample_timestamp_ns += SPEAKER_WINDOW_SIZE * 1_000_000_000 // (
                SPEAKER_SAMPLE_RATE
            )


def await_count(read_count, at_least: int, what: str) -> None:
    deadline = time.monotonic() + PLAYED_BLOCKS_DEADLINE_SECONDS
    while read_count() < at_least:
        assert time.monotonic() < deadline, (
            f"{read_count()} of {at_least} {what} within {PLAYED_BLOCKS_DEADLINE_SECONDS} s"
        )
        time.sleep(0.01)


@pytest.fixture
def speaker_wired_to_a_writer(
    request, monkeypatch, private_iceoryx2_domain_for_this_test_process
) -> SpeakerWiredToAWriter:
    """`log` reaches a sink only in the interpreter the runtime starts for a
    node, so here every level is recorded instead."""
    del private_iceoryx2_domain_for_this_test_process
    wired = SpeakerWiredToAWriter(request)
    for log_level in ("debug", "info", "warn", "error"):
        monkeypatch.setattr(log, log_level, wired.log_records.append)
    return wired


def test_a_started_speaker_plays_every_block_in_its_format(speaker_wired_to_a_writer):
    speaker = SpeakerSink(SpeakerSinkConfig())
    context = speaker_wired_to_a_writer.speaker_context
    speaker.setup(context)
    playback = speaker._opened_playback()
    speaker.start(context)
    try:
        speaker_wired_to_a_writer.write_blocks_in_the_speakers_format(BLOCKS_WRITTEN_PER_TEST)
        await_count(
            lambda: playback.played_block_count, BLOCKS_WRITTEN_PER_TEST, "blocks played"
        )
    finally:
        speaker.stop(context)
        speaker.teardown(context)

    assert (playback.sample_rate, playback.channels, playback.sample_format) == (
        SPEAKER_SAMPLE_RATE,
        SPEAKER_CHANNELS,
        SPEAKER_SAMPLE_FORMAT,
    )
    assert playback.refused_block_count == 0
    records = speaker_wired_to_a_writer.log_records
    assert any(
        f"{SPEAKER_SAMPLE_RATE} Hz × {SPEAKER_CHANNELS} ch" in record for record in records
    ), records
    assert any(
        f"played_blocks={BLOCKS_WRITTEN_PER_TEST}" in record and "underrun_bytes=" in record
        for record in records
    ), records


def test_a_block_in_another_format_is_refused_and_counted_never_played(
    speaker_wired_to_a_writer,
):
    speaker = SpeakerSink(SpeakerSinkConfig())
    context = speaker_wired_to_a_writer.speaker_context
    speaker.setup(context)
    playback = speaker._opened_playback()
    speaker.start(context)
    try:
        speaker_wired_to_a_writer.write(
            AudioBlock(
                samples=bytes(160 * 2),
                sample_rate=16_000,
                channels=1,
                sample_count=160,
                first_sample_timestamp_ns=1,
                dtype="i16",
            ).to_bytes()
        )
        speaker_wired_to_a_writer.write(b"not an audio block")
        await_count(lambda: playback.refused_block_count, 2, "blocks refused")
    finally:
        speaker.teardown(context)

    assert playback.played_block_count == 0


def test_the_audio_port_is_lent_to_native_code_so_python_cannot_read_it(
    speaker_wired_to_a_writer,
):
    speaker = SpeakerSink(SpeakerSinkConfig())
    context = speaker_wired_to_a_writer.speaker_context
    speaker.setup(context)
    try:
        with pytest.raises(Exception, match=AUDIO_INPUT_PORT):
            context.inputs.read(AUDIO_INPUT_PORT)
    finally:
        speaker.teardown(context)


def test_a_device_the_backend_cannot_open_refuses_setup_naming_it(speaker_wired_to_a_writer):
    speaker = SpeakerSink(SpeakerSinkConfig(device_id="tatolab-no-such-speaker"))
    with pytest.raises(RuntimeError, match="tatolab-no-such-speaker"):
        speaker.setup(speaker_wired_to_a_writer.speaker_context)


def test_a_speaker_stopped_and_started_again_keeps_playing(speaker_wired_to_a_writer):
    speaker = SpeakerSink(SpeakerSinkConfig())
    context = speaker_wired_to_a_writer.speaker_context
    speaker.setup(context)
    playback = speaker._opened_playback()
    try:
        speaker.start(context)
        speaker_wired_to_a_writer.write_blocks_in_the_speakers_format(1)
        await_count(lambda: playback.played_block_count, 1, "blocks played")
        speaker.stop(context)
        speaker.start(context)
        speaker_wired_to_a_writer.write_blocks_in_the_speakers_format(1)
        await_count(lambda: playback.played_block_count, 2, "blocks played")
    finally:
        speaker.teardown(context)


def test_a_speaker_never_set_up_or_started_tears_down_cleanly(speaker_wired_to_a_writer):
    speaker = SpeakerSink(SpeakerSinkConfig())
    context = speaker_wired_to_a_writer.speaker_context
    speaker.stop(context)
    speaker.teardown(context)
    speaker.setup(context)
    speaker.stop(context)
    speaker.teardown(context)


def test_start_before_setup_is_refused_by_name(speaker_wired_to_a_writer):
    speaker = SpeakerSink(SpeakerSinkConfig())
    with pytest.raises(RuntimeError, match="before setup"):
        speaker.start(speaker_wired_to_a_writer.speaker_context)


def test_the_distribution_declares_the_node_for_the_node_catalog():
    """The node type is the import path a processor interpreter imports the
    class back by, and the catalog reads it off the `tatolab.nodes` entry point."""
    from importlib.metadata import distribution

    declared = {
        entry_point.name: entry_point
        for entry_point in distribution("tatolab-speaker").entry_points
        if entry_point.group == "tatolab.nodes"
    }

    assert set(declared) == {"SpeakerSink"}
    assert declared["SpeakerSink"].value == "tatolab.speaker:SpeakerSink"
    assert declared["SpeakerSink"].load() is SpeakerSink

# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`SpeakerSink` over a wired link with no runtime: the node's helper-side
context opened directly, as a node interpreter opens it.

Whatever audio arm this machine probes plays — a device, or the null backend
discarding — so each test runs on a workstation and in a container alike. With
no runtime there is no read-side window stage, so the writer here writes blocks
already in the format the speaker plays. The tests import `tatolab.runtime`
from its lend, on `PYTHONPATH`.
"""

import array
import os
import time

import pytest

import tatolab.speaker
from helper_process_link_wiring import wire_one_link
from tatolab.runtime._node_function_running_for_a_test import NodeFunctionRunningForATest
from tatolab.runtime._node_the_wheel_hosts import link_input_data_reader_of, link_output_data_writer_of
from tatolab.runtime import _engine
from tatolab.speaker import (
    AUDIO_INPUT_PORT,
    SPEAKER_CHANNELS,
    SPEAKER_SAMPLE_FORMAT,
    SPEAKER_SAMPLE_RATE,
    SPEAKER_WINDOW_SIZE,
    SpeakerSink,
)
from tatolab.stream import RuntimeContextFullAccess, log
from tatolab.stream._node_declaration import NODE_DECLARATION_INPUT_PORTS_STAMP


#: Long enough for any arm to drain a few blocks, which first opens a device or
#: starts the null backend's clock.
PLAYED_BLOCKS_DEADLINE_SECONDS = 5.0

BLOCKS_WRITTEN_PER_TEST = 5


def silent_planar_f32_samples(channel_count: int, samples_per_channel: int) -> "memoryview[float]":
    """A C-contiguous `[channel][sample]` float32 buffer of silence."""
    return memoryview(array.array("f", bytes(channel_count * samples_per_channel * 4))).cast(
        "B"
    ).cast("f", (channel_count, samples_per_channel))


class SpeakerWiredToAWriter:
    """A `SpeakerSink`'s `audio` port wired from an output written here."""

    def __init__(self, request: pytest.FixtureRequest) -> None:
        self.log_records: "list[str]" = []
        self.next_start_sample = 0
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
                writer_outputs, "runtime-under-test", "writer-under-test"
            )
        )
        self.speaker_context: RuntimeContextFullAccess = (
            _engine.open_runtime_context_full_access_for_helper_process(
                speaker_inputs, "runtime-under-test", "speaker-under-test"
            )
        )

    def a_speaker(self, **values: object) -> NodeFunctionRunningForATest:
        """A `SpeakerSink` running as its helper runs it, `audio` bound to a stand-in: its native
        half reads the port."""
        return NodeFunctionRunningForATest(
            SpeakerSink,
            tatolab.speaker,
            self.speaker_context,
            values=values,
            pins={"audio": object()},
        )

    def write(self, payload: bytes) -> None:
        link_output_data_writer_of(self.writer).write(AUDIO_INPUT_PORT, payload)

    def write_silent_channels_block(
        self, channel_count: int, samples_per_channel: int, sample_rate: int
    ) -> None:
        link_output_data_writer_of(self.writer).write_channels_block(
            AUDIO_INPUT_PORT,
            silent_planar_f32_samples(channel_count, samples_per_channel),
            start_sample=self.next_start_sample,
            sample_rate=sample_rate,
        )
        self.next_start_sample += samples_per_channel

    def write_blocks_in_the_speakers_format(self, block_count: int) -> None:
        for _ in range(block_count):
            self.write_silent_channels_block(
                SPEAKER_CHANNELS, SPEAKER_WINDOW_SIZE, SPEAKER_SAMPLE_RATE
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


def test_a_speaker_plays_every_block_in_its_format(speaker_wired_to_a_writer):
    speaker = speaker_wired_to_a_writer.a_speaker()
    try:
        playback = speaker.built()
        speaker_wired_to_a_writer.write_blocks_in_the_speakers_format(BLOCKS_WRITTEN_PER_TEST)
        await_count(
            lambda: playback.played_block_count, BLOCKS_WRITTEN_PER_TEST, "blocks played"
        )
    finally:
        speaker.stop()

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


def test_a_block_in_another_format_is_refused_and_counted_never_played(
    speaker_wired_to_a_writer,
):
    speaker = speaker_wired_to_a_writer.a_speaker()
    try:
        playback = speaker.built()
        speaker_wired_to_a_writer.write_silent_channels_block(1, 160, 16_000)
        speaker_wired_to_a_writer.write(b"not a channels block")
        await_count(lambda: playback.refused_block_count, 2, "blocks refused")
    finally:
        speaker.stop()

    assert playback.played_block_count == 0


def test_the_audio_port_is_lent_to_native_code_so_python_cannot_read_it(
    speaker_wired_to_a_writer,
):
    speaker = speaker_wired_to_a_writer.a_speaker()
    try:
        with pytest.raises(Exception, match=AUDIO_INPUT_PORT):
            link_input_data_reader_of(speaker_wired_to_a_writer.speaker_context).read(
                AUDIO_INPUT_PORT
            )
    finally:
        speaker.stop()


def test_a_device_the_backend_cannot_open_fails_the_open_naming_it(speaker_wired_to_a_writer):
    speaker = speaker_wired_to_a_writer.a_speaker(device_id="tatolab-no-such-speaker")
    try:
        assert "tatolab-no-such-speaker" in speaker.why_the_last_build_failed()
    finally:
        speaker.stop()


def test_a_device_set_while_playing_opens_it_and_keeps_playing(
    speaker_wired_to_a_writer,
):
    """Mental-revert: leave the playback a set replaces open, and both drain the port."""
    speaker = speaker_wired_to_a_writer.a_speaker()
    try:
        first = speaker.built()
        speaker_wired_to_a_writer.write_blocks_in_the_speakers_format(1)
        await_count(lambda: first.played_block_count, 1, "blocks played")

        speaker.set(device_id="")
        second = speaker.built()
        # What the playback replaced had drained before it was closed.
        time.sleep(0.2)
        played_by_the_first = first.played_block_count
        speaker_wired_to_a_writer.write_blocks_in_the_speakers_format(BLOCKS_WRITTEN_PER_TEST)
        await_count(
            lambda: second.played_block_count, BLOCKS_WRITTEN_PER_TEST, "blocks played"
        )
    finally:
        speaker.stop()

    assert second is not first
    assert first.played_block_count == played_by_the_first, (
        "the playback replaced plays nothing more"
    )


def test_a_device_set_while_playing_closes_the_playback_it_has_even_when_it_fails_to_open(
    speaker_wired_to_a_writer,
):
    """Mental-revert: open the device a set names beside the playback it replaces, and a device
    that fails to open leaves the old one draining the port."""
    speaker = speaker_wired_to_a_writer.a_speaker()
    try:
        first = speaker.built()
        speaker_wired_to_a_writer.write_blocks_in_the_speakers_format(1)
        await_count(lambda: first.played_block_count, 1, "blocks played")

        speaker.set(device_id="tatolab-no-such-speaker")
        assert "tatolab-no-such-speaker" in speaker.why_the_last_build_failed()
        played_once_closed = first.played_block_count
        speaker_wired_to_a_writer.write_blocks_in_the_speakers_format(BLOCKS_WRITTEN_PER_TEST)
        time.sleep(0.2)
        played_after = first.played_block_count
    finally:
        speaker.stop()

    assert played_after == played_once_closed, "the playback replaced plays nothing more"


def test_the_distribution_declares_the_node_for_the_node_catalog():
    """The node type is the import path a node interpreter imports the
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


def test_the_audio_pin_shows_a_meter_and_reads_ordered_windows_in_the_format_it_plays():
    audio = next(
        port
        for port in getattr(SpeakerSink, NODE_DECLARATION_INPUT_PORTS_STAMP)
        if port["name"] == AUDIO_INPUT_PORT
    )

    assert audio["pin_declaration"] == {
        "pin_type": {"kind": "is", "pin_type": {"type": "audio"}},
        "flow": "stream",
        "annotations": {"control": "meter"},
    }
    assert audio["delivery_profile"] == "ordered"
    assert audio["audio_window"]["sample_rate"] == SPEAKER_SAMPLE_RATE
    assert audio["audio_window"]["channels"] == SPEAKER_CHANNELS
    assert audio["audio_window"]["dtype"] == SPEAKER_SAMPLE_FORMAT
    assert audio["audio_window"]["window_size"] == SPEAKER_WINDOW_SIZE

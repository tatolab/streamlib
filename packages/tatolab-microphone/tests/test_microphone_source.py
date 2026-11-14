# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""`MicrophoneSource` over a wired link with no runtime: the node's helper-side
context opened directly, as a processor interpreter opens it.

Whatever audio arm this machine probes captures — a device, or the null
backend's silence — so each test runs on a workstation and in a container
alike. The tests import `tatolab.runtime` from its lend, on `PYTHONPATH`.
"""

import os
import time

import pytest

from helper_process_link_wiring import wire_one_link
from tatolab.microphone import AUDIO_OUTPUT_PORT, MicrophoneSource, MicrophoneSourceConfig
from tatolab.runtime import _engine
from tatolab.stream import Channels, ChannelsBlock, Input, RuntimeContextFullAccess, log

#: Long enough for the first block on any arm, which opens a device or starts
#: the null backend's clock first.
FIRST_BLOCKS_DEADLINE_SECONDS = 5.0

BLOCKS_READ_PER_TEST = 5


class MicrophoneBlockReader:
    """The reader's `audio` input, read through its attribute as Channels blocks."""

    audio = Input(Channels, delivery="ordered")


class MicrophoneWiredToAReader:
    """A `MicrophoneSource`'s `audio` port wired to an input read here."""

    def __init__(self, request: pytest.FixtureRequest) -> None:
        self.log_records: "list[str]" = []
        unique = f"mic{os.getpid()}_{request.node.name}"[:60]
        reader_inputs = _engine.open_node_link_data_access_for_helper_process()
        microphone_outputs = _engine.open_node_link_data_access_for_helper_process()
        microphone_outputs.declare_ports([], [AUDIO_OUTPUT_PORT])
        wire_one_link(
            microphone_outputs,
            AUDIO_OUTPUT_PORT,
            reader_inputs,
            AUDIO_OUTPUT_PORT,
            f"{unique}/{AUDIO_OUTPUT_PORT}",
            f"{unique}_reader/notify",
        )
        self.reader: RuntimeContextFullAccess = (
            _engine.open_runtime_context_full_access_for_helper_process(
                {}, reader_inputs, "runtime-under-test", "reader-under-test"
            )
        )
        self.microphone_context: RuntimeContextFullAccess = (
            _engine.open_runtime_context_full_access_for_helper_process(
                {}, microphone_outputs, "runtime-under-test", "microphone-under-test"
            )
        )

    def read_blocks(self, block_count: int) -> "list[ChannelsBlock]":
        blocks: "list[ChannelsBlock]" = []
        deadline = time.monotonic() + FIRST_BLOCKS_DEADLINE_SECONDS
        while len(blocks) < block_count:
            block = self.reader.inputs.read(MicrophoneBlockReader.audio)
            if block is not None:
                blocks.append(block)
                continue
            assert time.monotonic() < deadline, (
                f"{len(blocks)} of {block_count} blocks arrived within "
                f"{FIRST_BLOCKS_DEADLINE_SECONDS} s"
            )
            time.sleep(0.01)
        return blocks


@pytest.fixture
def microphone_wired_to_a_reader(
    request, monkeypatch, private_iceoryx2_domain_for_this_test_process
) -> MicrophoneWiredToAReader:
    """`log` reaches a sink only in the interpreter the runtime starts for a
    node, so here every level is recorded instead."""
    del private_iceoryx2_domain_for_this_test_process
    wired = MicrophoneWiredToAReader(request)
    for log_level in ("debug", "info", "warn", "error"):
        monkeypatch.setattr(log, log_level, wired.log_records.append)
    return wired


def test_a_started_microphone_publishes_device_stamped_blocks_in_the_format_it_opened(
    microphone_wired_to_a_reader,
):
    microphone = MicrophoneSource(MicrophoneSourceConfig())
    context = microphone_wired_to_a_reader.microphone_context
    microphone.setup(context)
    capture = microphone._opened_capture()
    microphone.start(context)
    try:
        blocks = microphone_wired_to_a_reader.read_blocks(BLOCKS_READ_PER_TEST)
    finally:
        microphone.stop(context)
        microphone.teardown(context)

    assert any(
        f"{capture.sample_rate} Hz × {capture.channels} ch" in record
        for record in microphone_wired_to_a_reader.log_records
    )
    for block in blocks:
        assert block.sample_rate == capture.sample_rate
        assert block.channel_count == capture.channels
        assert block.dtype == "f32"
        assert block.samples_per_channel > 0
    for earlier, later in zip(blocks, blocks[1:]):
        assert later.start_sample >= earlier.start_sample + earlier.samples_per_channel
    stamps = [block.timestamp_ns for block in blocks]
    assert stamps == sorted(stamps) and len(set(stamps)) == len(stamps)
    assert capture.published_block_count >= BLOCKS_READ_PER_TEST


def test_the_audio_port_is_lent_to_native_code_so_python_cannot_write_it(
    microphone_wired_to_a_reader,
):
    microphone = MicrophoneSource(MicrophoneSourceConfig())
    context = microphone_wired_to_a_reader.microphone_context
    microphone.setup(context)
    try:
        with pytest.raises(Exception, match=AUDIO_OUTPUT_PORT):
            context.outputs.write(AUDIO_OUTPUT_PORT, b"from python")
    finally:
        microphone.teardown(context)


def test_a_device_the_backend_cannot_open_refuses_setup_naming_it(
    microphone_wired_to_a_reader,
):
    microphone = MicrophoneSource(MicrophoneSourceConfig(device_id="tatolab-no-such-microphone"))
    with pytest.raises(RuntimeError, match="tatolab-no-such-microphone"):
        microphone.setup(microphone_wired_to_a_reader.microphone_context)


def test_a_microphone_stopped_and_started_again_keeps_publishing(
    microphone_wired_to_a_reader,
):
    microphone = MicrophoneSource(MicrophoneSourceConfig())
    context = microphone_wired_to_a_reader.microphone_context
    microphone.setup(context)
    try:
        microphone.start(context)
        microphone_wired_to_a_reader.read_blocks(1)
        microphone.stop(context)
        microphone.start(context)
        microphone_wired_to_a_reader.read_blocks(1)
    finally:
        microphone.teardown(context)


def test_a_microphone_never_set_up_or_started_tears_down_cleanly(
    microphone_wired_to_a_reader,
):
    microphone = MicrophoneSource(MicrophoneSourceConfig())
    context = microphone_wired_to_a_reader.microphone_context
    microphone.stop(context)
    microphone.teardown(context)
    microphone.setup(context)
    microphone.stop(context)
    microphone.teardown(context)


def test_start_before_setup_is_refused_by_name(microphone_wired_to_a_reader):
    microphone = MicrophoneSource(MicrophoneSourceConfig())
    with pytest.raises(RuntimeError, match="before setup"):
        microphone.start(microphone_wired_to_a_reader.microphone_context)


def test_the_distribution_declares_the_node_for_the_node_catalog():
    """The node type is the import path a processor interpreter imports the
    class back by, and the catalog reads it off the `tatolab.nodes` entry point."""
    from importlib.metadata import distribution

    declared = {
        entry_point.name: entry_point
        for entry_point in distribution("tatolab-microphone").entry_points
        if entry_point.group == "tatolab.nodes"
    }

    assert set(declared) == {"MicrophoneSource"}
    assert declared["MicrophoneSource"].value == "tatolab.microphone:MicrophoneSource"
    assert declared["MicrophoneSource"].load() is MicrophoneSource

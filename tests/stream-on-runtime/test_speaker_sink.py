# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`tatolab.speaker.SpeakerSink` — the playback module, node class to device callback.

The load tests need no device: `tatolabd` loads the graph and its start is
then refused at the GPU. The graph tests start the engine, which initializes a GPU context,
so they carry `requires_gpu` like every other graph test here.

Deliberately arm-agnostic: the backend chain picks whichever arm the machine
running this actually has, and these assertions hold on all of them — the last
arm needs no audio library at all, so this still runs in a container. The
speaker's port declares a 48 kHz stereo `f32` window and its playback stream
opens in that format, which the backend converts to the device. What is *not*
asserted here is that a tone played out comes back recognisable; that
needs a device on both ends of a loop and lives in the engine's own fixture,
`runtime/streamlib-engine/tests/fixtures/verify_audio_loopback.sh`.
"""

import re
from collections.abc import Callable

import pytest

from block_wiring_streams import microphone_wired_straight_into_a_speaker
from conftest import StreamRunWithNoVulkanDriverOutcome, TatolabdUnderTest
from speaker_sink_streams import (
    SPEAKER_NODE_NAME,
    UNOPENABLE_DEVICE_ID,
    microphone_into_a_speaker_and_a_block_counting_probe,
    one_speaker_sink_left_unnamed,
    speaker_sink_naming_an_unopenable_device,
)
from tatolab.speaker import (
    SPEAKER_CHANNELS,
    SPEAKER_SAMPLE_FORMAT,
    SPEAKER_SAMPLE_RATE,
    SPEAKER_WINDOW_SIZE,
)
from tatolab.stream import run_graph_generator

READINESS_TIMEOUT_SECONDS = 10.0
PLAYBACK_READINESS_TIMEOUT_SECONDS = 20.0

PLAYED_BLOCKS = re.compile(r"played_blocks=(\d+)")
UNDERRUN_BYTES = re.compile(r"underrun_bytes=(\d+)")
DROPPED_BLOCKS = re.compile(r"dropped_blocks=(\d+)")

# Eight device periods of stereo `f32` at the PipeWire arm's 1024-sample
# quantum. A cold start costs two or three of these and then nothing more; a
# stream running without a cushion loses one every few blocks, which over the
# hundred blocks this waits for is an order of magnitude past this.
UNDERRUN_BYTES_A_COLD_START_MAY_COST = 8 * 1024 * 2 * 4


def the_speakers_settled_window_contract(graph: dict, speaker_node_name: str) -> object:
    """What `graph` renders for the speaker's `audio` port's window contract."""
    (speaker,) = [node for node in graph["nodes"] if node["name"] == speaker_node_name]
    audio = next(
        (port for port in speaker["ports"]["inputs"] if port["name"] == "audio"),
        None,
    )
    assert audio is not None, f"the speaker node renders no `audio` input port: {speaker}"
    return audio.get("audio_window")


# ---- node class semantics (no GPU) -----------------------------------------


def test_node_name_defaults_to_the_type_name(
    run_stream_on_tatolabd_with_no_vulkan_driver: "Callable[..., StreamRunWithNoVulkanDriverOutcome]",
):
    graph = run_graph_generator(one_speaker_sink_left_unnamed)
    assert [node["name"] for node in graph["nodes"]] == [SPEAKER_NODE_NAME]

    outcome = run_stream_on_tatolabd_with_no_vulkan_driver(graph)
    assert outcome.loaded and outcome.loaded_node_count == 1, outcome.tatolab_run_stderr_text


def test_the_speaker_declares_the_input_a_microphone_can_be_wired_to(
    run_stream_on_tatolabd_with_no_vulkan_driver: "Callable[..., StreamRunWithNoVulkanDriverOutcome]",
):
    """The two audio nodes have to compose without an adapter between them,
    which is what makes one `stream_builder.connect(microphone.output("audio"),
    speaker.input("audio"))` the whole of wiring audio through. The builder
    checks no port names, so the engine accepting the load is the proof."""
    outcome = run_stream_on_tatolabd_with_no_vulkan_driver(microphone_wired_straight_into_a_speaker)
    assert outcome.loaded and outcome.loaded_node_count == 2, outcome.tatolab_run_stderr_text


# ---- the module in a real graph (GPU) --------------------------------------


@pytest.mark.requires_gpu
@pytest.mark.audible_on_macos(
    reason="the default microphone wired straight to the default speaker feeds back without headphones"
)
def test_a_microphone_wired_to_a_speaker_runs_and_plays_what_it_captured(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    """The playback path end to end: the module's node class → its native half →
    the probed backend's playback stream, fed over a real link by the capture
    module, with no interpreter anywhere in the sample path.

    `stream_builder.add` with no `config` records `{}`, so this is also the
    added-without-config proof.

    On the ALSA arm the default source captures mono, and the speaker's port
    declares stereo, so the engine converts rather than refusing, and that
    disagreement is the case this asserts.
    """
    tatolabd = start_tatolabd_running_stream(microphone_into_a_speaker_and_a_block_counting_probe)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(
        stream=stream_id, timeout=PLAYBACK_READINESS_TIMEOUT_SECONDS
    )
    # The window the port declared, read back off the live graph.
    rendered = the_speakers_settled_window_contract(
        local_api.call_tool("graph", {"stream": stream_id}), SPEAKER_NODE_NAME
    )

    # Blocks really moving on the port the speaker reads, rather than a sleep
    # guessing that they are.
    tatolabd.await_marker("BLOCKS_COUNTED")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    stderr_text = tatolabd.stderr_text

    assert "SpeakerSink: playback stream opened" in stderr_text, (
        f"the speaker never opened a device:\n{tatolabd.recent_stderr()}"
    )

    played = PLAYED_BLOCKS.search(stderr_text)
    assert played is not None, f"no SpeakerSink teardown line:\n{tatolabd.recent_stderr()}"
    assert int(played.group(1)) > 0, (
        f"the speaker reached Running but was never given a block:\n{tatolabd.recent_stderr()}"
    )

    # Nothing is lost between the two nodes on this run. Block counts no
    # longer compare: the speaker plays windows the stage cut to its own
    # device's period, and the microphone publishes blocks at its own device's,
    # so the two are equal only by coincidence of format. What still holds is
    # loss at each end — nothing dropped at the capture edge, and the underrun
    # bound below for everything between.
    dropped = DROPPED_BLOCKS.search(stderr_text)
    assert dropped is not None, f"no MicrophoneSource teardown line:\n{tatolabd.recent_stderr()}"
    assert int(dropped.group(1)) == 0, (
        f"the microphone dropped {dropped.group(1)} blocks at the device edge, so the run "
        f"was not keeping up:\n{tatolabd.recent_stderr()}"
    )

    assert rendered is not None, "the speaker's port rendered no window contract at all"
    assert isinstance(rendered, dict), rendered
    assert {
        field: rendered[field]
        for field in ("sample_rate", "channels", "dtype", "window_size", "hop")
    } == {
        "sample_rate": SPEAKER_SAMPLE_RATE,
        "channels": SPEAKER_CHANNELS,
        "dtype": SPEAKER_SAMPLE_FORMAT,
        "window_size": SPEAKER_WINDOW_SIZE,
        "hop": SPEAKER_WINDOW_SIZE,
    }, rendered

    # The pre-roll's whole point. A speaker fed by a microphone runs in lockstep
    # with it, so without a cushion every scheduling jitter costs a whole period
    # — four in fourteen, measured before the ring pre-rolled. Bounded rather
    # than required to be zero: a cold start costs a couple of periods on any
    # real device, and what this catches is a stream that keeps paying.
    underrun = UNDERRUN_BYTES.search(stderr_text)
    assert underrun is not None, (
        f"no underrun count in the teardown line:\n{tatolabd.recent_stderr()}"
    )
    assert int(underrun.group(1)) <= UNDERRUN_BYTES_A_COLD_START_MAY_COST, (
        f"the device was given {underrun.group(1)} bytes of silence across "
        f"{played.group(1)} played blocks — the cushion is not holding:\n"
        f"{tatolabd.recent_stderr()}"
    )


@pytest.mark.requires_gpu
def test_a_device_that_was_named_and_cannot_be_opened_refuses_at_setup(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    """A machine with no audio is a supported environment; a wrong device id is
    a wiring error. Playing into a different speaker than the one named would be
    worse than failing, so the sink refuses and the processor never reaches
    Running."""
    tatolabd = start_tatolabd_running_stream(speaker_sink_naming_an_unopenable_device)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    node_states = tatolabd.local_api_client().await_every_node_past_setup(
        stream=stream_id, timeout=READINESS_TIMEOUT_SECONDS
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert node_states[SPEAKER_NODE_NAME] == "Error", (
        f"the speaker must refuse at setup rather than reach Running: {node_states}"
    )
    assert UNOPENABLE_DEVICE_ID in tatolabd.stderr_text, (
        f"the refusal must name the device that was asked for:\n{tatolabd.recent_stderr()}"
    )

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.SpeakerSink` — the playback built-in, built-in class to device callback.

The load tests need no device: `tatolabd` loads the graph and is then refused
at the GPU. The graph tests start the engine, which initializes a GPU context,
so they carry `requires_gpu` like every other graph test here.

Deliberately arm-agnostic: the backend chain picks whichever arm the machine
running this actually has, and these assertions hold on all of them — the last
arm needs no audio library at all, so this still runs in a container. Nothing
here states a rate or a channel count either, because the speaker's port
declares `audio_window = match_device` and the answer is this machine's. What
is *not* asserted here is that a tone played out comes back recognisable; that
needs a device on both ends of a loop and lives in the engine's own fixture,
`runtime/streamlib-engine/tests/fixtures/verify_audio_loopback.sh`.
"""

import re
from collections.abc import Callable

import pytest

import tatolab.stream
from block_wiring_streams import microphone_wired_straight_into_a_speaker
from conftest import StreamRunWithNoVulkanDriverOutcome
from runtime_process_under_test import RuntimeProcessUnderTest
from speaker_sink_probes import AudioBlockCountingProbe
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream

UNOPENABLE_DEVICE_ID = "not-a-real-audio-device"
READINESS_TIMEOUT_SECONDS = 10.0
PLAYBACK_READINESS_TIMEOUT_SECONDS = 20.0
SPEAKER_NODE_NAME = "speakersink"

PLAYED_BLOCKS = re.compile(r"played_blocks=(\d+)")
UNDERRUN_BYTES = re.compile(r"underrun_bytes=(\d+)")
DROPPED_BLOCKS = re.compile(r"dropped_blocks=(\d+)")

# Eight device periods of stereo `f32` at the PipeWire arm's 1024-sample
# quantum. A cold start costs two or three of these and then nothing more; a
# stream running without a cushion loses one every few blocks, which over the
# hundred blocks this waits for is an order of magnitude past this.
UNDERRUN_BYTES_A_COLD_START_MAY_COST = 8 * 1024 * 2 * 4


@stream
def one_speaker_sink_left_unnamed(stream_builder: StreamBuilder) -> None:
    stream_builder.add(tatolab.stream.SpeakerSink)


@stream
def microphone_into_a_speaker_and_a_block_counting_probe(stream_builder: StreamBuilder) -> None:
    """A microphone wired straight to a speaker, with no Python in the sample path.

    `stream_builder.add` with no `config` on either end records `{}`. The two
    ends need not agree on rate, channels or dtype, and on a stock machine they
    do not: the ALSA arm asks a capture device for mono and a playback device
    for stereo. `SpeakerSink`'s input port declares `audio_window =
    match_device`, so the engine converts every block into whatever format the
    speaker's own device opened at.

    The probe hangs off the same output the speaker reads, so the test has a
    marker saying enough blocks have really flowed rather than a sleep guessing
    that they have. It is a second consumer of the microphone's port, not a
    stage between the two built-ins.
    """
    microphone = stream_builder.add(tatolab.stream.MicrophoneSource)
    speaker = stream_builder.add(tatolab.stream.SpeakerSink, name=SPEAKER_NODE_NAME)
    stream_builder.connect(microphone.output("audio"), speaker.input("audio"))

    probe = stream_builder.add(AudioBlockCountingProbe)
    stream_builder.connect(microphone.output("audio"), probe.input("audio_from_upstream"))


@stream
def speaker_sink_naming_an_unopenable_device(stream_builder: StreamBuilder) -> None:
    stream_builder.add(tatolab.stream.SpeakerSink, config={"device_id": UNOPENABLE_DEVICE_ID})


def the_speakers_settled_window_contract(graph: dict, speaker_node_name: str) -> object:
    """What `graph` renders for the speaker's `audio` port's window contract."""
    (speaker,) = [node for node in graph["nodes"] if node["name"] == speaker_node_name]
    audio = next(
        (port for port in speaker["ports"]["inputs"] if port["name"] == "audio"),
        None,
    )
    assert audio is not None, f"the speaker node renders no `audio` input port: {speaker}"
    return audio.get("audio_window")


# ---- built-in class semantics (no GPU) -------------------------------------


def test_node_name_defaults_to_the_type_name(
    run_stream_on_tatolabd_with_no_vulkan_driver: "Callable[..., StreamRunWithNoVulkanDriverOutcome]",
):
    graph = compile_stream_to_graph(one_speaker_sink_left_unnamed)
    assert [node["name"] for node in graph["nodes"]] == [SPEAKER_NODE_NAME]

    outcome = run_stream_on_tatolabd_with_no_vulkan_driver(graph)
    assert outcome.loaded and outcome.loaded_node_count == 1, outcome.tatolab_run_stderr_text


def test_the_speaker_declares_the_input_a_microphone_can_be_wired_to(
    run_stream_on_tatolabd_with_no_vulkan_driver: "Callable[..., StreamRunWithNoVulkanDriverOutcome]",
):
    """The two audio built-ins have to compose without an adapter between them,
    which is what makes one `stream_builder.connect(microphone.output("audio"),
    speaker.input("audio"))` the whole of wiring audio through. The builder
    checks no port names, so the engine accepting the load is the proof."""
    outcome = run_stream_on_tatolabd_with_no_vulkan_driver(microphone_wired_straight_into_a_speaker)
    assert outcome.loaded and outcome.loaded_node_count == 2, outcome.tatolab_run_stderr_text


# ---- the native block in a real graph (GPU) --------------------------------


@pytest.mark.requires_gpu
@pytest.mark.audible_on_macos(
    reason="the default microphone wired straight to the default speaker feeds back without headphones"
)
def test_a_microphone_wired_to_a_speaker_runs_and_plays_what_it_captured(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]",
):
    """The playback path end to end: built-in class → native registration → the
    probed backend's playback stream, fed over a real link by the capture
    built-in, with no interpreter anywhere in the sample path.

    `stream_builder.add` with no `config` records `{}`, so this is also the
    added-without-config proof.

    On the ALSA arm the default source and default sink disagree on format on
    every machine: the capture side asks for mono and the playback side for
    stereo by construction. The speaker's port declares `audio_window =
    match_device`, so the engine converts rather than refusing, and that
    disagreement is the case this asserts.
    """
    tatolabd = start_tatolabd(microphone_into_a_speaker_and_a_block_counting_probe)
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(timeout=PLAYBACK_READINESS_TIMEOUT_SECONDS)
    # What the sentinel settled to, read back off the live graph: the values are
    # this machine's device format, which is the whole reason the port declares
    # `match_device` instead of five written values.
    rendered = the_speakers_settled_window_contract(
        local_api.call_tool("graph"), SPEAKER_NODE_NAME
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

    # Nothing is lost between the two built-ins on this run. Block counts no
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

    # Only the rendering's shape is fixed; its values are this machine's.
    assert rendered is not None, "the speaker's port rendered no window contract at all"
    assert isinstance(rendered, dict), rendered
    assert rendered["resolved_from"] == "device", (
        f"graph must render the values the device settled, said to have come from the "
        f"device rather than from an author: {rendered}"
    )
    assert rendered["window_size"] == rendered["hop"], (
        f"a sink converts format rather than re-framing, so window and hop are one "
        f"device period: {rendered}"
    )
    for field in ("sample_rate", "channels", "window_size"):
        assert rendered[field] > 0, f"`{field}` must be the device's own: {rendered}"
    assert rendered["dtype"] in ("f32", "i16"), rendered

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
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]",
):
    """A machine with no audio is a supported environment; a wrong device id is
    a wiring error. Playing into a different speaker than the one named would be
    worse than failing, so the sink refuses and the processor never reaches
    Running."""
    tatolabd = start_tatolabd(speaker_sink_naming_an_unopenable_device)
    node_states = tatolabd.local_api_client().await_every_node_past_setup(
        timeout=READINESS_TIMEOUT_SECONDS
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert node_states[SPEAKER_NODE_NAME] == "Error", (
        f"the speaker must refuse at setup rather than reach Running: {node_states}"
    )
    assert UNOPENABLE_DEVICE_ID in tatolabd.stderr_text, (
        f"the refusal must name the device that was asked for:\n{tatolabd.recent_stderr()}"
    )

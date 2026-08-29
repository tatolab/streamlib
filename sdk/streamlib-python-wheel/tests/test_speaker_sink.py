# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`streamlib.SpeakerSink` — the playback built-in, marker to device callback.

The marker tests are pure Python. The graph tests boot a real engine, which
initializes a GPU context, so they carry `requires_gpu` like every other graph
test here.

Deliberately arm-agnostic: the backend chain picks whichever arm the machine
running this actually has, and these assertions hold on all of them — the last
arm needs no audio library at all, so this still runs in a container. Nothing
here states a rate or a channel count either, because the speaker's port
declares `audio_window = match_device` and the answer is this machine's. What
is *not* asserted here is that a tone played out comes back recognisable; that
needs a device on both ends of a loop and lives in the engine's own fixture,
`runtime/streamlib-engine/tests/fixtures/verify_audio_loopback.sh`.
"""

import json
import re
from pathlib import Path

import pytest

import streamlib
from speaker_sink_named_device_app import UNOPENABLE_DEVICE_ID

SPEAKER_SINK_APP = Path(__file__).parent / "speaker_sink_app.py"
NAMED_DEVICE_APP = Path(__file__).parent / "speaker_sink_named_device_app.py"

PLAYED_BLOCKS = re.compile(r"played_blocks=(\d+)")
UNDERRUN_BYTES = re.compile(r"underrun_bytes=(\d+)")
DROPPED_BLOCKS = re.compile(r"dropped_blocks=(\d+)")
SETTLED_AUDIO_WINDOW = re.compile(r"MARKER:SPEAKER_AUDIO_WINDOW (.+)")

# Eight device periods of stereo `f32` at the PipeWire arm's 1024-sample
# quantum. A cold start costs two or three of these and then nothing more; a
# stream running without a cushion loses one every few blocks, which over the
# hundred blocks this waits for is an order of magnitude past this.
UNDERRUN_BYTES_A_COLD_START_MAY_COST = 8 * 1024 * 2 * 4


# ---- marker semantics (no GPU) ---------------------------------------------


def test_the_marker_class_cannot_be_instantiated():
    with pytest.raises(TypeError):
        streamlib.SpeakerSink()


def test_display_name_defaults_to_the_type_name():
    runtime = streamlib.Runtime()
    try:
        speaker = runtime.add(streamlib.SpeakerSink)
        assert speaker.display_name == "SpeakerSink"
    finally:
        runtime.shutdown()


def test_the_speaker_declares_the_input_a_microphone_can_be_wired_to():
    """The two audio built-ins have to compose without an adapter between them,
    which is what makes `rt.connect(mic.output("audio"), speaker.input("audio"))`
    the whole of wiring audio through."""
    runtime = streamlib.Runtime()
    try:
        microphone = runtime.add(streamlib.MicrophoneSource)
        speaker = runtime.add(streamlib.SpeakerSink)
        runtime.connect(microphone.output("audio"), speaker.input("audio"))
    finally:
        runtime.shutdown()


# ---- the native block in a real graph (GPU) --------------------------------


@pytest.mark.requires_gpu
def test_a_microphone_wired_to_a_speaker_runs_and_plays_what_it_captured(
    start_app_under_test,
):
    """The playback path end to end: marker class → native registration → the
    probed backend's playback stream, fed over a real link by the capture
    built-in, with no interpreter anywhere in the sample path.

    Added with no `config`, so this is also the added-without-config proof.

    This used to skip on a machine whose default source and default sink
    disagreed on format — which, on the ALSA arm, is every machine: the capture
    side asks for mono and the playback side for stereo by construction. The
    speaker's port now declares `audio_window = match_device`, so the engine
    converts rather than refusing, and the case that used to be unanswerable is
    the one this asserts.
    """
    app = start_app_under_test(SPEAKER_SINK_APP)
    app.await_marker("EVERY_PROCESSOR_RUNNING")

    # Blocks really moving on the port the speaker reads, rather than a sleep
    # guessing that they are.
    app.await_output_containing(
        "MARKER:BLOCKS_COUNTED", "enough blocks to judge a playback run by"
    )
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    assert "SpeakerSink: playback stream opened" in app.output, (
        f"the speaker never opened a device:\n{app.output}"
    )

    played = PLAYED_BLOCKS.search(app.output)
    assert played is not None, f"no SpeakerSink teardown line:\n{app.output}"
    assert int(played.group(1)) > 0, (
        f"the speaker reached Running but was never given a block:\n{app.output}"
    )

    # Nothing is lost between the two built-ins on this run. Block counts no
    # longer compare: the speaker plays windows the stage cut to its own
    # device's period, and the microphone publishes blocks at its own device's,
    # so the two are equal only by coincidence of format. What still holds is
    # loss at each end — nothing dropped at the capture edge, and the underrun
    # bound below for everything between.
    dropped = DROPPED_BLOCKS.search(app.output)
    assert dropped is not None, f"no MicrophoneSource teardown line:\n{app.output}"
    assert int(dropped.group(1)) == 0, (
        f"the microphone dropped {dropped.group(1)} blocks at the device edge, so the run "
        f"was not keeping up:\n{app.output}"
    )

    # What the sentinel settled to, read back off the live graph rather than
    # asserted as numbers: the values are this machine's device format, which
    # is the whole reason the port declares `match_device` instead of five
    # written values. Only their shape is fixed.
    settled = SETTLED_AUDIO_WINDOW.search(app.output)
    assert settled is not None, f"the app never reported the graph's rendering:\n{app.output}"
    rendered = json.loads(settled.group(1))
    assert rendered is not None, (
        f"the speaker's port rendered no window contract at all:\n{app.output}"
    )
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
    underrun = UNDERRUN_BYTES.search(app.output)
    assert underrun is not None, f"no underrun count in the teardown line:\n{app.output}"
    assert int(underrun.group(1)) <= UNDERRUN_BYTES_A_COLD_START_MAY_COST, (
        f"the device was given {underrun.group(1)} bytes of silence across "
        f"{played.group(1)} played blocks — the cushion is not holding:\n{app.output}"
    )


@pytest.mark.requires_gpu
def test_a_device_that_was_named_and_cannot_be_opened_refuses_at_setup(
    start_app_under_test,
):
    """A machine with no audio is a supported environment; a wrong device id is
    a wiring error. Playing into a different speaker than the one named would be
    worse than failing, so the sink refuses and the processor never reaches
    Running."""
    app = start_app_under_test(NAMED_DEVICE_APP)
    app.await_output_containing(
        "MARKER:NOT_EVERY_PROCESSOR_RUNNING", "the readiness wait to report Error"
    )
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    assert "MARKER:EVERY_PROCESSOR_RUNNING" not in app.output
    assert UNOPENABLE_DEVICE_ID in app.output, (
        f"the refusal must name the device that was asked for:\n{app.output}"
    )

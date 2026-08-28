# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`streamlib.SpeakerSink` — the playback built-in, marker to device callback.

The marker tests are pure Python. The graph tests boot a real engine, which
initializes a GPU context, so they carry `requires_gpu` like every other graph
test here.

Deliberately arm-agnostic: the backend chain picks whichever arm the machine
running this actually has, and these assertions hold on all of them — the last
arm needs no audio library at all, so this still runs in a container. What is
*not* asserted here is that a tone played out comes back recognisable; that
needs a device on both ends of a loop and lives in the engine's own fixture,
`runtime/streamlib-engine/tests/fixtures/verify_audio_loopback.sh`.
"""

import re
from pathlib import Path

import pytest

import streamlib
from speaker_sink_named_device_app import UNOPENABLE_DEVICE_ID

SPEAKER_SINK_APP = Path(__file__).parent / "speaker_sink_app.py"
NAMED_DEVICE_APP = Path(__file__).parent / "speaker_sink_named_device_app.py"

PLAYED_BLOCKS = re.compile(r"\"played_blocks\":\s*(\d+)")


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
    """
    app = start_app_under_test(SPEAKER_SINK_APP)
    app.await_output_containing(
        "MARKER:EVERY_PROCESSOR_RUNNING", "the speaker to open its device"
    )
    # Long enough for many device periods to have been asked for and served.
    app.await_output_containing("SpeakerSink: playback stream opened", "the open log line")
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    # A refusal is the designed answer to a format mismatch, so it must not be
    # mistaken for a pass: if it fired, nothing was played and the assertion
    # below would be measuring an empty run.
    assert "cannot be played on a device running at" not in app.output, (
        f"the microphone's format is not one this machine's speaker can play; "
        f"there is no resampler on this rung:\n{app.output}"
    )

    played = PLAYED_BLOCKS.search(app.output)
    assert played is not None, f"no SpeakerSink teardown line:\n{app.output}"
    assert int(played.group(1)) > 0, (
        f"the speaker reached Running but was never given a block:\n{app.output}"
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

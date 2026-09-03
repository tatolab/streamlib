# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`Mp4Sink` from Python, marker class to a file with two tracks in it.

The marker tests are pure Python — constructing a `Runtime` and wiring a graph
needs no device, which is why they run in CI. The recording test starts one,
so it carries `requires_gpu` like every other graph test here and runs nowhere
in CI: writing an MP4 needs no device, but a running processor does.

No camera and no microphone. What this suite proves is the container and the
track-per-link rule reached from Python, so both tracks are Opus over a tone
whose format the source states — the file's contents are the test's own fact
rather than the rig's. The bytes inside the boxes are locked GPU-free by the
engine's own `mp4_fragmented_file_writer` tests; what only a running graph can
show is that two links became two tracks named after their producers, and that
the file was already playable while the run was still going.

The recording is read back with `cargo xtask mp4-inspect`, the same reader
`/verify-video` and the fixture scripts use, so nothing here needs ffprobe.
"""

import json
import re
import subprocess
import time
from pathlib import Path

import pytest

import streamlib
from streamlib import Mp4Sink

MP4_SINK_APP = Path(__file__).parent / "mp4_sink_app.py"
REPOSITORY_ROOT = Path(__file__).resolve().parents[3]

RECORDED_TRACK_NAMES = re.compile(r"MARKER:RECORDED_TRACK_NAMES (\[.*\])")

# Long enough to cross several of the writer's audio-only fragment spans on a
# loaded rig, and bounded so a sink that never closes a fragment fails here
# rather than hanging the suite.
LONGEST_WAIT_FOR_A_GROWING_RECORDING_SECONDS = 60.0
RECORDING_POLL_INTERVAL_SECONDS = 0.25

# With no video track wired, the writer closes a fragment every second — so a
# track carrying less than one span never closed one, whatever the file says.
SHORTEST_CREDIBLE_TRACK_SECONDS = 1.0

# How far the two tracks may disagree. They are fed by two sources started
# together, so this bounds one track stalling rather than the start skew
# between two helper processes, which is milliseconds.
WIDEST_CREDIBLE_DISAGREEMENT_BETWEEN_THE_TRACKS_SECONDS = 1.0

# The recording cannot outlast the run that wrote it; the slack covers the
# publishing lead the source runs at and the last fragment teardown closes.
SLACK_OVER_THE_OBSERVED_RUN_SECONDS = 2.0


@pytest.fixture(scope="module")
def mp4_inspect_binary():
    """The release `xtask`, built once for the whole module.

    Built rather than run through `cargo run` per call: the recording is
    inspected in a polling loop, and paying cargo's resolve on every poll
    would make the loop measure the build rather than the sink.
    """
    build = subprocess.run(
        ["cargo", "build", "--release", "--locked", "--package", "xtask"],
        cwd=REPOSITORY_ROOT,
        capture_output=True,
        text=True,
    )
    assert build.returncode == 0, f"xtask did not build:\n{build.stderr}"
    return REPOSITORY_ROOT / "target" / "release" / "xtask"


def inspect_recording(mp4_inspect_binary, recording_path):
    """The inspector's report, or `None` while the file is not yet readable.

    A recording is inspected while it is still being written, so "no `moov`
    yet" and "a box whose bytes are still in the writer's buffer" are ordinary
    states of a healthy run rather than failures — they read as not-yet-here
    and the caller polls again.
    """
    inspected = subprocess.run(
        [str(mp4_inspect_binary), "mp4-inspect", str(recording_path)],
        capture_output=True,
        text=True,
    )
    if inspected.returncode != 0:
        return None
    return json.loads(inspected.stdout)


def await_recording_with_at_least(mp4_inspect_binary, recording_path, fragments, app):
    """Poll the live file until it parses with `fragments` closed fragments."""
    deadline = time.monotonic() + LONGEST_WAIT_FOR_A_GROWING_RECORDING_SECONDS
    report = None
    while time.monotonic() < deadline:
        report = inspect_recording(mp4_inspect_binary, recording_path)
        if report is not None and report["fragment_count"] >= fragments:
            return report
        time.sleep(RECORDING_POLL_INTERVAL_SECONDS)
    raise AssertionError(
        f"{recording_path} never reached {fragments} closed fragments within "
        f"{LONGEST_WAIT_FOR_A_GROWING_RECORDING_SECONDS}s; last report was "
        f"{report}; app output:\n{app.output}"
    )


# ---- marker semantics (no GPU) ---------------------------------------------


def test_the_marker_class_cannot_be_instantiated():
    with pytest.raises(TypeError):
        Mp4Sink()


def test_display_name_defaults_to_the_type_name(tmp_path):
    runtime = streamlib.Runtime()
    try:
        block = runtime.add(
            Mp4Sink, config={"path": str(tmp_path / "recording.mp4")}
        )
        assert block.display_name == "Mp4Sink"
    finally:
        runtime.shutdown()


def test_two_encoders_wire_into_the_one_input_without_an_adapter(tmp_path):
    """Two producers into `tracks`, and no fan-in machinery between them.

    This is the whole authoring surface for a two-track recording: the sink
    declares one input, any number of links may enter it, and each becomes a
    track. A second `rt.connect` into the same port is the second track.
    """
    runtime = streamlib.Runtime()
    try:
        sink = runtime.add(
            Mp4Sink, config={"path": str(tmp_path / "recording.mp4")}
        )
        for _ in range(2):
            microphone = runtime.add(streamlib.MicrophoneSource)
            encoder = runtime.add(streamlib.OpusEncoder)
            runtime.connect(microphone.output("audio"), encoder.input("audio"))
            runtime.connect(encoder.output("encoded_audio"), sink.input("tracks"))
    finally:
        runtime.shutdown()


# ---- a real recording (GPU) ------------------------------------------------


@pytest.mark.requires_gpu
def test_two_sources_record_two_tracks_named_after_their_producers(
    start_app_under_test, mp4_inspect_binary, tmp_path
):
    """Two tone streams into one sink, read back out of the written file.

    The file is inspected twice while the graph is still running — once as
    soon as it parses, once after another fragment has closed — because that
    is the property the fragmented layout exists for: a recording is playable
    up to its last closed fragment at every instant of the run, not only after
    a clean teardown. The final inspection then adds what only a clean stop
    gives, which is the open fragment closed and every track's duration whole.
    """
    recording_path = tmp_path / "recording.mp4"
    app = start_app_under_test(MP4_SINK_APP, "--path", str(recording_path))
    app.await_marker("EVERY_PROCESSOR_RUNNING")
    recording_started_at = time.monotonic()

    named_tracks = RECORDED_TRACK_NAMES.findall(app.output)
    assert named_tracks, f"the app never named its tracks; output:\n{app.output}"
    expected_track_names = json.loads(named_tracks[-1])
    assert len(expected_track_names) == 2

    while_running = await_recording_with_at_least(
        mp4_inspect_binary, recording_path, 1, app
    )
    assert len(while_running["tracks"]) == 2, (
        "the `moov` describes every track before the first fragment lands, so "
        "a mid-run file already names both; it described "
        f"{while_running['tracks']}"
    )
    grown = await_recording_with_at_least(
        mp4_inspect_binary,
        recording_path,
        while_running["fragment_count"] + 1,
        app,
    )

    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    observed_run_seconds = time.monotonic() - recording_started_at

    recorded = inspect_recording(mp4_inspect_binary, recording_path)
    assert recorded is not None, f"{recording_path} did not parse after a clean stop"
    assert recorded["fragment_count"] >= grown["fragment_count"], (
        "teardown closes the open fragment, so the finished file can never "
        "carry fewer than the run was already observed to have written"
    )

    tracks = recorded["tracks"]
    assert len(tracks) == 2, (
        f"two links entered `tracks`, so the file owes two tracks; it has {tracks}"
    )
    assert [track["name"] for track in tracks] == expected_track_names, (
        "each track is named by the channel its link subscribed to, which is "
        "what makes a recording self-describing"
    )

    for track in tracks:
        assert track["handler"] == "soun", (
            "the track's kind follows its bags' `codec`, and `opus` is audio"
        )
        assert track["sample_entry"]["kind"] == "Opus"
        assert track["sample_entry"]["output_channel_count"] == 2, (
            "the source publishes stereo and the encoder follows it, so the "
            "`dOps` carries two channels"
        )
        assert track["sample_entry"]["pre_skip"] > 0, (
            "`dOps` PreSkip is the encoder's reported lookahead; a zero would "
            "mean the sample entry was built without asking libopus"
        )
        assert track["samples"] > 0
        assert track["duration_seconds"] >= SHORTEST_CREDIBLE_TRACK_SECONDS, (
            f"{track['name']} recorded {track['duration_seconds']}s, less than "
            "the one-second span the writer closes an audio-only fragment at"
        )
        assert (
            track["duration_seconds"]
            <= observed_run_seconds + SLACK_OVER_THE_OBSERVED_RUN_SECONDS
        ), (
            f"{track['name']} claims {track['duration_seconds']}s of audio out "
            f"of a {observed_run_seconds:.2f}s run"
        )

    first_seconds, second_seconds = (track["duration_seconds"] for track in tracks)
    assert (
        abs(first_seconds - second_seconds)
        < WIDEST_CREDIBLE_DISAGREEMENT_BETWEEN_THE_TRACKS_SECONDS
    ), (
        "both tracks were fed by sources started together and stopped at one "
        f"SIGINT, so {first_seconds}s against {second_seconds}s is one of them "
        "having stalled rather than start skew"
    )

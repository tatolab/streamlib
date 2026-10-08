# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`Mp4Sink` from Python, built-in class to a file with two tracks in it.

The load tests need no device: `tatolabd` loads the graph and is then refused
at the GPU, which is why they run in CI. The recording test starts the engine,
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
import subprocess
import time
from collections.abc import Callable
from pathlib import Path

import pytest

import tatolab.stream
from conftest import StreamGraphLoadOutcome
from opus_blocks_probes import StereoToneSource
from runtime_process_under_test import RuntimeProcessUnderTest
from runtime_unit_under_test import REPOSITORY_ROOT
from tatolab.stream import Mp4Sink, StreamBuilder, compile_stream_to_graph, stream

# Long enough to cross several of the writer's audio-only fragment spans on a
# loaded rig, and bounded so a sink that never closes a fragment fails here
# rather than hanging the suite.
LONGEST_WAIT_FOR_A_GROWING_RECORDING_SECONDS = 60.0
RECORDING_POLL_INTERVAL_SECONDS = 0.25
READINESS_TIMEOUT_SECONDS = 20.0

# With no video track wired, the writer closes a fragment every second — so a
# track carrying less than one span never closed one, whatever the file says.
SHORTEST_CREDIBLE_TRACK_SECONDS = 1.0

# How far the two tracks may disagree. They are fed by two sources started
# together, so this bounds one track stalling rather than the start skew
# between two processor interpreters, which is milliseconds.
WIDEST_CREDIBLE_DISAGREEMENT_BETWEEN_THE_TRACKS_SECONDS = 1.0

# The recording cannot outlast the process that wrote it. The clock below
# starts before `tatolabd` is spawned, so the only thing this slack has to
# cover is the source's own publishing lead — it runs ahead of the monotonic
# clock, so the last stamp it wrote can name an instant a little past now.
SLACK_OVER_THE_OBSERVED_RUN_SECONDS = 2.0

# The sink opens its file at `setup()`, which a graph loaded and never run does
# not reach, so nothing is ever written here.
NEVER_OPENED_RECORDING_PATH = "/nonexistent-streamlib-test/never-opened.mp4"

RECORDER_NODE_NAME = "recorder"

# What each recorded pair is called in the graph. Two entries, because the
# file owes one track per inbound link and this is the list of them.
RECORDED_PAIR_NAMES = ("first", "second")


@stream
def one_mp4_sink_left_unnamed(stream_builder: StreamBuilder) -> None:
    stream_builder.add(Mp4Sink, config={"path": NEVER_OPENED_RECORDING_PATH})


@stream
def two_microphone_encoder_pairs_into_one_mp4_sink(stream_builder: StreamBuilder) -> None:
    sink = stream_builder.add(Mp4Sink, config={"path": NEVER_OPENED_RECORDING_PATH})
    for _ in range(2):
        microphone = stream_builder.add(tatolab.stream.MicrophoneSource)
        encoder = stream_builder.add(tatolab.stream.OpusEncoder)
        stream_builder.connect(microphone.output("audio"), encoder.input("audio"))
        stream_builder.connect(encoder.output("encoded_audio"), sink.input("tracks"))


@stream
def two_tone_pairs_recorded_into_one_mp4(stream_builder: StreamBuilder) -> None:
    """Two independent tone streams recorded into one file, one track each.

    `StereoToneSource -> OpusEncoder` twice, both encoders into the single
    `tracks` input of one `Mp4Sink`. Nothing configures the second track: the
    sink enumerates its inbound links at `setup()` and each one becomes a track
    named by the channel it subscribed to.

    Two sources rather than one fanned out, because a fan-out is one channel
    with two subscribers and would be one track.
    """
    sink = stream_builder.add(
        tatolab.stream.Mp4Sink,
        name=RECORDER_NODE_NAME,
        config={"path": NEVER_OPENED_RECORDING_PATH},
    )
    for pair_name in RECORDED_PAIR_NAMES:
        source = stream_builder.add(StereoToneSource, name=f"{pair_name}_tone")
        encoder = stream_builder.add(tatolab.stream.OpusEncoder, name=f"{pair_name}_encoder")
        stream_builder.connect(source.output("audio"), encoder.input("audio"))
        stream_builder.connect(encoder.output("encoded_audio"), sink.input("tracks"))


def two_tone_pairs_recorded_into(recording_path: Path) -> dict:
    """`two_tone_pairs_recorded_into_one_mp4`'s graph, its recorder writing to `recording_path`."""
    graph = compile_stream_to_graph(two_tone_pairs_recorded_into_one_mp4)
    (recorder,) = [node for node in graph["nodes"] if node["name"] == RECORDER_NODE_NAME]
    recorder["config"]["path"] = str(recording_path)
    return graph


def recorded_track_names(live_graph: dict) -> "list[str]":
    """The name each pair's track will carry, in `RECORDED_PAIR_NAMES` order.

    A track is named by the channel its link subscribed to: the producing
    processor's id lowercased over its output port — what `graph` and `tap`
    show. The test cannot derive it from the stream, because a channel name
    carries the producer's engine-minted processor id, not its node name.
    """
    processor_id_by_node_name = {node["name"]: node["id"] for node in live_graph["nodes"]}
    return [
        f"{processor_id_by_node_name[f'{pair_name}_encoder'].lower()}/encoded_audio"
        for pair_name in RECORDED_PAIR_NAMES
    ]


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


def await_recording_with_at_least(
    mp4_inspect_binary, recording_path, fragments, tatolabd: RuntimeProcessUnderTest
):
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
        f"{report}; tatolabd's standard error:\n{tatolabd.recent_stderr()}"
    )


# ---- built-in class semantics (no GPU) -------------------------------------


def test_node_name_defaults_to_the_type_name(
    load_stream_graph_on_tatolabd: "Callable[..., StreamGraphLoadOutcome]",
):
    graph = compile_stream_to_graph(one_mp4_sink_left_unnamed)
    assert [node["name"] for node in graph["nodes"]] == ["mp4sink"]

    outcome = load_stream_graph_on_tatolabd(graph)
    assert outcome.loaded and outcome.loaded_node_count == 1, outcome.stderr_text


def test_two_encoders_wire_into_the_one_input_without_an_adapter(
    load_stream_graph_on_tatolabd: "Callable[..., StreamGraphLoadOutcome]",
):
    """Two producers into `tracks`, and no fan-in machinery between them.

    This is the whole authoring surface for a two-track recording: the sink
    declares one input, any number of links may enter it, and each becomes a
    track. A second `stream_builder.connect` into the same port is the second track.
    The builder checks no port names, so the engine accepting the load is the
    proof.
    """
    outcome = load_stream_graph_on_tatolabd(two_microphone_encoder_pairs_into_one_mp4_sink)
    assert outcome.loaded and outcome.loaded_node_count == 5, outcome.stderr_text


# ---- a real recording (GPU) ------------------------------------------------


@pytest.mark.requires_gpu
def test_two_sources_record_two_tracks_named_after_their_producers(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]", mp4_inspect_binary, tmp_path
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
    # Before the spawn, not after readiness: the sink opens the file in
    # `setup()`, which is over by the time every node is Running, so a clock
    # started there would under-measure the run it is bounding the file against.
    run_started_at = time.monotonic()
    tatolabd = start_tatolabd(two_tone_pairs_recorded_into(recording_path))
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(timeout=READINESS_TIMEOUT_SECONDS)

    expected_track_names = recorded_track_names(local_api.call_tool("graph"))
    assert len(expected_track_names) == 2

    while_running = await_recording_with_at_least(mp4_inspect_binary, recording_path, 1, tatolabd)
    assert len(while_running["tracks"]) == 2, (
        "the `moov` describes every track before the first fragment lands, so "
        "a mid-run file already names both; it described "
        f"{while_running['tracks']}"
    )
    grown = await_recording_with_at_least(
        mp4_inspect_binary,
        recording_path,
        while_running["fragment_count"] + 1,
        tatolabd,
    )

    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    observed_run_seconds = time.monotonic() - run_started_at

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
        assert track["duration_seconds"] <= observed_run_seconds + SLACK_OVER_THE_OBSERVED_RUN_SECONDS, (
            f"{track['name']} claims {track['duration_seconds']}s of audio out "
            f"of a {observed_run_seconds:.2f}s run"
        )

    first_seconds, second_seconds = (track["duration_seconds"] for track in tracks)
    assert abs(first_seconds - second_seconds) < WIDEST_CREDIBLE_DISAGREEMENT_BETWEEN_THE_TRACKS_SECONDS, (
        "both tracks were fed by sources started together and stopped at one "
        f"SIGINT, so {first_seconds}s against {second_seconds}s is one of them "
        "having stalled rather than start skew"
    )

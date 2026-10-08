# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The window contract reaching a Python consumer in its processor interpreter.

The declaration surface is #2032's; this is the stage delivering on it in the
placement every Python processor actually runs in. The contract crosses the
wiring envelope from `tatolabd` beside `read_mode`, and the processor
interpreter's own `InputMailboxesInner` — the same Rust `tatolabd`'s mailboxes
run — resamples, mixes down and frames before `process()` ever sees a bag.

Every test starts the engine, which initializes a GPU context, so they carry
`requires_gpu` like every other graph test here. How a processor interpreter
reads the envelope itself, with no engine, is the wheel suite's
`test_audio_window_stage.py`.
"""

import time
from collections.abc import Callable
from pathlib import Path

import pytest

import tatolab.stream
from audio_window_probes import (
    DeclaredMonoWindowProbe,
    ExactWindowProbe,
    RollingWindowProbe,
    SourceFollowingWindowProbe,
    StereoToneSource,
)
from local_api_client import LocalApiClient
from runtime_process_under_test import ENGINE_STARTED_LOG_LINE, RuntimeProcessUnderTest
from tatolab.stream import StreamBuilder, stream

pytestmark = pytest.mark.requires_gpu

# What `StereoToneSource` publishes, and what the two probes must each make of
# it: the same stereo blocks, one following the count and one converting it.
STEREO_SOURCE_SAMPLE_RATE = 48_000
STEREO_SOURCE_CHANNELS = 2
SOURCE_FOLLOWING_WINDOW_SIZE = 960
SOURCE_FOLLOWING_HOP_NS = 20_000_000

# The contract both probes declare, and what it makes each window worth.
CONTRACT_SAMPLE_RATE = 16_000
CONTRACT_WINDOW_SIZE = 512
CONTIGUOUS_HOP_NS = 32_000_000
ROLLING_HOP_NS = 10_000_000

DISCARDED_SAMPLES_TIMEOUT_SECONDS = 30.0
METRICS_POLL_INTERVAL_SECONDS = 0.2


def _microphone_into(stream_builder: StreamBuilder, probe_class: type) -> None:
    microphone = stream_builder.add(tatolab.stream.MicrophoneSource)
    probe = stream_builder.add(probe_class)
    stream_builder.connect(microphone.output("audio"), probe.input("audio_from_upstream"))


@stream
def microphone_into_an_exact_window_probe(stream_builder: StreamBuilder) -> None:
    _microphone_into(stream_builder, ExactWindowProbe)


@stream
def microphone_into_a_rolling_window_probe(stream_builder: StreamBuilder) -> None:
    _microphone_into(stream_builder, RollingWindowProbe)


@stream
def one_stereo_source_into_both_window_probes(stream_builder: StreamBuilder) -> None:
    """One stated-format source into two consumers: one that declares no
    channel count and one that declares mono.

    A Python source rather than the microphone, because what is under test is
    that the count follows *the source* — which needs a source whose count the
    test knows.
    """
    source = stream_builder.add(StereoToneSource)
    following = stream_builder.add(SourceFollowingWindowProbe)
    declared_mono = stream_builder.add(DeclaredMonoWindowProbe)
    stream_builder.connect(source.output("audio"), following.input("audio_from_upstream"))
    stream_builder.connect(source.output("audio"), declared_mono.input("audio_from_upstream"))


def run_until(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]",
    stream_function: object,
    awaited_marker_name: str,
) -> RuntimeProcessUnderTest:
    tatolabd = start_tatolabd(stream_function)
    tatolabd.await_marker(awaited_marker_name)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    return tatolabd


def readings_from(tatolabd: RuntimeProcessUnderTest, marker_name: str, what: str):
    readings = tatolabd.marker_payloads(marker_name)
    assert readings and isinstance(readings[0], list), (
        f"no parseable {what} report:\n{tatolabd.recent_stderr()}"
    )
    return readings[0]


def assert_windows_are_contiguous_once_the_run_has_settled(readings):
    """Every window after the first advances by exactly one window's duration.

    The first step is excluded, and deliberately: a Python source publishes as
    soon as `process()` first runs, which can be before the consumer's
    interpreter has its subscriber live, so the opening blocks are lost on the
    producer's ring and the stage flushes and re-anchors — leaving one wide step
    between the pre-flush window and the run that follows. That loss is the
    plan's open question about uncounted publisher-side drops, not something
    the window contract promises against. What the contract does promise is
    contiguity *within* a run, and asserting from the second window on still
    catches a flush anywhere later in the stream.
    """
    stamps = [reading["first_sample_timestamp_ns"] for reading in readings]
    steps = [later - earlier for earlier, later in zip(stamps, stamps[1:])]
    assert len(steps) >= 2, f"too few windows to judge a cadence: {stamps}"
    assert steps[1:] == [SOURCE_FOLLOWING_HOP_NS] * len(steps[1:]), (
        "960 samples at 48 kHz is exactly 20 ms whatever the channel count — "
        f"got {steps}"
    )


def assert_every_window_matches_the_contract(readings):
    for reading in readings:
        assert reading["sample_count"] == CONTRACT_WINDOW_SIZE, (
            "the contract's whole promise is an exact-size block; a short or long "
            f"one means the stage handed over a partial window: {reading}"
        )
        assert reading["sample_rate"] == CONTRACT_SAMPLE_RATE
        assert reading["channels"] == 1
        assert reading["dtype"] == "f32"
        assert reading["shape"] == [CONTRACT_WINDOW_SIZE, 1]


def test_a_helper_placed_consumer_reads_exact_windows_at_the_rate_it_declared(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]",
):
    """A device capturing at its own rate reaches a 16 kHz mono 512/512 port as
    exactly-512-sample windows 32 ms apart, in a processor interpreter.

    The rate the machine's device settles on is whatever it settles on; what the
    contract promises is that the consumer never sees it.
    """
    tatolabd = run_until(start_tatolabd, microphone_into_an_exact_window_probe, "WINDOWS_SEEN")
    readings = readings_from(tatolabd, "WINDOWS_SEEN", "window")
    assert len(readings) >= 2, "the cadence assertion needs two windows to subtract"
    assert_every_window_matches_the_contract(readings)

    # Checked before the arithmetic: a discontinuity flush is a legitimate
    # outcome and re-anchors the run, so it should fail by name rather than as a
    # confusing subtraction.
    assert "flushed rather than emitting a window that spans the gap" not in tatolabd.stderr_text, (
        f"the stage flushed while the probe was reporting:\n{tatolabd.recent_stderr()}"
    )
    assert "dropped at the device edge" not in tatolabd.stderr_text, (
        f"the source dropped blocks while the probe was reporting:\n{tatolabd.recent_stderr()}"
    )

    stamps = [reading["first_sample_timestamp_ns"] for reading in readings]
    steps = [later - earlier for earlier, later in zip(stamps, stamps[1:])]
    assert steps == [CONTIGUOUS_HOP_NS] * len(steps), (
        "512 samples at 16 kHz is exactly 32 ms, and every stamp derives from one "
        f"anchor within a contiguous run — got {steps}"
    )


def test_a_hop_below_the_window_rolls_at_the_hops_cadence_not_the_windows(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]",
):
    """A rolling window is still exact-size; only its cadence changes."""
    tatolabd = run_until(
        start_tatolabd, microphone_into_a_rolling_window_probe, "ROLLING_WINDOWS_SEEN"
    )
    readings = readings_from(tatolabd, "ROLLING_WINDOWS_SEEN", "rolling window")
    assert len(readings) >= 2
    assert_every_window_matches_the_contract(readings)

    assert "flushed rather than emitting a window that spans the gap" not in tatolabd.stderr_text, (
        f"the stage flushed while the probe was reporting:\n{tatolabd.recent_stderr()}"
    )

    stamps = [reading["first_sample_timestamp_ns"] for reading in readings]
    steps = [later - earlier for earlier, later in zip(stamps, stamps[1:])]
    assert steps == [ROLLING_HOP_NS] * len(steps), (
        "a hop of 160 at 16 kHz advances by exactly 10 ms while each window still "
        f"carries 512 samples — got {steps}"
    )


def test_a_helper_placed_consumer_with_no_declared_count_reads_the_sources_own(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]",
):
    """A contract stating everything but its count carries the source's stereo
    through to a processor interpreter, over a real link.

    Its sibling in the same graph declares mono off the same source, so one run
    shows both that following follows and that declaring still converts.
    """
    tatolabd = start_tatolabd(one_stereo_source_into_both_window_probes)
    tatolabd.await_every_marker("SOURCE_FOLLOWING_WINDOWS_SEEN", "DECLARED_MONO_WINDOWS_SEEN")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    following = readings_from(tatolabd, "SOURCE_FOLLOWING_WINDOWS_SEEN", "source-following window")
    assert len(following) >= 3
    for reading in following:
        assert reading["channels"] == STEREO_SOURCE_CHANNELS, (
            "the contract declared no count, so every window must carry the "
            f"source's own: {reading}"
        )
        assert reading["sample_count"] == SOURCE_FOLLOWING_WINDOW_SIZE
        assert reading["sample_rate"] == STEREO_SOURCE_SAMPLE_RATE
        assert reading["dtype"] == "f32"
        assert reading["shape"] == [
            SOURCE_FOLLOWING_WINDOW_SIZE,
            STEREO_SOURCE_CHANNELS,
        ]

    assert_windows_are_contiguous_once_the_run_has_settled(following)

    declared_mono = readings_from(tatolabd, "DECLARED_MONO_WINDOWS_SEEN", "declared-mono window")
    assert len(declared_mono) >= 3
    for reading in declared_mono:
        assert reading["channels"] == 1, (
            "a declared count is still converted to by the fixed rule, off the "
            f"same stereo source: {reading}"
        )
        assert reading["shape"] == [SOURCE_FOLLOWING_WINDOW_SIZE, 1]
    assert_windows_are_contiguous_once_the_run_has_settled(declared_mono)


GAPPED_AUDIO_PROCESSORS_SOURCE = '''\
"""A mono source whose every block starts a second after the last one ended."""

from tatolab.stream import (
    AudioBlock,
    AudioWindowContract,
    RuntimeContextLimitedAccess,
    monotonic_now_ns,
    node,
)

SAMPLE_RATE = 16_000
FRAMES_PER_BLOCK = 300
GAP_BETWEEN_BLOCKS_NS = 1_000_000_000


@node(execution="continuous", interval_ms=20)
class GappedMonoSource:
    """Each block is short of a 512-sample window, and each one's stamp is a
    discontinuity, so the windowed consumer flushes what the last block left."""

    def __init__(self) -> None:
        self.blocks = 0
        self.anchor_ns = monotonic_now_ns()

    @node.output()
    def audio(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.outputs.write(
            "audio",
            {
                "samples": bytes(4 * FRAMES_PER_BLOCK),
                "sample_rate": SAMPLE_RATE,
                "channels": 1,
                "sample_count": FRAMES_PER_BLOCK,
                "dtype": "f32",
                "first_sample_timestamp_ns": self.anchor_ns
                + self.blocks * GAP_BETWEEN_BLOCKS_NS,
            },
        )
        self.blocks += 1


@node
class WindowedMonoConsumer:
    @node.input(
        delivery_profile="ordered",
        audio_window=AudioWindowContract(
            sample_rate=SAMPLE_RATE, channels=1, dtype="f32", window_size=512
        ),
    )
    def audio(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("audio", into=AudioBlock)
'''

GAPPED_AUDIO_STREAM_SOURCE = '''\
from tatolab.stream import StreamBuilder, stream

from processors.gapped_audio import GappedMonoSource, WindowedMonoConsumer


@stream
def main(stream_builder: StreamBuilder) -> None:
    source = stream_builder.add(GappedMonoSource, name="gapped-source")
    consumer = stream_builder.add(WindowedMonoConsumer, name="windowed-consumer")
    stream_builder.connect(source.output("audio"), consumer.input("audio"))
'''


def node_named(graph: dict, name: str) -> dict:
    """The one node carrying `name`, or a failure naming what is there."""
    matches = [node for node in graph["nodes"] if node["name"] == name]
    assert len(matches) == 1, (
        f"expected exactly one node named {name!r}; graph names "
        f"{[node['name'] for node in graph['nodes']]}"
    )
    return matches[0]


def the_link_into(graph: dict, name: str) -> str:
    """The id of the one link into `name`."""
    node_named(graph, name)
    link_ids = [link["id"] for link in graph["links"] if link["target"]["node"] == name]
    assert len(link_ids) == 1, f"expected one link into {name!r}: {graph['links']}"
    return link_ids[0]


def await_metrics_satisfying(
    local_api: LocalApiClient,
    name: str,
    satisfied: "Callable[[dict], bool]",
    awaited: str,
    tatolab: RuntimeProcessUnderTest,
) -> dict:
    """Poll `graph` until `name`'s metrics satisfy `satisfied`."""
    deadline = time.monotonic() + DISCARDED_SAMPLES_TIMEOUT_SECONDS
    metrics: dict = {}
    while time.monotonic() < deadline:
        metrics = node_named(local_api.call_tool("graph"), name)["components"].get("metrics", {})
        if satisfied(metrics):
            return metrics
        time.sleep(METRICS_POLL_INTERVAL_SECONDS)
    raise AssertionError(
        f"{name!r} never rendered {awaited} within {DISCARDED_SAMPLES_TIMEOUT_SECONDS}s; "
        f"its metrics were {metrics}\n{tatolab.recent_stderr()}"
    )


def test_a_helper_placed_windowed_consumers_flush_renders_its_discarded_samples_on_its_link(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
):
    """A Python windowed consumer flushes in its own processor interpreter, and
    its node renders the samples each flush discarded on the one link feeding
    the port, beside a bag count the flushes never enter.

    Fail-without-fix: mirror only the dropped-bag count onto the board, and
    `discarded_samples_by_link` renders zero however many flushes ran.
    """
    app_directory = make_tatolab_project(
        {
            "processors/__init__.py": "",
            "processors/gapped_audio.py": GAPPED_AUDIO_PROCESSORS_SOURCE,
            "stream.py": GAPPED_AUDIO_STREAM_SOURCE,
        }
    )
    tatolab = start_tatolab("run", working_directory=app_directory)
    local_api = tatolab.local_api_client()
    tatolab.await_stderr_containing(ENGINE_STARTED_LOG_LINE)
    link_id = the_link_into(local_api.call_tool("graph"), "windowed-consumer")

    metrics = await_metrics_satisfying(
        local_api,
        "windowed-consumer",
        lambda metrics: metrics.get("discarded_samples_by_link", {}).get(link_id, 0) > 0,
        f"discarded samples on {link_id}",
        tatolab,
    )
    tatolab.interrupt()
    assert tatolab.await_exit() == 0, (
        f"`tatolab run` must exit cleanly on SIGINT:\n{tatolab.recent_stderr()}"
    )

    assert set(metrics) == {
        "frames_dropped",
        "dropped_bags_by_link",
        "discarded_samples_by_link",
        "refused_bags_by_output_port",
    }, metrics
    assert list(metrics["discarded_samples_by_link"]) == [link_id]
    assert metrics["discarded_samples_by_link"][link_id] % 300 == 0, (
        "each flush discards the one 300-sample block the last gap left staged: "
        f"{metrics}"
    )

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The window contract reaching a helper-placed Python consumer.

The declaration surface is #2032's; this is the stage delivering on it in the
placement every Python processor actually runs in. The contract crosses the
parent→child wiring envelope beside `read_mode`, and the child's own
`InputMailboxesInner` — the same Rust the parent's mailboxes run — resamples,
mixes down and frames before `process()` ever sees a bag.

The graph tests boot a real engine, which initializes a GPU context, so they
carry `requires_gpu` like every other graph test here. The wiring-refusal tests
below need `/dev/shm` and nothing else, so they run in CI.
"""

import json
import re
from pathlib import Path

import pytest

from streamlib import ProcessorLinkDataAccess
from test_cli_launch import (  # noqa: F401 — the two fixtures are used by name
    NODE_READY_TIMEOUT_SECONDS,
    await_sole_registry_entry,
    free_port,
    isolated_runtime_directory,
    launch_node,
)
from test_helper_loss_counts import await_metrics_satisfying, mcp_json, the_link_into

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")

APP = Path(__file__).parent / "audio_window_app.py"

WINDOWS_SEEN = re.compile(r"MARKER:WINDOWS_SEEN (\[.*\])")
ROLLING_WINDOWS_SEEN = re.compile(r"MARKER:ROLLING_WINDOWS_SEEN (\[.*\])")
SOURCE_FOLLOWING_WINDOWS_SEEN = re.compile(
    r"MARKER:SOURCE_FOLLOWING_WINDOWS_SEEN (\[.*\])"
)
DECLARED_MONO_WINDOWS_SEEN = re.compile(r"MARKER:DECLARED_MONO_WINDOWS_SEEN (\[.*\])")

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


def run_until(start_app_under_test, scenario: str, awaited: str, what: str):
    app = start_app_under_test(APP, scenario)
    app.await_output_containing(awaited, what)
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    return app


def readings_from(app, pattern, what: str):
    match = pattern.search(app.output)
    assert match is not None, f"no parseable {what} report:\n{app.output}"
    return json.loads(match.group(1))


def assert_windows_are_contiguous_once_the_run_has_settled(readings):
    """Every window after the first advances by exactly one window's duration.

    The first step is excluded, and deliberately: a Python source publishes as
    soon as `process()` first runs, which can be before the consumer's child
    has its subscriber live, so the opening blocks are lost on the producer's
    ring and the stage flushes and re-anchors — leaving one wide step between
    the pre-flush window and the run that follows. That loss is the plan's
    open question about uncounted publisher-side drops, not something the
    window contract promises against. What the contract does promise is
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


@pytest.mark.requires_gpu
def test_a_helper_placed_consumer_reads_exact_windows_at_the_rate_it_declared(
    start_app_under_test,
):
    """A device capturing at its own rate reaches a 16 kHz mono 512/512 port as
    exactly-512-sample windows 32 ms apart, in a child process.

    The rate the machine's device settles on is whatever it settles on; what the
    contract promises is that the consumer never sees it.
    """
    app = run_until(
        start_app_under_test,
        "contiguous_windows",
        "MARKER:WINDOWS_SEEN",
        "the probe's first windows",
    )
    readings = readings_from(app, WINDOWS_SEEN, "window")
    assert len(readings) >= 2, "the cadence assertion needs two windows to subtract"
    assert_every_window_matches_the_contract(readings)

    # Checked before the arithmetic: a discontinuity flush is a legitimate
    # outcome and re-anchors the run, so it should fail by name rather than as a
    # confusing subtraction.
    assert "flushed rather than emitting a window that spans the gap" not in app.output, (
        f"the stage flushed while the probe was reporting:\n{app.output}"
    )
    assert "dropped at the device edge" not in app.output, (
        f"the source dropped blocks while the probe was reporting:\n{app.output}"
    )

    stamps = [reading["first_sample_timestamp_ns"] for reading in readings]
    steps = [later - earlier for earlier, later in zip(stamps, stamps[1:])]
    assert steps == [CONTIGUOUS_HOP_NS] * len(steps), (
        "512 samples at 16 kHz is exactly 32 ms, and every stamp derives from one "
        f"anchor within a contiguous run — got {steps}"
    )


@pytest.mark.requires_gpu
def test_a_hop_below_the_window_rolls_at_the_hops_cadence_not_the_windows(
    start_app_under_test,
):
    """A rolling window is still exact-size; only its cadence changes."""
    app = run_until(
        start_app_under_test,
        "rolling_windows",
        "MARKER:ROLLING_WINDOWS_SEEN",
        "the probe's first rolling windows",
    )
    readings = readings_from(app, ROLLING_WINDOWS_SEEN, "rolling window")
    assert len(readings) >= 2
    assert_every_window_matches_the_contract(readings)

    assert "flushed rather than emitting a window that spans the gap" not in app.output, (
        f"the stage flushed while the probe was reporting:\n{app.output}"
    )

    stamps = [reading["first_sample_timestamp_ns"] for reading in readings]
    steps = [later - earlier for earlier, later in zip(stamps, stamps[1:])]
    assert steps == [ROLLING_HOP_NS] * len(steps), (
        "a hop of 160 at 16 kHz advances by exactly 10 ms while each window still "
        f"carries 512 samples — got {steps}"
    )


@pytest.mark.requires_gpu
def test_a_helper_placed_consumer_with_no_declared_count_reads_the_sources_own(
    start_app_under_test,
):
    """A contract stating everything but its count carries the source's stereo
    through to a child process, over a real link.

    Its sibling in the same graph declares mono off the same source, so one run
    shows both that following follows and that declaring still converts.
    """
    app = run_until(
        start_app_under_test,
        "source_following_windows",
        "MARKER:DECLARED_MONO_WINDOWS_SEEN",
        "both probes' first windows",
    )

    following = readings_from(
        app, SOURCE_FOLLOWING_WINDOWS_SEEN, "source-following window"
    )
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

    declared_mono = readings_from(
        app, DECLARED_MONO_WINDOWS_SEEN, "declared-mono window"
    )
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

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    AudioBlock,
    AudioWindowContract,
    RuntimeContextLimitedAccess,
    input,
    monotonic_now_ns,
    output,
    processor,
)

SAMPLE_RATE = 16_000
FRAMES_PER_BLOCK = 300
GAP_BETWEEN_BLOCKS_NS = 1_000_000_000


@processor(execution="continuous", interval_ms=20)
class GappedMonoSource:
    """Each block is short of a 512-sample window, and each one's stamp is a
    discontinuity, so the windowed consumer flushes what the last block left."""

    def __init__(self) -> None:
        self.blocks = 0
        self.anchor_ns = monotonic_now_ns()

    @output()
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


@processor
class WindowedMonoConsumer:
    @input(
        delivery_profile="ordered",
        audio_window=AudioWindowContract(
            sample_rate=SAMPLE_RATE, channels=1, dtype="f32", window_size=512
        ),
    )
    def audio(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("audio", into=AudioBlock)
'''

GAPPED_AUDIO_APP_SOURCE = '''\
from streamlib import Runtime

from processors.gapped_audio import GappedMonoSource, WindowedMonoConsumer


def setup(rt: Runtime) -> None:
    source = rt.add(GappedMonoSource, display_name="gapped source")
    consumer = rt.add(WindowedMonoConsumer, display_name="windowed consumer")
    rt.connect(source.output("audio"), consumer.input("audio"))
'''


@pytest.mark.requires_gpu
def test_a_helper_placed_windowed_consumers_flush_renders_its_discarded_samples_on_its_link(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """A Python windowed consumer flushes in its own process, and its node
    renders the samples each flush discarded on the one link feeding the port,
    beside a bag count the flushes never enter.

    Fail-without-fix: mirror only the dropped-bag count onto the board, and
    `discarded_samples_by_link` renders zero however many flushes ran.
    """
    app_directory = tmp_path / "app"
    (app_directory / "processors").mkdir(parents=True)
    (app_directory / "processors" / "__init__.py").write_text("")
    (app_directory / "processors" / "gapped_audio.py").write_text(
        GAPPED_AUDIO_PROCESSORS_SOURCE
    )
    (app_directory / "app.py").write_text(GAPPED_AUDIO_APP_SOURCE)
    node = launch_node("run", app_directory, free_port(), capture_output=True)
    control_url = await_sole_registry_entry(
        isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS
    )["control_url"]
    node.await_captured_output_containing("[start] Runtime started", NODE_READY_TIMEOUT_SECONDS)
    link_id = the_link_into(mcp_json(control_url, "graph", {}), "windowed consumer")

    metrics = await_metrics_satisfying(
        control_url,
        "windowed consumer",
        lambda metrics: metrics.get("discarded_samples_by_link", {}).get(link_id, 0) > 0,
        f"discarded samples on {link_id}",
        node,
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


# ---- the child's own reading of the envelope (no device, no GPU) ------------


def a_helper_process_data_plane() -> ProcessorLinkDataAccess:
    """The object a child builds for itself, with its own iceoryx2 node."""
    return ProcessorLinkDataAccess()


def wire_with_window(data_plane: ProcessorLinkDataAccess, audio_window) -> None:
    data_plane.wire_input_link(
        "audio",
        "streamlib/tests/audio-window/never-opened",
        "streamlib/tests/audio-window/never-opened",
        "streamlib/tests/audio-window/never-notified",
        "read_next_in_order",
        16,
        16,
        2,
        1,
        "L-test",
        audio_window,
    )


def test_a_child_refuses_a_window_contract_whose_field_it_cannot_read():
    """The parent sends the contract resolved; a key it got wrong is named here
    rather than surfacing as an anonymous decode failure."""
    data_plane = a_helper_process_data_plane()

    with pytest.raises(ValueError) as refusal:
        wire_with_window(data_plane, {"sample_rate": 16_000, "channels": 1})

    rendered = str(refusal.value)
    assert "audio" in rendered and "dtype" in rendered, (
        f"the refusal must name the port and the field; got {rendered}"
    )


def test_a_child_reads_a_contract_that_follows_the_sources_channels():
    """The count is the one value the envelope may spell as a word, and the
    child must read it rather than refusing what the parent legitimately sent."""
    data_plane = a_helper_process_data_plane()

    wire_with_window(
        data_plane,
        {
            "sample_rate": 48_000,
            "channels": "source",
            "dtype": "f32",
            "window_size": 960,
            "hop": 960,
        },
    )


def test_a_child_reads_a_contract_whose_channels_key_the_parent_omitted():
    """An omitted key means the same thing as the spelled one, so a terser
    writer is not refused for terseness."""
    data_plane = a_helper_process_data_plane()

    wire_with_window(
        data_plane,
        {"sample_rate": 48_000, "dtype": "f32", "window_size": 960, "hop": 960},
    )


def test_a_child_refuses_a_channels_value_that_names_no_count():
    """A word that is not the one spelling is a writer that meant something
    else, and guessing which count is the reshaping the contract refuses."""
    data_plane = a_helper_process_data_plane()

    with pytest.raises(ValueError) as refusal:
        wire_with_window(
            data_plane,
            {
                "sample_rate": 48_000,
                "channels": "stereo",
                "dtype": "f32",
                "window_size": 960,
                "hop": 960,
            },
        )

    rendered = str(refusal.value)
    assert "channels" in rendered and "source" in rendered, (
        f"the refusal must name the field and the spelling that works; got {rendered}"
    )


def test_a_child_refuses_a_bool_where_a_channel_count_belongs():
    """`bool` is an `int` subclass, so `True` would otherwise wire as one
    channel — a plausible count nobody wrote. The declaration constructor
    refuses one by name; the envelope must too, since nothing else guards it."""
    data_plane = a_helper_process_data_plane()

    with pytest.raises(TypeError) as refusal:
        wire_with_window(
            data_plane,
            {
                "sample_rate": 48_000,
                "channels": True,
                "dtype": "f32",
                "window_size": 960,
                "hop": 960,
            },
        )

    rendered = str(refusal.value)
    assert "channels" in rendered and "bool" in rendered, (
        f"the refusal must name the field and the kind; got {rendered}"
    )


def test_a_child_refuses_a_window_contract_the_stage_could_not_honour():
    """The same validator both languages' declaration paths call, applied again
    where the child receives the contract: a hop above the window would silently
    discard the samples between windows."""
    data_plane = a_helper_process_data_plane()

    with pytest.raises(ValueError) as refusal:
        wire_with_window(
            data_plane,
            {
                "sample_rate": 16_000,
                "channels": 1,
                "dtype": "f32",
                "window_size": 512,
                "hop": 1_024,
            },
        )

    rendered = str(refusal.value)
    assert "512" in rendered and "1024" in rendered, (
        f"the refusal must name both numbers; got {rendered}"
    )

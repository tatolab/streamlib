# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_helper_placement.py` runs from the suite project."""

from tatolab.stream import StreamBuilder, TestPatternSource, stream

from helper_placement_processors import (
    DiesAbruptlyProbe,
    ForksAWorkerThatOutlivesItProbe,
    ReportsItsOwnProcessSource,
    ReportsItsOwnProcessVideoSink,
    ReportsUpstreamProcessSink,
    SleepsThroughItsOwnSetupProbe,
    SleepsThroughItsOwnShutdownProbe,
)


def _labelled_source_into_sink(
    stream_builder: StreamBuilder, label: str, *, sink_name: "str | None" = None
) -> None:
    source = stream_builder.add(ReportsItsOwnProcessSource, config={"label": label})
    sink = stream_builder.add(ReportsUpstreamProcessSink, name=sink_name)
    stream_builder.connect(source.output("frames_to_downstream"), sink.input("frames_from_upstream"))


@stream
def first_labelled_source_into_sink(stream_builder: StreamBuilder) -> None:
    """A source labelled `first` into a sink reporting where its bags came from."""
    _labelled_source_into_sink(stream_builder, "first")


@stream
def only_labelled_source_into_sink(stream_builder: StreamBuilder) -> None:
    """A source labelled `only` into a sink reporting where its bags came from."""
    _labelled_source_into_sink(stream_builder, "only")


@stream
def reaped_labelled_source_into_sink(stream_builder: StreamBuilder) -> None:
    """A source labelled `reaped` into a sink reporting where its bags came from."""
    _labelled_source_into_sink(stream_builder, "reaped")


@stream
def two_labelled_sources_each_into_its_own_sink(stream_builder: StreamBuilder) -> None:
    """Two instances of one source class, each into a sink named for its label."""
    for label in ("first", "second"):
        _labelled_source_into_sink(stream_builder, label, sink_name=f"{label}Sink")


@stream
def dies_abruptly_beside_a_survivor_pair(stream_builder: StreamBuilder) -> None:
    """A processor that takes its own process down, beside a source-sink pair."""
    stream_builder.add(DiesAbruptlyProbe)
    _labelled_source_into_sink(stream_builder, "survivor")


@stream
def stale_build_labelled_source(stream_builder: StreamBuilder) -> None:
    """A lone source labelled `stale`, for a helper made to see another build."""
    stream_builder.add(ReportsItsOwnProcessSource, config={"label": "stale"})


@stream
def native_test_pattern_into_python_video_sink(stream_builder: StreamBuilder) -> None:
    """A native 64x32 test pattern into a Python sink reporting its own process."""
    pattern = stream_builder.add(TestPatternSource, config={"width": 64, "height": 32})
    sink = stream_builder.add(ReportsItsOwnProcessVideoSink)
    stream_builder.connect(pattern.output("video"), sink.input("video_from_upstream"))


@stream
def one_probe_sleeping_through_its_own_shutdown(stream_builder: StreamBuilder) -> None:
    """A processor parked in `process()` when shutdown arrives."""
    stream_builder.add(SleepsThroughItsOwnShutdownProbe)


@stream
def one_probe_forking_a_worker_that_outlives_it(stream_builder: StreamBuilder) -> None:
    """A processor that forks a worker meant to outlive its helper."""
    stream_builder.add(ForksAWorkerThatOutlivesItProbe)


@stream
def one_probe_sleeping_through_its_own_setup(stream_builder: StreamBuilder) -> None:
    """A processor still inside `setup()` when shutdown arrives."""
    stream_builder.add(SleepsThroughItsOwnSetupProbe)

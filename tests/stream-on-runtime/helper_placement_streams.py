# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_helper_placement.py` runs from the suite project."""

from tatolab.ops import Lfo, Remap
from tatolab.stream import ChannelsToRecords, SteppedStreamClock, StreamBuilder, stream

from helper_placement_processors import (
    STEPPED_LFO_STEP_MS,
    STEPPED_LFO_STEPS_REPORTED,
    CountsItsOwnCalls,
    DiesAbruptlyProbe,
    ForksAWorkerThatOutlivesItProbe,
    ReportsItsOwnProcessSource,
    ReportsItsOwnProcessRecordsSink,
    ReportsTheCountItReadAndDiesOnceAFreshCountReachesTwenty,
    ReportsTheFunctionNodeValuesItReadSink,
    ReportsTheLfoValueOfEachStepSink,
    ReportsTheRawMessagePayloadItReadSink,
    ReportsUpstreamProcessSink,
    SleepsThroughItsOwnSetupProbe,
    SleepsThroughItsOwnShutdownProbe,
    SpinsForever,
    WritesRawMessagePayloadSource,
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
def raw_message_payload_source_into_sink(stream_builder: StreamBuilder) -> None:
    """A Python source writing a non-msgpack payload into a Python sink reporting it."""
    source = stream_builder.add(WritesRawMessagePayloadSource)
    sink = stream_builder.add(ReportsTheRawMessagePayloadItReadSink)
    stream_builder.connect(
        source.output("raw_messages_to_downstream"), sink.input("raw_messages_from_upstream")
    )


@stream
def native_channels_to_records_into_python_records_sink(stream_builder: StreamBuilder) -> None:
    """A native Channels-to-Records converter into a Python sink reporting its own process."""
    to_records = stream_builder.add(ChannelsToRecords, config={"channel_names": ["level"]})
    sink = stream_builder.add(ReportsItsOwnProcessRecordsSink)
    stream_builder.connect(to_records.output("records"), sink.input("records_from_upstream"))


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


@stream
def lfo_into_remap_into_python_sink(stream_builder: StreamBuilder) -> None:
    """`tatolab.ops`' `Lfo` into `Remap` into a Python class reading the value they evaluate to."""
    lfo = stream_builder.add(Lfo)
    remap = stream_builder.add(Remap)
    sink = stream_builder.add(ReportsTheFunctionNodeValuesItReadSink)
    stream_builder.connect(lfo.output("result"), remap.input("x"))
    stream_builder.connect(remap.output("result"), sink.input("brightness"))


@stream
def function_node_that_spins_forever_into_python_sink(stream_builder: StreamBuilder) -> None:
    """A function node whose compiled call never returns, inside the read of a Python class."""
    spinner = stream_builder.add(SpinsForever, name="spinner")
    sink = stream_builder.add(ReportsTheFunctionNodeValuesItReadSink, name="sink")
    stream_builder.connect(spinner.output("result"), sink.input("brightness"))


@stream
def stateful_counter_into_python_sink_that_dies_once(stream_builder: StreamBuilder) -> None:
    """A stateful function node counting its calls, read by a Python class that
    takes its own helper down once a fresh count reaches twenty."""
    counter = stream_builder.add(CountsItsOwnCalls, name="counter")
    sink = stream_builder.add(ReportsTheCountItReadAndDiesOnceAFreshCountReachesTwenty, name="sink")
    stream_builder.connect(counter.output("result"), sink.input("count"))


@stream
def two_lfos_each_read_by_its_own_python_sink_on_a_stepped_clock(
    stream_builder: StreamBuilder,
) -> None:
    """The same `Lfo` island twice, each read by its own Python class, on an unpaced stepped clock."""
    stream_builder.run_on_clock(
        SteppedStreamClock(
            step_ns=STEPPED_LFO_STEP_MS * 1_000_000,
            stop_after_steps=STEPPED_LFO_STEPS_REPORTED * 4,
        )
    )
    for run in ("first", "second"):
        lfo = stream_builder.add(Lfo, name=f"{run}_lfo")
        sink = stream_builder.add(ReportsTheLfoValueOfEachStepSink, name=f"{run}_sink")
        stream_builder.connect(lfo.output("result"), sink.input("wave"))

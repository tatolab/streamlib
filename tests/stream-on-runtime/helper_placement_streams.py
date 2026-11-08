# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@graph_generator` functions `test_helper_placement.py` runs from the suite project."""

from rust_function_node_fixture import CountsItsOwnCalls as RustCountsItsOwnCalls
from rust_function_node_fixture import ScaleAndOffset
from tatolab.ops import Clamp, Gate, Lfo, Remap
from tatolab.stream import ChannelsToRecords, SteppedStreamClock, StreamBuilder, graph_generator

from helper_placement_processors import (
    STEPPED_LFO_STEP_MS,
    STEPPED_LFO_STEPS_REPORTED,
    CountsEachTickRecordsSource,
    CountsItsOwnCalls,
    CrashesItsHelper,
    DiesAbruptlyProbe,
    HoldsItsConfiguredLevelCountingItsCalls,
    LabelsTheLevel,
    LevelScaledByItsGain,
    ForksAWorkerThatOutlivesItProbe,
    ReportsEachBatchOfFunctionNodeValuesItReadSink,
    ReportsEachBatchOfGatedCountsItReadSink,
    ReportsEachBatchOfItsConfiguredLevel,
    ReportsEachBatchOfPinValuesItReadSink,
    ReportsItsOwnProcessSource,
    ReportsItsOwnProcessRecordsSink,
    ReportsTheCountItReadAndDiesOnceAFreshCountReachesTwenty,
    ReportsTheCountsItReadInOrderSink,
    ReportsTheFunctionNodeValuesItReadSink,
    ReportsTheLevelRecordsItReadSink,
    ReportsTheLfoValueOfEachStepSink,
    ReportsTheRawMessagePayloadItReadSink,
    ReportsTheRecordsItReadSink,
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


@graph_generator
def first_labelled_source_into_sink(stream_builder: StreamBuilder) -> None:
    """A source labelled `first` into a sink reporting where its bags came from."""
    _labelled_source_into_sink(stream_builder, "first")


@graph_generator
def only_labelled_source_into_sink(stream_builder: StreamBuilder) -> None:
    """A source labelled `only` into a sink reporting where its bags came from."""
    _labelled_source_into_sink(stream_builder, "only")


@graph_generator
def reaped_labelled_source_into_sink(stream_builder: StreamBuilder) -> None:
    """A source labelled `reaped` into a sink reporting where its bags came from."""
    _labelled_source_into_sink(stream_builder, "reaped")


@graph_generator
def two_labelled_sources_each_into_its_own_sink(stream_builder: StreamBuilder) -> None:
    """Two instances of one source class, each into a sink named for its label."""
    for label in ("first", "second"):
        _labelled_source_into_sink(stream_builder, label, sink_name=f"{label}Sink")


@graph_generator
def dies_abruptly_beside_a_survivor_pair(stream_builder: StreamBuilder) -> None:
    """A processor that takes its own process down, beside a source-sink pair."""
    stream_builder.add(DiesAbruptlyProbe)
    _labelled_source_into_sink(stream_builder, "survivor")


@graph_generator
def stale_build_labelled_source(stream_builder: StreamBuilder) -> None:
    """A lone source labelled `stale`, for a helper made to see another build."""
    stream_builder.add(ReportsItsOwnProcessSource, config={"label": "stale"})


@graph_generator
def raw_message_payload_source_into_sink(stream_builder: StreamBuilder) -> None:
    """A Python source writing a non-msgpack payload into a Python sink reporting it."""
    source = stream_builder.add(WritesRawMessagePayloadSource)
    sink = stream_builder.add(ReportsTheRawMessagePayloadItReadSink)
    stream_builder.connect(
        source.output("raw_messages_to_downstream"), sink.input("raw_messages_from_upstream")
    )


@graph_generator
def native_channels_to_records_into_python_records_sink(stream_builder: StreamBuilder) -> None:
    """A native Channels-to-Records converter into a Python sink reporting its own process."""
    to_records = stream_builder.add(ChannelsToRecords, config={"channel_names": ["level"]})
    sink = stream_builder.add(ReportsItsOwnProcessRecordsSink)
    stream_builder.connect(to_records.output("records"), sink.input("records_from_upstream"))


@graph_generator
def one_probe_sleeping_through_its_own_shutdown(stream_builder: StreamBuilder) -> None:
    """A processor parked in `process()` when shutdown arrives."""
    stream_builder.add(SleepsThroughItsOwnShutdownProbe)


@graph_generator
def one_probe_forking_a_worker_that_outlives_it(stream_builder: StreamBuilder) -> None:
    """A processor that forks a worker meant to outlive its helper."""
    stream_builder.add(ForksAWorkerThatOutlivesItProbe)


@graph_generator
def one_probe_sleeping_through_its_own_setup(stream_builder: StreamBuilder) -> None:
    """A processor still inside `setup()` when shutdown arrives."""
    stream_builder.add(SleepsThroughItsOwnSetupProbe)


@graph_generator
def lfo_into_remap_into_python_sink(stream_builder: StreamBuilder) -> None:
    """`tatolab.ops`' `Lfo` into `Remap` into a Python class reading the value they evaluate to."""
    lfo = stream_builder.add(Lfo)
    remap = stream_builder.add(Remap)
    sink = stream_builder.add(ReportsTheFunctionNodeValuesItReadSink)
    stream_builder.connect(lfo.output("result"), remap.input("x"))
    stream_builder.connect(remap.output("result"), sink.input("brightness"))


@graph_generator
def function_nodes_read_by_two_native_converters_into_python_records_sink(
    stream_builder: StreamBuilder,
) -> None:
    """`Lfo` into `Remap` into a native `ChannelsToRecords` into a Python class, and a second `Lfo` into a second converter.

    Neither converter runs in a helper, so neither can evaluate the island it
    reads: both islands go to the stream's one function node pool. Each
    island has an `Lfo` of its own, since a stateful node two readers read runs
    in a process of its own.
    """
    lfo = stream_builder.add(Lfo)
    second_lfo = stream_builder.add(Lfo, name="second-lfo")
    remap = stream_builder.add(Remap)
    first_converter = stream_builder.add(
        ChannelsToRecords, config={"channel_names": ["level"]}, name="first-converter"
    )
    second_converter = stream_builder.add(
        ChannelsToRecords, config={"channel_names": ["level"]}, name="second-converter"
    )
    sink = stream_builder.add(ReportsTheLevelRecordsItReadSink, name="sink")
    stream_builder.connect(lfo.output("result"), remap.input("x"))
    stream_builder.connect(remap.output("result"), first_converter.input("channels"))
    stream_builder.connect(second_lfo.output("result"), second_converter.input("channels"))
    stream_builder.connect(first_converter.output("records"), sink.input("level_records"))


@graph_generator
def lfo_into_python_sink_reporting_each_batch_it_reads(stream_builder: StreamBuilder) -> None:
    """`tatolab.ops`' `Lfo` read directly by a Python class reporting each batch of values."""
    lfo = stream_builder.add(Lfo, name="lfo")
    sink = stream_builder.add(ReportsEachBatchOfFunctionNodeValuesItReadSink, name="sink")
    stream_builder.connect(lfo.output("result"), sink.input("brightness"))


@graph_generator
def clamp_with_its_input_linked_to_nothing_into_python_sink(stream_builder: StreamBuilder) -> None:
    """`tatolab.ops`' `Clamp`, its `x` linked to nothing and unpinned, read by a Python class."""
    clamp = stream_builder.add(Clamp, name="clamp")
    sink = stream_builder.add(ReportsEachBatchOfFunctionNodeValuesItReadSink, name="sink")
    stream_builder.connect(clamp.output("result"), sink.input("brightness"))


@graph_generator
def level_scaled_by_its_gain_into_python_sink(stream_builder: StreamBuilder) -> None:
    """A function node scaling its level by the `gain` of its config, read by a Python class."""
    scaled = stream_builder.add(LevelScaledByItsGain, name="scaled")
    sink = stream_builder.add(ReportsEachBatchOfFunctionNodeValuesItReadSink, name="sink")
    stream_builder.connect(scaled.output("result"), sink.input("brightness"))


@graph_generator
def python_sink_reading_the_pin_value_of_its_unlinked_input(stream_builder: StreamBuilder) -> None:
    """A Python class whose one input is linked to nothing, so it reads its pin value."""
    stream_builder.add(ReportsEachBatchOfPinValuesItReadSink, name="sink")


@graph_generator
def function_node_that_spins_forever_into_python_sink(stream_builder: StreamBuilder) -> None:
    """A function node whose compiled call never returns, inside the read of a Python class."""
    spinner = stream_builder.add(SpinsForever, name="spinner")
    sink = stream_builder.add(ReportsTheFunctionNodeValuesItReadSink, name="sink")
    stream_builder.connect(spinner.output("result"), sink.input("brightness"))


@graph_generator
def function_node_run_as_python_into_python_records_sink(stream_builder: StreamBuilder) -> None:
    """A function node giving a `dict`, which runs as Python, read by a Python class as Records."""
    labeller = stream_builder.add(LabelsTheLevel, name="labeller")
    sink = stream_builder.add(ReportsTheRecordsItReadSink, name="sink")
    stream_builder.connect(labeller.output("result"), sink.input("label"))


@graph_generator
def function_node_run_as_python_that_crashes_into_python_sink(stream_builder: StreamBuilder) -> None:
    """A function node run as Python whose call takes its reader's helper down."""
    crasher = stream_builder.add(CrashesItsHelper, name="crasher")
    sink = stream_builder.add(ReportsTheFunctionNodeValuesItReadSink, name="sink")
    stream_builder.connect(crasher.output("result"), sink.input("brightness"))


@graph_generator
def stateful_counter_into_python_sink_that_dies_once(stream_builder: StreamBuilder) -> None:
    """A stateful function node counting its calls, read by a Python class that
    takes its own helper down once a fresh count reaches twenty."""
    counter = stream_builder.add(CountsItsOwnCalls, name="counter")
    sink = stream_builder.add(ReportsTheCountItReadAndDiesOnceAFreshCountReachesTwenty, name="sink")
    stream_builder.connect(counter.output("result"), sink.input("count"))


@graph_generator
def stateful_counter_read_by_two_python_sinks(stream_builder: StreamBuilder) -> None:
    """One stateful function node counting its calls, read by two Python classes."""
    counter = stream_builder.add(CountsItsOwnCalls, name="counter")
    for reader_name in ("first_reader", "second_reader"):
        reader = stream_builder.add(ReportsTheCountsItReadInOrderSink, name=reader_name)
        stream_builder.connect(counter.output("result"), reader.input("count"))


@graph_generator
def python_class_reporting_its_configured_level(stream_builder: StreamBuilder) -> None:
    """A Python class reading the `level` of its config at each run."""
    stream_builder.add(ReportsEachBatchOfItsConfiguredLevel, name="configured")


@graph_generator
def held_level_read_by_two_python_sinks(stream_builder: StreamBuilder) -> None:
    """A stateful function node holding the `level` of its config, read by two Python classes."""
    held = stream_builder.add(HoldsItsConfiguredLevelCountingItsCalls, name="held")
    for reader_name in ("first_reader", "second_reader"):
        reader = stream_builder.add(ReportsEachBatchOfFunctionNodeValuesItReadSink, name=reader_name)
        stream_builder.connect(held.output("result"), reader.input("brightness"))


@graph_generator
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


@graph_generator
def rust_function_nodes_in_islands_beside_tatolab_ops(stream_builder: StreamBuilder) -> None:
    """`Lfo` into a Rust `ScaleAndOffset` into `Clamp`, and a Rust `CountsItsOwnCalls`
    into `Remap`, each island read by its own Python class."""
    lfo = stream_builder.add(Lfo)
    scale_and_offset = stream_builder.add(ScaleAndOffset)
    clamp = stream_builder.add(Clamp)
    values_sink = stream_builder.add(ReportsTheFunctionNodeValuesItReadSink)
    stream_builder.connect(lfo.output("result"), scale_and_offset.input("x"))
    stream_builder.connect(scale_and_offset.output("result"), clamp.input("x"))
    stream_builder.connect(clamp.output("result"), values_sink.input("brightness"))

    counter = stream_builder.add(RustCountsItsOwnCalls, name="rust_counter")
    remap = stream_builder.add(Remap)
    counts_sink = stream_builder.add(ReportsTheCountsItReadInOrderSink)
    stream_builder.connect(counter.output("result"), remap.input("x"))
    stream_builder.connect(remap.output("result"), counts_sink.input("count"))


@graph_generator
def python_source_through_a_gate_into_python_sink(stream_builder: StreamBuilder) -> None:
    """A Python class's counts through `tatolab.ops`' `Gate` into another Python class."""
    source = stream_builder.add(CountsEachTickRecordsSource, name="source")
    gate = stream_builder.add(Gate, name="gate")
    sink = stream_builder.add(ReportsEachBatchOfGatedCountsItReadSink, name="sink")
    stream_builder.connect(source.output("counts_to_downstream"), gate.input("value"))
    stream_builder.connect(gate.output("result"), sink.input("gated_counts"))

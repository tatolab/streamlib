# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@graph_generator` functions the helper placement tests run from the suite project."""

from typing import Any

from rust_function_node_fixture import CountsItsOwnCalls as RustCountsItsOwnCalls
from rust_function_node_fixture import RustGate
from rust_function_node_fixture import ScaleAndOffset
from helper_placement_function_nodes import Clamp, Gate, Lfo, Remap, Wave
from tatolab.stream import FloatToInteger, SteppedStreamClock, StreamBuilder, graph_generator

from helper_placement_nodes import (
    STEPPED_LFO_STEP_MS,
    STEPPED_LFO_STEPS_REPORTED,
    CountsEachTickAsALevelSource,
    CountsEachTickRecordsSource,
    CountsItsOwnCalls,
    CrashesItsHelper,
    DiesAbruptlyProbe,
    GateThatCrashesItsHelperOnCue,
    HoldsItsLevelCountingItsCalls,
    HangsAfterItsFirstCalls,
    LabelsTheLevel,
    LevelScaledByItsGain,
    LendsTheLabelToItsNativeHalf,
    ForksAWorkerThatOutlivesItProbe,
    ReportsEachBatchOfCountsItReadInOrderAsTheFirstReader,
    ReportsEachBatchOfFunctionNodeValuesItReadSink,
    ReportsEachBatchOfGatedCountsItReadSink,
    ReportsEachBatchOfItsLevel,
    ReportsEachBatchOfPinValuesItReadSink,
    ReportsItsOwnProcessSource,
    ReportsItsOwnProcessIntegerSink,
    ReportsTheCountsItReadInOrderSink,
    ReportsTheCountsItReadSink,
    ReportsTheFunctionNodeValuesItReadSink,
    ReportsTheLevelItIsBuiltWith,
    ReportsTheLevelsItReadSink,
    ReportsTheLfoValueOfEachStepSink,
    ReportsTheRawMessagePayloadItReadSink,
    ReportsTheRecordsItReadSink,
    ReportsUpstreamProcessSink,
    SleepsPastThePoolsFrameInterval,
    SleepsThroughItsOwnSetupProbe,
    SleepsThroughItsOwnShutdownProbe,
    SpinsForever,
    WritesOneWidthSource,
    WritesRawMessagePayloadSource,
)


def _labelled_source_into_sink(
    stream_builder: StreamBuilder, label: str, *, sink_name: "str | None" = None
) -> None:
    source = stream_builder.add(ReportsItsOwnProcessSource, input_values={"label": label})
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
    """A node that takes its own process down, beside a source-sink pair."""
    stream_builder.add(DiesAbruptlyProbe)
    _labelled_source_into_sink(stream_builder, "survivor")


@graph_generator
def stale_build_labelled_source(stream_builder: StreamBuilder) -> None:
    """A lone source labelled `stale`, for a helper made to see another build."""
    stream_builder.add(ReportsItsOwnProcessSource, input_values={"label": "stale"})


@graph_generator
def raw_message_payload_source_into_sink(stream_builder: StreamBuilder) -> None:
    """A Python source writing a non-msgpack payload into a Python sink reporting it."""
    source = stream_builder.add(WritesRawMessagePayloadSource)
    sink = stream_builder.add(ReportsTheRawMessagePayloadItReadSink)
    stream_builder.connect(
        source.output("raw_messages_to_downstream"), sink.input("raw_messages_from_upstream")
    )


@graph_generator
def native_float_to_integer_into_python_integer_sink(stream_builder: StreamBuilder) -> None:
    """A native Float-to-Integer converter into a Python sink reporting its own process."""
    to_integer = stream_builder.add(FloatToInteger)
    sink = stream_builder.add(ReportsItsOwnProcessIntegerSink)
    stream_builder.connect(to_integer.output("cast"), sink.input("integers_from_upstream"))


@graph_generator
def one_probe_sleeping_through_its_own_shutdown(stream_builder: StreamBuilder) -> None:
    """A node parked in `process()` when shutdown arrives."""
    stream_builder.add(SleepsThroughItsOwnShutdownProbe)


@graph_generator
def one_probe_forking_a_worker_that_outlives_it(stream_builder: StreamBuilder) -> None:
    """A node that forks a worker meant to outlive its helper."""
    stream_builder.add(ForksAWorkerThatOutlivesItProbe)


@graph_generator
def one_probe_sleeping_through_its_own_setup(stream_builder: StreamBuilder) -> None:
    """A node still inside `setup()` when shutdown arrives."""
    stream_builder.add(SleepsThroughItsOwnSetupProbe)


@graph_generator
def wave_into_remap_into_python_sink(stream_builder: StreamBuilder) -> None:
    """`Wave` into `Remap` into a Python class reading the value they evaluate to."""
    wave = stream_builder.add(Wave)
    remap = stream_builder.add(Remap)
    sink = stream_builder.add(ReportsTheFunctionNodeValuesItReadSink)
    stream_builder.connect(wave.output("result"), remap.input("x"))
    stream_builder.connect(remap.output("result"), sink.input("brightness"))


@graph_generator
def function_nodes_read_by_two_native_converters_into_python_sink(
    stream_builder: StreamBuilder,
) -> None:
    """`Wave` into `Remap` onto 0..100 into a native `FloatToInteger` into a Python class, and a second `Wave` into a second converter.

    Neither converter runs in a helper, so neither can evaluate the island it
    reads: both islands go to the stream's one function node pool.
    """
    wave = stream_builder.add(Wave)
    second_wave = stream_builder.add(Wave, name="second-wave")
    remap = stream_builder.add(Remap, input_values={"to_high": 100.0})
    first_converter = stream_builder.add(FloatToInteger, name="first-converter")
    second_converter = stream_builder.add(FloatToInteger, name="second-converter")
    sink = stream_builder.add(ReportsTheLevelsItReadSink, name="sink")
    stream_builder.connect(wave.output("result"), remap.input("x"))
    stream_builder.connect(remap.output("result"), first_converter.input("value"))
    stream_builder.connect(second_wave.output("result"), second_converter.input("value"))
    stream_builder.connect(first_converter.output("cast"), sink.input("levels"))


@graph_generator
def wave_into_python_sink_reporting_each_batch_it_reads(stream_builder: StreamBuilder) -> None:
    """`Wave` read directly by a Python class reporting each batch of values."""
    wave = stream_builder.add(Wave, name="wave")
    sink = stream_builder.add(ReportsEachBatchOfFunctionNodeValuesItReadSink, name="sink")
    stream_builder.connect(wave.output("result"), sink.input("brightness"))


@graph_generator
def level_scaled_by_its_gain_into_python_sink(stream_builder: StreamBuilder) -> None:
    """A function node scaling its level by its `gain` input, 0.25, read by a Python class."""
    scaled = stream_builder.add(LevelScaledByItsGain, name="scaled")
    sink = stream_builder.add(ReportsEachBatchOfFunctionNodeValuesItReadSink, name="sink")
    stream_builder.connect(scaled.output("result"), sink.input("brightness"))


@graph_generator
def typed_level_scaled_by_the_gain_its_input_values_hold_into_python_sink(
    stream_builder: StreamBuilder,
) -> None:
    """A typed function node built with `input_values` setting its `gain` to 0.5, read by a Python class."""
    scaled = stream_builder.add(
        LevelScaledByItsGain, name="scaled", input_values={"gain": 0.5}
    )
    sink = stream_builder.add(ReportsEachBatchOfFunctionNodeValuesItReadSink, name="sink")
    stream_builder.connect(scaled.output("result"), sink.input("brightness"))


@graph_generator
def python_sink_reading_the_pin_value_of_its_unlinked_input(stream_builder: StreamBuilder) -> None:
    """A Python class whose one input is linked to nothing, so it reads its pin value."""
    stream_builder.add(ReportsEachBatchOfPinValuesItReadSink, name="sink")


@graph_generator
def function_node_run_as_python_into_a_class_lending_it_to_native_code(
    stream_builder: StreamBuilder,
) -> None:
    """A function node that runs as Python, read by a manual class whose native half reads it."""
    labeller = stream_builder.add(LabelsTheLevel, name="labeller")
    lender = stream_builder.add(LendsTheLabelToItsNativeHalf, name="lender")
    stream_builder.connect(labeller.output("result"), lender.input("label"))


@graph_generator
def function_node_slower_than_the_pools_frame_interval_read_by_a_native_converter(
    stream_builder: StreamBuilder,
) -> None:
    """A function node giving 5 after each call outlasts the pool's frame interval, read by a native `FloatToInteger` into a Python class."""
    sleeper = stream_builder.add(
        SleepsPastThePoolsFrameInterval, name="sleeper", input_values={"level": 5.0}
    )
    converter = stream_builder.add(FloatToInteger, name="converter")
    sink = stream_builder.add(ReportsTheLevelsItReadSink, name="sink")
    stream_builder.connect(sleeper.output("result"), converter.input("value"))
    stream_builder.connect(converter.output("cast"), sink.input("levels"))


@graph_generator
def stateful_counter_beside_a_pooled_node_that_hangs(stream_builder: StreamBuilder) -> None:
    """A stateful counter and a node that hangs after its first calls, each read by a native
    `FloatToInteger`: the hanger runs in the pool, and the counter in a helper of its own."""
    counter = stream_builder.add(CountsItsOwnCalls, name="counter")
    count_converter = stream_builder.add(FloatToInteger, name="count-converter")
    sink = stream_builder.add(ReportsTheCountsItReadSink, name="sink")
    hanger = stream_builder.add(HangsAfterItsFirstCalls, name="hanger")
    hanger_converter = stream_builder.add(FloatToInteger, name="hanger-converter")
    stream_builder.connect(counter.output("result"), count_converter.input("value"))
    stream_builder.connect(count_converter.output("cast"), sink.input("counts"))
    stream_builder.connect(hanger.output("result"), hanger_converter.input("value"))


@graph_generator
def function_node_that_spins_forever_into_python_sink(stream_builder: StreamBuilder) -> None:
    """A function node whose compiled call never returns, inside the read of a Python class."""
    spinner = stream_builder.add(SpinsForever, name="spinner")
    sink = stream_builder.add(ReportsTheFunctionNodeValuesItReadSink, name="sink")
    stream_builder.connect(spinner.output("result"), sink.input("brightness"))


@graph_generator
def function_node_run_as_python_into_python_records_sink(stream_builder: StreamBuilder) -> None:
    """A function node giving a bundle, which runs as Python, read by a Python class's field."""
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
def stateful_counter_read_by_two_python_sinks(stream_builder: StreamBuilder) -> None:
    """One stateful function node counting its calls, read by two Python classes."""
    counter = stream_builder.add(CountsItsOwnCalls, name="counter")
    for reader_name in ("first_reader", "second_reader"):
        reader = stream_builder.add(ReportsTheCountsItReadInOrderSink, name=reader_name)
        stream_builder.connect(counter.output("result"), reader.input("count"))


@graph_generator
def stateful_counter_read_by_one_python_sink_reporting_each_batch(
    stream_builder: StreamBuilder,
) -> None:
    """One stateful function node counting its calls, read by one Python class reporting each
    batch of the counts it read in order."""
    counter = stream_builder.add(CountsItsOwnCalls, name="counter")
    first_reader = stream_builder.add(
        ReportsEachBatchOfCountsItReadInOrderAsTheFirstReader, name="first_reader"
    )
    stream_builder.connect(counter.output("result"), first_reader.input("count"))


@graph_generator
def python_class_reporting_its_level(stream_builder: StreamBuilder) -> None:
    """A Python class reading its `level` setting at each run."""
    stream_builder.add(ReportsEachBatchOfItsLevel, name="configured")


@graph_generator
def held_level_read_by_two_python_sinks(stream_builder: StreamBuilder) -> None:
    """A stateful function node holding its `level` input, read by two Python classes."""
    held = stream_builder.add(HoldsItsLevelCountingItsCalls, name="held")
    for reader_name in ("first_reader", "second_reader"):
        reader = stream_builder.add(ReportsEachBatchOfFunctionNodeValuesItReadSink, name=reader_name)
        stream_builder.connect(held.output("result"), reader.input("brightness"))


@graph_generator
def two_lfos_each_read_by_its_own_python_sink_on_a_stepped_clock(
    stream_builder: StreamBuilder,
) -> None:
    """The same `Lfo` twice, each in a helper of its own read by its own Python class, on an unpaced stepped clock."""
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
def rust_function_nodes_in_islands_beside_python_ones(stream_builder: StreamBuilder) -> None:
    """`Wave` into a Rust `ScaleAndOffset` into `Clamp`, and a Rust `CountsItsOwnCalls`,
    each read by its own Python class."""
    wave = stream_builder.add(Wave)
    scale_and_offset = stream_builder.add(ScaleAndOffset)
    clamp = stream_builder.add(Clamp)
    values_sink = stream_builder.add(ReportsTheFunctionNodeValuesItReadSink)
    stream_builder.connect(wave.output("result"), scale_and_offset.input("x"))
    stream_builder.connect(scale_and_offset.output("result"), clamp.input("x"))
    stream_builder.connect(clamp.output("result"), values_sink.input("brightness"))

    counter = stream_builder.add(RustCountsItsOwnCalls, name="rust_counter")
    counts_sink = stream_builder.add(ReportsTheCountsItReadInOrderSink)
    stream_builder.connect(counter.output("result"), counts_sink.input("count"))


@graph_generator
def python_source_through_a_gate_into_python_sink(stream_builder: StreamBuilder) -> None:
    """A Python class's counts through `Gate` into another Python class."""
    _python_source_through_this_gate_into_python_sink(stream_builder, Gate)


@graph_generator
def python_source_through_a_rust_gate_into_python_sink(stream_builder: StreamBuilder) -> None:
    """A Python class's counts through the fixture module's Rust `RustGate` into another Python class."""
    _python_source_through_this_gate_into_python_sink(stream_builder, RustGate)


def _python_source_through_this_gate_into_python_sink(stream_builder: StreamBuilder, gate_node: Any) -> None:
    source = stream_builder.add(CountsEachTickRecordsSource, name="source")
    gate = stream_builder.add(gate_node, name="gate")
    sink = stream_builder.add(ReportsEachBatchOfGatedCountsItReadSink, name="sink")
    stream_builder.connect(source.output("counts_to_downstream"), gate.input("value"))
    stream_builder.connect(gate.output("result"), sink.input("gated_counts"))


@graph_generator
def python_source_through_a_gate_that_crashes_on_cue_into_python_sink(
    stream_builder: StreamBuilder,
) -> None:
    """A Python class's counts through a `Gate`-shaped node whose `crash` pin takes its helper down, into another Python class."""
    source = stream_builder.add(CountsEachTickRecordsSource, name="source")
    gate = stream_builder.add(GateThatCrashesItsHelperOnCue, name="gate")
    sink = stream_builder.add(ReportsEachBatchOfGatedCountsItReadSink, name="sink")
    stream_builder.connect(source.output("counts_to_downstream"), gate.input("value"))
    stream_builder.connect(gate.output("result"), sink.input("gated_counts"))


@graph_generator
def python_source_through_a_gate_into_two_python_sinks(stream_builder: StreamBuilder) -> None:
    """A Python class's counts through one `Gate` into two Python classes."""
    source = stream_builder.add(CountsEachTickRecordsSource, name="source")
    gate = stream_builder.add(Gate, name="gate")
    first_sink = stream_builder.add(ReportsEachBatchOfGatedCountsItReadSink, name="first-sink")
    second_sink = stream_builder.add(ReportsEachBatchOfGatedCountsItReadSink, name="second-sink")
    stream_builder.connect(source.output("counts_to_downstream"), gate.input("value"))
    stream_builder.connect(gate.output("result"), first_sink.input("gated_counts"))
    stream_builder.connect(gate.output("result"), second_sink.input("gated_counts"))


@graph_generator
def python_level_source_through_a_gate_into_a_native_converter(stream_builder: StreamBuilder) -> None:
    """A Python class's levels through `Gate` into a native `FloatToInteger`, read by a Python class.

    The converter runs no node code, so the stream's function node pool reads
    the source's channel and evaluates `Gate`, handing each level on unread.
    """
    source = stream_builder.add(CountsEachTickAsALevelSource, name="source")
    gate = stream_builder.add(Gate, name="gate")
    converter = stream_builder.add(FloatToInteger, name="converter")
    sink = stream_builder.add(ReportsTheLevelsItReadSink, name="sink")
    stream_builder.connect(source.output("level_to_downstream"), gate.input("value"))
    stream_builder.connect(gate.output("result"), converter.input("value"))
    stream_builder.connect(converter.output("cast"), sink.input("levels"))


@graph_generator
def typed_level_into_a_value_input_a_computed_value_reads(stream_builder: StreamBuilder) -> None:
    """A typed function node built with `gain` 0.5 into `level`, a value input a computed value reads."""
    scaled = stream_builder.add(
        LevelScaledByItsGain, name="scaled", input_values={"gain": 0.5}
    )
    reader = stream_builder.add(ReportsTheLevelItIsBuiltWith, name="reader")
    stream_builder.connect(scaled.output("result"), reader.input("level"))


@graph_generator
def held_level_into_a_computed_value_and_a_python_sink(stream_builder: StreamBuilder) -> None:
    """A stateful function node holding its `level` input, into a value input a computed value
    reads and into a second Python node; stateful, so it runs in a process of its own."""
    held = stream_builder.add(HoldsItsLevelCountingItsCalls, name="held")
    reader = stream_builder.add(ReportsTheLevelItIsBuiltWith, name="reader")
    sink = stream_builder.add(ReportsEachBatchOfFunctionNodeValuesItReadSink, name="sink")
    stream_builder.connect(held.output("result"), reader.input("level"))
    stream_builder.connect(held.output("result"), sink.input("brightness"))


@graph_generator
def one_width_source_alone(stream_builder: StreamBuilder) -> None:
    """A Python class writing a width, read by nothing until a reader is added while it plays."""
    stream_builder.add(WritesOneWidthSource, name="widths")

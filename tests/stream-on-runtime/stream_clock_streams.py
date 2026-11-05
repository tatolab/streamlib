# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_stream_clock.py` runs from the suite project."""

from tatolab.stream import ScaledStreamClock, SteppedStreamClock, StreamBuilder, stream

from stream_clock_processors import (
    CROSSING_TICK_COUNTER_INTERVAL_MS,
    STEPPED_STREAM_STEP_MS,
    STEPPED_STREAM_STEPS,
    ContinuousTickCounterSource,
    RealTimePacedReporterSink,
    RealTimeCrossingTickCounterSource,
    RealTimePacedWriterSource,
    SlowStepRelay,
    StepTimeReporterSink,
    StepTimeWriterSource,
    StreamTimeStampReporterSink,
    StreamTimeWriterSource,
)

#: The scale the slow-motion stream runs at.
SLOW_MOTION_SCALE = 0.5

#: The scale above real time the fast-forward stream runs at.
FAST_FORWARD_SCALE = 4.0


def _writer_into_reporter(stream_builder: StreamBuilder) -> None:
    writer = stream_builder.add(StreamTimeWriterSource)
    reporter = stream_builder.add(StreamTimeStampReporterSink)
    stream_builder.connect(
        writer.output("stream_times_to_downstream"),
        reporter.input("stream_times_from_upstream"),
    )


@stream
def stream_times_in_slow_motion(stream_builder: StreamBuilder) -> None:
    """A stream-time writer into a stamp reporter, at half real time."""
    stream_builder.run_on_clock(ScaledStreamClock(scale=SLOW_MOTION_SCALE))
    _writer_into_reporter(stream_builder)


@stream
def stream_times_in_real_time(stream_builder: StreamBuilder) -> None:
    """A stream-time writer into a stamp reporter, on the machine clock."""
    _writer_into_reporter(stream_builder)


@stream
def continuous_ticks_in_slow_motion(stream_builder: StreamBuilder) -> None:
    """A continuous tick counter alone, at half real time until the test rescales it."""
    stream_builder.run_on_clock(ScaledStreamClock(scale=SLOW_MOTION_SCALE))
    stream_builder.add(ContinuousTickCounterSource)


@stream
def continuous_ticks_crossing_real_time(stream_builder: StreamBuilder) -> None:
    """A continuous tick counter alone, at half real time in steps of its interval
    until the test rescales it across 1× and back."""
    stream_builder.run_on_clock(
        ScaledStreamClock(
            scale=SLOW_MOTION_SCALE, step_ns=CROSSING_TICK_COUNTER_INTERVAL_MS * 1_000_000
        )
    )
    stream_builder.add(RealTimeCrossingTickCounterSource)


def _step_writer_through_a_slow_relay_into_a_reporter(stream_builder: StreamBuilder) -> None:
    writer = stream_builder.add(StepTimeWriterSource)
    relay = stream_builder.add(SlowStepRelay)
    reporter = stream_builder.add(StepTimeReporterSink)
    stream_builder.connect(
        writer.output("step_times_to_downstream"),
        relay.input("step_times_from_upstream"),
    )
    stream_builder.connect(
        relay.output("step_times_to_downstream"),
        reporter.input("step_times_from_upstream"),
    )


@stream
def step_times_through_a_slow_relay(stream_builder: StreamBuilder) -> None:
    """A step-time writer through a slow relay into a reporter, on an unpaced stepped clock."""
    stream_builder.run_on_clock(
        SteppedStreamClock(
            step_ns=STEPPED_STREAM_STEP_MS * 1_000_000,
            stop_after_steps=STEPPED_STREAM_STEPS,
        )
    )
    _step_writer_through_a_slow_relay_into_a_reporter(stream_builder)


@stream
def step_times_through_a_slow_relay_in_fast_forward(stream_builder: StreamBuilder) -> None:
    """A step-time writer through a slow relay into a reporter, on a clock scaled above 1×."""
    stream_builder.run_on_clock(
        ScaledStreamClock(scale=FAST_FORWARD_SCALE, step_ns=STEPPED_STREAM_STEP_MS * 1_000_000)
    )
    _step_writer_through_a_slow_relay_into_a_reporter(stream_builder)


@stream
def real_time_writes_beside_steps(stream_builder: StreamBuilder) -> None:
    """A node paced by real time into a reporter, beside the step chain, on an unpaced stepped clock."""
    stream_builder.run_on_clock(SteppedStreamClock(step_ns=STEPPED_STREAM_STEP_MS * 1_000_000))
    _step_writer_through_a_slow_relay_into_a_reporter(stream_builder)
    real_time_writer = stream_builder.add(RealTimePacedWriterSource)
    real_time_reporter = stream_builder.add(RealTimePacedReporterSink)
    stream_builder.connect(
        real_time_writer.output("real_time_writes_to_downstream"),
        real_time_reporter.input("real_time_writes_from_upstream"),
    )

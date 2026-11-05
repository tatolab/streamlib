# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_stream_clock.py` runs from the suite project."""

from tatolab.stream import ScaledStreamClock, StreamBuilder, stream

from stream_clock_processors import (
    ContinuousTickCounterSource,
    StreamTimeStampReporterSink,
    StreamTimeWriterSource,
)

#: The scale the slow-motion stream runs at.
SLOW_MOTION_SCALE = 0.5


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

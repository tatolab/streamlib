# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Nodes that report the stream time they read and the stamps they see."""

from typing import Callable
import json
import os
import tempfile

from json_messages import JsonMessagesInOrder, json_bytes, json_read
from tatolab.stream import EncodedVideo, InputPin, OutputPin, RuntimeContextLimitedAccess, log, monotonic_now_ns, node

#: Messages the reporter reads before it reports the first and the last of them.
MESSAGES_BETWEEN_THE_FIRST_AND_THE_LAST_REPORTED = 40


def report(marker_name: str, payload: "dict[str, object]") -> None:
    log.info(f"MARKER:{marker_name} " + json.dumps(payload))


@node(execution="continuous", interval_ms=10)
def StreamTimeWriterSource(
    *,
    stream_times_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Writes, with no stamp, the stream time and the machine time it wrote at."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        stream_times_to_downstream.write(
            json_bytes(
                {
                    "stream_time_ns": ctx.time,
                    "machine_time_ns": monotonic_now_ns(),
                    "mode": ctx.stream_clock.mode,
                    "scale": ctx.stream_clock.scale,
                }
            )
        )

    return each_frame


@node(execution="reactive")
def StreamTimeStampReporterSink(
    *,
    stream_times_from_upstream: InputPin[JsonMessagesInOrder],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports the first and the last of a run of messages: stamps, writer times, its own."""

    first: "dict[str, object] | None" = None
    messages_read = 0
    reported = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal first, messages_read, reported
        message = stream_times_from_upstream.read_message()
        if message is None or reported:
            return
        written = json_read(message.payload)
        seen = {
            "stamp_ns": message.timestamp_ns,
            "written_stream_time_ns": written["stream_time_ns"],
            "written_machine_time_ns": written["machine_time_ns"],
            "writer_mode": written["mode"],
            "writer_scale": written["scale"],
            "reader_stream_time_ns": ctx.time,
            "reader_mode": ctx.stream_clock.mode,
        }
        messages_read += 1
        if first is None:
            first = seen
        elif messages_read > MESSAGES_BETWEEN_THE_FIRST_AND_THE_LAST_REPORTED:
            report("STREAM_TIME_STAMPS", {"first": first, "last": seen})
            reported = True

    return each_frame


#: Ticks a continuous counter times at each scale before it reports them.
TICKS_TIMED_AT_EACH_SCALE = 20

#: The continuous counter's interval, in stream milliseconds.
CONTINUOUS_TICK_COUNTER_INTERVAL_MS = 10


class _TicksTimedOnBothClocksAtEachScale:
    """Times its ticks on both clocks at the scale it starts on, then at each
    next one, up to `SCALES_TIMED` scales."""

    SCALES_TIMED = 2

    def __init__(self) -> None:
        self.scales_reported: "list[float]" = []
        self.ticks_at_this_scale: "list[dict[str, float]]" = []
        self.stream_time_ns_last_read = -1
        self.stream_time_went_backwards = False

    def each_frame(self, ctx) -> None:
        stream_time_ns = ctx.time
        if stream_time_ns < self.stream_time_ns_last_read:
            self.stream_time_went_backwards = True
        self.stream_time_ns_last_read = stream_time_ns
        scale = ctx.stream_clock.scale
        if self.ticks_at_this_scale and self.ticks_at_this_scale[0]["scale"] != scale:
            self.ticks_at_this_scale = []
        if len(self.scales_reported) >= self.SCALES_TIMED or (
            self.scales_reported and self.scales_reported[-1] == scale
        ):
            return
        self.ticks_at_this_scale.append(
            {"scale": scale, "stream_time_ns": stream_time_ns, "machine_time_ns": monotonic_now_ns()}
        )
        if len(self.ticks_at_this_scale) <= TICKS_TIMED_AT_EACH_SCALE:
            return
        first, last = self.ticks_at_this_scale[1], self.ticks_at_this_scale[-1]
        report(
            f"CONTINUOUS_TICKS_AT_SCALE_{len(self.scales_reported)}",
            {
                "scale": scale,
                "ticks": len(self.ticks_at_this_scale) - 2,
                "machine_elapsed_ns": last["machine_time_ns"] - first["machine_time_ns"],
                "stream_elapsed_ns": last["stream_time_ns"] - first["stream_time_ns"],
                "stream_time_went_backwards": self.stream_time_went_backwards,
            },
        )
        self.scales_reported.append(scale)


class _ContinuousTickCounterSource(_TicksTimedOnBothClocksAtEachScale):
    pass


@node(execution="continuous", interval_ms=CONTINUOUS_TICK_COUNTER_INTERVAL_MS)
def ContinuousTickCounterSource() -> Callable[[RuntimeContextLimitedAccess], None]:
    """Times its ticks on both clocks at the scale it starts on, then at the next one."""
    return _ContinuousTickCounterSource().each_frame


#: The interval of the counter whose clock crosses 1×, in stream milliseconds,
#: and its clock's step.
CROSSING_TICK_COUNTER_INTERVAL_MS = 100


class _RealTimeCrossingTickCounterSource(_TicksTimedOnBothClocksAtEachScale):
    SCALES_TIMED = 3


@node(execution="continuous", interval_ms=CROSSING_TICK_COUNTER_INTERVAL_MS)
def RealTimeCrossingTickCounterSource() -> Callable[[RuntimeContextLimitedAccess], None]:
    """Times its ticks on both clocks at three scales in turn: below 1×, above
    it in steps, and below it again."""
    return _RealTimeCrossingTickCounterSource().each_frame


#: Stream milliseconds per step of the stepped stream, and the source's interval.
STEPPED_STREAM_STEP_MS = 1_000

#: Steps the stepped stream runs before it stops itself.
STEPPED_STREAM_STEPS = 12

#: Machine milliseconds the slow relay holds each message before it writes it on.
SLOW_RELAY_HOLD_MS = 20


@node(execution="continuous", interval_ms=STEPPED_STREAM_STEP_MS)
def StepTimeWriterSource(
    *,
    step_times_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Writes, with no stamp, the step open and the stream time it wrote in."""

    machine_time_ns_of_the_first_write: "int | None" = None

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal machine_time_ns_of_the_first_write
        machine_now_ns = monotonic_now_ns()
        if machine_time_ns_of_the_first_write is None:
            machine_time_ns_of_the_first_write = machine_now_ns
        step_times_to_downstream.write(
            json_bytes(
                {
                    "step_index": ctx.stream_clock.step_index,
                    "stream_time_ns": ctx.time,
                    "first_write_machine_time_ns": machine_time_ns_of_the_first_write,
                }
            )
        )

    return each_frame


@node(execution="reactive")
def SlowStepRelay(
    *,
    step_times_from_upstream: InputPin[JsonMessagesInOrder],
    step_times_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Holds each message for a while before writing it on, with no stamp."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        message = step_times_from_upstream.read_message()
        if message is None:
            return
        held_until_ns = monotonic_now_ns() + SLOW_RELAY_HOLD_MS * 1_000_000
        while monotonic_now_ns() < held_until_ns:
            pass
        relayed = json_read(message.payload)
        relayed["stamp_the_relay_read_ns"] = message.timestamp_ns
        relayed["relay_stream_time_ns"] = ctx.time
        step_times_to_downstream.write(json_bytes(relayed))

    return each_frame


@node(execution="reactive")
def StepTimeReporterSink(
    *,
    step_times_from_upstream: InputPin[JsonMessagesInOrder],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports every step's message once it has read one per step of the run."""

    seen: "list[dict[str, object]]" = []

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        message = step_times_from_upstream.read_message()
        if message is None:
            return
        relayed = json_read(message.payload)
        seen.append(
            {
                "step_index": relayed["step_index"],
                "written_stream_time_ns": relayed["stream_time_ns"],
                "stamp_the_relay_read_ns": relayed["stamp_the_relay_read_ns"],
                "relay_stream_time_ns": relayed["relay_stream_time_ns"],
                "stamp_ns": message.timestamp_ns,
                "lost_since_last_read": message.lost_since_last_read,
                "reader_step_index": ctx.stream_clock.step_index,
            }
        )
        if len(seen) == STEPPED_STREAM_STEPS:
            report(
                "STEPPED_STREAM_RUN",
                {
                    "seen": seen,
                    "machine_elapsed_ns": monotonic_now_ns()
                    - relayed["first_write_machine_time_ns"],
                },
            )

    return each_frame


#: The machine milliseconds between the writes of the node paced by real time.
REAL_TIME_PACED_WRITER_INTERVAL_MS = 50

#: Messages the real-time reporter reads before it reports them.
REAL_TIME_PACED_MESSAGES_REPORTED = 10


@node(execution="continuous", interval_ms=REAL_TIME_PACED_WRITER_INTERVAL_MS, paced_by_real_time=True)
def RealTimePacedWriterSource(
    *,
    real_time_writes_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Writes, with no stamp, the stream time and the machine time it wrote at, paced on the machine."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        real_time_writes_to_downstream.write(
            json_bytes({"stream_time_ns": ctx.time, "machine_time_ns": monotonic_now_ns()})
        )

    return each_frame


@node(execution="reactive")
def RealTimePacedReporterSink(
    *,
    real_time_writes_from_upstream: InputPin[JsonMessagesInOrder],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports a run of the real-time writer's messages: stamps, writer stream and machine times."""

    seen: "list[dict[str, object]]" = []

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        message = real_time_writes_from_upstream.read_message()
        if message is None or len(seen) >= REAL_TIME_PACED_MESSAGES_REPORTED:
            return
        written = json_read(message.payload)
        seen.append(
            {
                "stamp_ns": message.timestamp_ns,
                "written_stream_time_ns": written["stream_time_ns"],
                "written_machine_time_ns": written["machine_time_ns"],
            }
        )
        if len(seen) == REAL_TIME_PACED_MESSAGES_REPORTED:
            report("REAL_TIME_PACED_WRITES", {"seen": seen})

    return each_frame


#: The step in which the relay's first helper ends itself, after taking the
#: first of that step's two messages and before the second.
STEP_THE_RELAY_HELPER_DIES_IN = 3


@node(execution="continuous", interval_ms=STEPPED_STREAM_STEP_MS)
def TwoWritesPerStepSource(
    *,
    step_indices_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Writes two messages in every step, each naming the step."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        for message_in_the_step in range(2):
            step_indices_to_downstream.write(
                json_bytes(
                    {
                        "step_index": ctx.stream_clock.step_index,
                        "message_in_the_step": message_in_the_step,
                    }
                )
            )

    return each_frame


def _flag_marking_that_a_helper_of_this_stream_died() -> str:
    return os.path.join(
        tempfile.gettempdir(), f"tatolab-relay-helper-died-under-{os.getppid()}"
    )


@node(execution="reactive")
def RelayWhoseFirstHelperDiesMidStep(
    *,
    step_indices_from_upstream: InputPin[JsonMessagesInOrder],
    step_indices_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Relays each message on; its first helper ends itself mid-step, leaving one unread."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        message = step_indices_from_upstream.read_message()
        if message is None:
            return
        relayed = json_read(message.payload)
        flag = _flag_marking_that_a_helper_of_this_stream_died()
        if relayed["step_index"] == STEP_THE_RELAY_HELPER_DIES_IN and not os.path.exists(flag):
            with open(flag, "w"):
                pass
            os._exit(17)
        step_indices_to_downstream.write(json_bytes(relayed))

    return each_frame


@node(execution="reactive")
def LastStepReporterSink(
    *,
    step_indices_from_upstream: InputPin[JsonMessagesInOrder],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports the step indices it read once it reads both of the last step's."""

    step_indices_seen: "list[int]" = []

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        message = step_indices_from_upstream.read_message()
        if message is None:
            return
        step_index = json_read(message.payload)["step_index"]
        step_indices_seen.append(step_index)
        if step_indices_seen.count(STEPPED_STREAM_STEPS - 1) == 2:
            if os.path.exists(_flag_marking_that_a_helper_of_this_stream_died()):
                os.remove(_flag_marking_that_a_helper_of_this_stream_died())
            report("STEPS_PAST_A_HELPER_RESTART", {"step_indices_seen": step_indices_seen})

    return each_frame

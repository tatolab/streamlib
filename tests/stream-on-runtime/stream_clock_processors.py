# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Processors that report the stream time they read and the stamps they see."""

import json
import os
import tempfile

from tatolab.stream import Input, Output, Records, log, monotonic_now_ns, node

#: Messages the reporter reads before it reports the first and the last of them.
MESSAGES_BETWEEN_THE_FIRST_AND_THE_LAST_REPORTED = 40


def report(marker_name: str, payload: "dict[str, object]") -> None:
    log.info(f"MARKER:{marker_name} " + json.dumps(payload))


@node.process(execution="continuous", interval_ms=10)
class StreamTimeWriterSource:
    """Writes, with no stamp, the stream time and the machine time it wrote at."""

    stream_times_to_downstream = Output(Records)

    def process(self, ctx) -> None:
        ctx.outputs.write(
            self.stream_times_to_downstream,
            {
                "stream_time_ns": ctx.time,
                "machine_time_ns": monotonic_now_ns(),
                "mode": ctx.stream_clock.mode,
                "scale": ctx.stream_clock.scale,
            },
        )


@node.process
class StreamTimeStampReporterSink:
    """Reports the first and the last of a run of messages: stamps, writer times, its own."""

    def __init__(self) -> None:
        self.first: "dict[str, object] | None" = None
        self.messages_read = 0
        self.reported = False

    stream_times_from_upstream = Input(Records, delivery="ordered")

    def process(self, ctx) -> None:
        message = ctx.inputs.read_message(self.stream_times_from_upstream)
        if message is None or self.reported:
            return
        written = message.payload
        seen = {
            "stamp_ns": message.timestamp_ns,
            "written_stream_time_ns": written["stream_time_ns"],
            "written_machine_time_ns": written["machine_time_ns"],
            "writer_mode": written["mode"],
            "writer_scale": written["scale"],
            "reader_stream_time_ns": ctx.time,
            "reader_mode": ctx.stream_clock.mode,
        }
        self.messages_read += 1
        if self.first is None:
            self.first = seen
        elif self.messages_read > MESSAGES_BETWEEN_THE_FIRST_AND_THE_LAST_REPORTED:
            report("STREAM_TIME_STAMPS", {"first": self.first, "last": seen})
            self.reported = True


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

    def process(self, ctx) -> None:
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


@node.process(execution="continuous", interval_ms=CONTINUOUS_TICK_COUNTER_INTERVAL_MS)
class ContinuousTickCounterSource(_TicksTimedOnBothClocksAtEachScale):
    """Times its ticks on both clocks at the scale it starts on, then at the next one."""


#: The interval of the counter whose clock crosses 1×, in stream milliseconds,
#: and its clock's step.
CROSSING_TICK_COUNTER_INTERVAL_MS = 100


@node.process(execution="continuous", interval_ms=CROSSING_TICK_COUNTER_INTERVAL_MS)
class RealTimeCrossingTickCounterSource(_TicksTimedOnBothClocksAtEachScale):
    """Times its ticks on both clocks at three scales in turn: below 1×, above
    it in steps, and below it again."""

    SCALES_TIMED = 3


#: Stream milliseconds per step of the stepped stream, and the source's interval.
STEPPED_STREAM_STEP_MS = 1_000

#: Steps the stepped stream runs before it stops itself.
STEPPED_STREAM_STEPS = 12

#: Machine milliseconds the slow relay holds each message before it writes it on.
SLOW_RELAY_HOLD_MS = 20


@node.process(execution="continuous", interval_ms=STEPPED_STREAM_STEP_MS)
class StepTimeWriterSource:
    """Writes, with no stamp, the step open and the stream time it wrote in."""

    def __init__(self) -> None:
        self.machine_time_ns_of_the_first_write: "int | None" = None

    step_times_to_downstream = Output(Records)

    def process(self, ctx) -> None:
        machine_now_ns = monotonic_now_ns()
        if self.machine_time_ns_of_the_first_write is None:
            self.machine_time_ns_of_the_first_write = machine_now_ns
        ctx.outputs.write(
            self.step_times_to_downstream,
            {
                "step_index": ctx.stream_clock.step_index,
                "stream_time_ns": ctx.time,
                "first_write_machine_time_ns": self.machine_time_ns_of_the_first_write,
            },
        )


@node.process
class SlowStepRelay:
    """Holds each message for a while before writing it on, with no stamp."""

    step_times_from_upstream = Input(Records, delivery="ordered")

    step_times_to_downstream = Output(Records)

    def process(self, ctx) -> None:
        message = ctx.inputs.read_message(self.step_times_from_upstream)
        if message is None:
            return
        held_until_ns = monotonic_now_ns() + SLOW_RELAY_HOLD_MS * 1_000_000
        while monotonic_now_ns() < held_until_ns:
            pass
        relayed = message.payload
        relayed["stamp_the_relay_read_ns"] = message.timestamp_ns
        relayed["relay_stream_time_ns"] = ctx.time
        ctx.outputs.write(self.step_times_to_downstream, relayed)


@node.process
class StepTimeReporterSink:
    """Reports every step's message once it has read one per step of the run."""

    def __init__(self) -> None:
        self.seen: "list[dict[str, object]]" = []

    step_times_from_upstream = Input(Records, delivery="ordered")

    def process(self, ctx) -> None:
        message = ctx.inputs.read_message(self.step_times_from_upstream)
        if message is None:
            return
        relayed = message.payload
        self.seen.append(
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
        if len(self.seen) == STEPPED_STREAM_STEPS:
            report(
                "STEPPED_STREAM_RUN",
                {
                    "seen": self.seen,
                    "machine_elapsed_ns": monotonic_now_ns()
                    - relayed["first_write_machine_time_ns"],
                },
            )


#: The machine milliseconds between the writes of the node paced by real time.
REAL_TIME_PACED_WRITER_INTERVAL_MS = 50

#: Messages the real-time reporter reads before it reports them.
REAL_TIME_PACED_MESSAGES_REPORTED = 10


@node.process(execution="continuous", interval_ms=REAL_TIME_PACED_WRITER_INTERVAL_MS, paced_by_real_time=True)
class RealTimePacedWriterSource:
    """Writes, with no stamp, the stream time and the machine time it wrote at, paced on the machine."""

    real_time_writes_to_downstream = Output(Records)

    def process(self, ctx) -> None:
        ctx.outputs.write(
            self.real_time_writes_to_downstream,
            {"stream_time_ns": ctx.time, "machine_time_ns": monotonic_now_ns()},
        )


@node.process
class RealTimePacedReporterSink:
    """Reports a run of the real-time writer's messages: stamps, writer stream and machine times."""

    def __init__(self) -> None:
        self.seen: "list[dict[str, object]]" = []

    real_time_writes_from_upstream = Input(Records, delivery="ordered")

    def process(self, ctx) -> None:
        message = ctx.inputs.read_message(self.real_time_writes_from_upstream)
        if message is None or len(self.seen) >= REAL_TIME_PACED_MESSAGES_REPORTED:
            return
        written = message.payload
        self.seen.append(
            {
                "stamp_ns": message.timestamp_ns,
                "written_stream_time_ns": written["stream_time_ns"],
                "written_machine_time_ns": written["machine_time_ns"],
            }
        )
        if len(self.seen) == REAL_TIME_PACED_MESSAGES_REPORTED:
            report("REAL_TIME_PACED_WRITES", {"seen": self.seen})


#: The step in which the relay's first helper ends itself, after taking the
#: first of that step's two messages and before the second.
STEP_THE_RELAY_HELPER_DIES_IN = 3


@node.process(execution="continuous", interval_ms=STEPPED_STREAM_STEP_MS)
class TwoWritesPerStepSource:
    """Writes two messages in every step, each naming the step."""

    step_indices_to_downstream = Output(Records)

    def process(self, ctx) -> None:
        for message_in_the_step in range(2):
            ctx.outputs.write(
                self.step_indices_to_downstream,
                {
                    "step_index": ctx.stream_clock.step_index,
                    "message_in_the_step": message_in_the_step,
                },
            )


def _flag_marking_that_a_helper_of_this_stream_died() -> str:
    return os.path.join(
        tempfile.gettempdir(), f"tatolab-relay-helper-died-under-{os.getppid()}"
    )


@node.process
class RelayWhoseFirstHelperDiesMidStep:
    """Relays each message on; its first helper ends itself mid-step, leaving one unread."""

    step_indices_from_upstream = Input(Records, delivery="ordered")

    step_indices_to_downstream = Output(Records)

    def process(self, ctx) -> None:
        message = ctx.inputs.read_message(self.step_indices_from_upstream)
        if message is None:
            return
        relayed = message.payload
        flag = _flag_marking_that_a_helper_of_this_stream_died()
        if relayed["step_index"] == STEP_THE_RELAY_HELPER_DIES_IN and not os.path.exists(flag):
            with open(flag, "w"):
                pass
            os._exit(17)
        ctx.outputs.write(self.step_indices_to_downstream, relayed)


@node.process
class LastStepReporterSink:
    """Reports the step indices it read once it reads both of the last step's."""

    def __init__(self) -> None:
        self.step_indices_seen: "list[int]" = []

    step_indices_from_upstream = Input(Records, delivery="ordered")

    def process(self, ctx) -> None:
        message = ctx.inputs.read_message(self.step_indices_from_upstream)
        if message is None:
            return
        step_index = message.payload["step_index"]
        self.step_indices_seen.append(step_index)
        if self.step_indices_seen.count(STEPPED_STREAM_STEPS - 1) == 2:
            if os.path.exists(_flag_marking_that_a_helper_of_this_stream_died()):
                os.remove(_flag_marking_that_a_helper_of_this_stream_died())
            report("STEPS_PAST_A_HELPER_RESTART", {"step_indices_seen": self.step_indices_seen})

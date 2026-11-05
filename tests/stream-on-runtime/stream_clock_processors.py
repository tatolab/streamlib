# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Processors that report the stream time they read and the stamps they see."""

import json

from tatolab.stream import log, monotonic_now_ns, node
from tatolab.stream.msgpack import pack, unpack

#: Messages the reporter reads before it reports the first and the last of them.
MESSAGES_BETWEEN_THE_FIRST_AND_THE_LAST_REPORTED = 40


def report(marker_name: str, payload: "dict[str, object]") -> None:
    log.info(f"MARKER:{marker_name} " + json.dumps(payload))


@node(execution="continuous", interval_ms=10)
class StreamTimeWriterSource:
    """Writes, with no stamp, the stream time and the machine time it wrote at."""

    @node.output()
    def stream_times_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        ctx.outputs.write(
            "stream_times_to_downstream",
            pack(
                {
                    "stream_time_ns": ctx.time,
                    "machine_time_ns": monotonic_now_ns(),
                    "mode": ctx.stream_clock.mode,
                    "scale": ctx.stream_clock.scale,
                }
            ),
        )


@node
class StreamTimeStampReporterSink:
    """Reports the first and the last of a run of messages: stamps, writer times, its own."""

    def __init__(self) -> None:
        self.first: "dict[str, object] | None" = None
        self.messages_read = 0
        self.reported = False

    @node.input(delivery_profile="ordered")
    def stream_times_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        message = ctx.inputs.read_message("stream_times_from_upstream")
        if message is None or self.reported:
            return
        written = unpack(message.payload)
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


@node(execution="continuous", interval_ms=CONTINUOUS_TICK_COUNTER_INTERVAL_MS)
class ContinuousTickCounterSource:
    """Times its ticks on both clocks at the scale it starts on, then at the next one."""

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
        if len(self.scales_reported) >= 2 or scale in self.scales_reported:
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

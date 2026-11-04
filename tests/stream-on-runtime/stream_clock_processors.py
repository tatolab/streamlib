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

    @node.input(delivery_profile="fifo")
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

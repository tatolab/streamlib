# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`BrowserSourceOutputProbe`: what a `BrowserSource` publishes, measured downstream.

It reads the page's frames, audio and events, and sends the page a `data`
message every `frames_between_data_messages` frames. A page that answers each
one by publishing `{"data_painted": <the message's sent_ns>}` from the first
animation frame after it handled the event lets the probe report how long a
`data` message takes to reach the page's next animation frame callback and come
back; it does not wait for that frame's paint to reach `video`. A page that
publishes `{"page_input_port_name": <port>, "page_input_timestamp_ns": <the
chunk's timestamp>}` as it draws or plays an input's chunk lets the probe report
each input's latency: from the chunk's own timestamp, through the page, to the
probe reading the event.
"""

from __future__ import annotations

from typing import Annotated, Callable

import numpy
from tatolab.stream import (
    Audio,
    Description,
    Texture,
    Input,
    InputPin,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    log,
    node,
)
from tatolab.stream.clock import monotonic_now_ns

from ._duration_samples import DurationSamplesInOneReportingWindow
from .page_message import PageMessage

# A block whose first sample lands further than this from where the previous
# block's samples end breaks the timeline.
AUDIO_TIMELINE_BREAK_TOLERANCE_NS = 1_000_000


@node.meta(description="Logs a BrowserSource's frame rate, audio continuity and data round trip")
@node(execution="reactive")
def BrowserSourceOutputProbe(
    *,
    frames_between_reports: Input[
        Annotated[
            int,
            NumberRange(minimum=2),
            Description("How many frames one report summarises; a change starts the count again."),
        ]
    ] = Input(600),
    frames_between_data_messages: Input[
        Annotated[
            int,
            NumberRange(minimum=1),
            Description("How many frames pass between two `data` messages sent to the page."),
        ]
    ] = Input(30),
    video_from_browser: InputPin[
        Annotated[Texture, StreamDelivery("ordered"), Description("Frames from the browser under test")]
    ],
    audio_from_browser: InputPin[
        Annotated[Audio, StreamDelivery("ordered"), Description("Audio from the browser under test")]
    ],
    events_from_browser: InputPin[
        Annotated[PageMessage, StreamDelivery("ordered"), Description("Messages the page published")]
    ],
    data_to_browser: OutputPin[
        Annotated[
            PageMessage,
            Description(
                '`{"sent_ns": <monotonic now>}`, every `frames_between_data_messages` frames'
            ),
        ]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """A `BrowserSource`'s outputs in, `data` messages to it out, a report every window."""
    measurement = BrowserSourceOutputMeasurement()

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        del ctx
        while (block := audio_from_browser.read()) is not None:
            measurement.measure_audio_block(block)
        while (event := events_from_browser.read()) is not None:
            measurement.measure_published_event(event)
        while (frame := video_from_browser.read()) is not None:
            if measurement.measure_frame(
                frame, frames_between_reports.get(), frames_between_data_messages.get()
            ):
                data_to_browser.write(PageMessage({"sent_ns": monotonic_now_ns()}))

    return each_frame


class BrowserSourceOutputMeasurement:
    """What a `BrowserSourceOutputProbe` has measured in its reporting window."""

    def __init__(self) -> None:
        self._frame_interval = DurationSamplesInOneReportingWindow()
        self._data_round_trip = DurationSamplesInOneReportingWindow()
        self._frames_read = 0
        self._frames_between_reports: "int | None" = None
        self._window_first_frame_read_ns: "int | None" = None
        self._previous_frame_timestamp_ns: "int | None" = None
        self._audio_blocks_read = 0
        self._silent_audio_blocks = 0
        self._audio_timeline_breaks = 0
        self._next_audio_sample_timestamp_ns: "int | None" = None
        self._events_read = 0
        self._page_input_latency_by_input_port_name: dict[
            str, DurationSamplesInOneReportingWindow
        ] = {}

    def measure_audio_block(self, block: Audio) -> None:
        self._audio_blocks_read += 1
        if not numpy.from_dlpack(block).any():
            self._silent_audio_blocks += 1
        expected_ns = self._next_audio_sample_timestamp_ns
        if (
            expected_ns is not None
            and abs(block.timestamp_ns - expected_ns) > AUDIO_TIMELINE_BREAK_TOLERANCE_NS
        ):
            self._audio_timeline_breaks += 1
        self._next_audio_sample_timestamp_ns = (
            block.timestamp_ns + block.samples_per_channel * 1_000_000_000 // block.sample_rate
        )

    def measure_published_event(self, event: PageMessage) -> None:
        self._events_read += 1
        published = event.members
        data_painted_ns = published.get("data_painted")
        if isinstance(data_painted_ns, int):
            self._data_round_trip.add(monotonic_now_ns() - data_painted_ns)
        input_port_name = published.get("page_input_port_name")
        input_timestamp_ns = published.get("page_input_timestamp_ns")
        if isinstance(input_port_name, str) and isinstance(input_timestamp_ns, int):
            self._page_input_latency_by_input_port_name.setdefault(
                input_port_name, DurationSamplesInOneReportingWindow()
            ).add(monotonic_now_ns() - input_timestamp_ns)

    def measure_frame(
        self, frame: Texture, frames_between_reports: int, frames_between_data_messages: int
    ) -> bool:
        """Measure one frame, reporting at the end of a window; whether a `data` message is due."""
        if frames_between_reports != self._frames_between_reports:
            self._frames_between_reports = frames_between_reports
            self._frames_read = 0
            self._window_first_frame_read_ns = None
        read_ns = monotonic_now_ns()
        if self._previous_frame_timestamp_ns is not None:
            self._frame_interval.add(frame.timestamp_ns - self._previous_frame_timestamp_ns)
        self._previous_frame_timestamp_ns = frame.timestamp_ns
        if self._window_first_frame_read_ns is None:
            self._window_first_frame_read_ns = read_ns
        self._frames_read += 1
        if self._frames_read % frames_between_reports == 0:
            self._report(frame, read_ns, frames_between_reports)
        return self._frames_read % frames_between_data_messages == 0

    def _report(self, frame: Texture, read_ns: int, frames_between_reports: int) -> None:
        window_first_read_ns = self._window_first_frame_read_ns or read_ns
        window_seconds = max(read_ns - window_first_read_ns, 1) / 1_000_000_000
        frames_per_second = (frames_between_reports - 1) / window_seconds
        log.info(
            f"BrowserSourceOutputProbe: {frame.width}x{frame.height} "
            f"frames_read={self._frames_read} fps={frames_per_second:.2f} "
            f"frame_interval_ms {self._frame_interval.summary_in_milliseconds()} | "
            f"data_round_trip_ms {self._data_round_trip.summary_in_milliseconds()} | "
            f"audio_blocks_read={self._audio_blocks_read} "
            f"silent_audio_blocks={self._silent_audio_blocks} "
            f"audio_timeline_breaks={self._audio_timeline_breaks} "
            f"events_read={self._events_read}"
            + "".join(
                f" | {input_port_name}_latency_ms {latency.summary_in_milliseconds()}"
                for input_port_name, latency in sorted(
                    self._page_input_latency_by_input_port_name.items()
                )
            )
        )
        self._window_first_frame_read_ns = None

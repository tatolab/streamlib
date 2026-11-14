# Copyright (c) 2025 Jonathan Fontanez
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

import dataclasses
from typing import Annotated

import numpy
from tatolab.stream import (
    Channels,
    ChannelsBlock,
    Image,
    ImageFrame,
    Input,
    Output,
    Records,
    RuntimeContextLimitedAccess,
    log,
    node,
)
from tatolab.stream.clock import monotonic_now_ns

from ._duration_samples import DurationSamplesInOneReportingWindow

# A block whose first sample lands further than this from where the previous
# block's samples end breaks the timeline.
AUDIO_TIMELINE_BREAK_TOLERANCE_NS = 1_000_000


@dataclasses.dataclass
class BrowserSourceOutputProbeConfig:
    """What a `BrowserSourceOutputProbe` is configured with."""

    frames_between_reports: Annotated[int, "How many frames one report summarises."] = 600
    frames_between_data_messages: Annotated[
        int, "How many frames pass between two `data` messages sent to the page."
    ] = 30

    def __post_init__(self) -> None:
        fewest_frames_by_field_name = {
            "frames_between_reports": 2,
            "frames_between_data_messages": 1,
        }
        for field_name, fewest_frames in fewest_frames_by_field_name.items():
            frames = getattr(self, field_name)
            if not isinstance(frames, int) or isinstance(frames, bool) or frames < fewest_frames:
                raise ValueError(
                    f"BrowserSourceOutputProbeConfig: {field_name} is a whole number of "
                    f"frames, at least {fewest_frames}, got {frames!r}"
                )


@node.meta(description="Logs a BrowserSource's frame rate, audio continuity and data round trip")
@node.process
class BrowserSourceOutputProbe:
    """A `BrowserSource`'s outputs in, `data` messages to it out, a report every window."""

    def __init__(self, config: BrowserSourceOutputProbeConfig) -> None:
        self._frames_between_reports = config.frames_between_reports
        self._frames_between_data_messages = config.frames_between_data_messages
        self._frame_interval = DurationSamplesInOneReportingWindow()
        self._data_round_trip = DurationSamplesInOneReportingWindow()
        self._frames_read = 0
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

    video_from_browser = Input(
        Image, delivery="ordered", description="Frames from the browser under test"
    )
    audio_from_browser = Input(
        Channels, delivery="ordered", description="Audio from the browser under test"
    )
    events_from_browser = Input(Records, delivery="ordered", description="Maps the page published")
    data_to_browser = Output(
        Records,
        description='`{"sent_ns": <monotonic now>}`, every `frames_between_data_messages` frames',
    )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        while (block := ctx.inputs.read(self.audio_from_browser)) is not None:
            self._measure_audio_block(block)
        while (event := ctx.inputs.read(self.events_from_browser)) is not None:
            self._measure_published_event(event)
        while (frame := ctx.inputs.read(self.video_from_browser)) is not None:
            self._measure_frame(ctx, frame)

    def _measure_audio_block(self, block: ChannelsBlock) -> None:
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

    def _measure_published_event(self, event: "dict[str, object]") -> None:
        self._events_read += 1
        data_painted_ns = event.get("data_painted")
        if isinstance(data_painted_ns, int):
            self._data_round_trip.add(monotonic_now_ns() - data_painted_ns)
        input_port_name = event.get("page_input_port_name")
        input_timestamp_ns = event.get("page_input_timestamp_ns")
        if isinstance(input_port_name, str) and isinstance(input_timestamp_ns, int):
            self._page_input_latency_by_input_port_name.setdefault(
                input_port_name, DurationSamplesInOneReportingWindow()
            ).add(monotonic_now_ns() - input_timestamp_ns)

    def _measure_frame(self, ctx: RuntimeContextLimitedAccess, frame: ImageFrame) -> None:
        read_ns = monotonic_now_ns()
        if self._previous_frame_timestamp_ns is not None:
            self._frame_interval.add(frame.timestamp_ns - self._previous_frame_timestamp_ns)
        self._previous_frame_timestamp_ns = frame.timestamp_ns
        if self._window_first_frame_read_ns is None:
            self._window_first_frame_read_ns = read_ns
        self._frames_read += 1
        if self._frames_read % self._frames_between_data_messages == 0:
            ctx.outputs.write(self.data_to_browser, {"sent_ns": monotonic_now_ns()})
        if self._frames_read % self._frames_between_reports == 0:
            self._report(frame, read_ns)

    def _report(self, frame: ImageFrame, read_ns: int) -> None:
        window_first_read_ns = self._window_first_frame_read_ns or read_ns
        window_seconds = max(read_ns - window_first_read_ns, 1) / 1_000_000_000
        frames_per_second = (self._frames_between_reports - 1) / window_seconds
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

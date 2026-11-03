# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`BrowserSourceOutputProbe`: what a `BrowserSource` publishes, measured downstream.

It reads the page's frames, audio and events, and sends the page a `data`
message every `frames_between_data_messages` frames. A page that answers each
one by publishing `{"data_painted": <the message's sent_ns>}` from the first
animation frame after it handled the event lets the probe report how long a
`data` message takes to reach the page's paint and come back.
"""

from __future__ import annotations

import dataclasses
from typing import Annotated, Any

from tatolab.stream import AudioBlock, RuntimeContextLimitedAccess, VideoFrame, log, msgpack, node
from tatolab.stream.catalog import BinaryEntry
from tatolab.stream.clock import monotonic_now_ns

from ._duration_samples import DurationSamplesInOneReportingWindow

VIDEO_INPUT_PORT = "video_from_browser"
AUDIO_INPUT_PORT = "audio_from_browser"
EVENTS_INPUT_PORT = "events_from_browser"
DATA_OUTPUT_PORT = "data_to_browser"

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
        for field_name in ("frames_between_reports", "frames_between_data_messages"):
            frames = getattr(self, field_name)
            if not isinstance(frames, int) or isinstance(frames, bool) or frames < 1:
                raise ValueError(
                    f"BrowserSourceOutputProbeConfig: {field_name} is a whole number of "
                    f"frames, at least 1, got {frames!r}"
                )


@node(description="Logs a BrowserSource's frame rate, audio continuity and data round trip")
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

    @node.input(delivery_profile="ordered", description="Frames from the browser under test")
    def video_from_browser(self) -> VideoFrame: ...

    @node.input(delivery_profile="ordered")
    def audio_from_browser(self) -> None:
        """Audio blocks from the browser under test."""

    @node.input(delivery_profile="ordered")
    def events_from_browser(self) -> None:
        """Maps the page published."""

    @node.output(catalog=BinaryEntry(mode="snapshot", mime="application/msgpack"))
    def data_to_browser(self) -> None:
        """`{"sent_ns": <monotonic now>}`, every `frames_between_data_messages` frames."""

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        while (payload := ctx.inputs.read(AUDIO_INPUT_PORT, into=AudioBlock)) is not None:
            self._measure_audio_block(payload)
        while (payload := ctx.inputs.read(EVENTS_INPUT_PORT)) is not None:
            self._measure_published_event(msgpack.unpack(payload))
        while (frame := ctx.inputs.read(VIDEO_INPUT_PORT, into=VideoFrame)) is not None:
            self._measure_frame(ctx, frame)

    def _measure_audio_block(self, block: AudioBlock) -> None:
        self._audio_blocks_read += 1
        if block.interleaved_sample_bytes.count(0) == len(block.interleaved_sample_bytes):
            self._silent_audio_blocks += 1
        expected_ns = self._next_audio_sample_timestamp_ns
        if (
            expected_ns is not None
            and abs(block.first_sample_timestamp_ns - expected_ns)
            > AUDIO_TIMELINE_BREAK_TOLERANCE_NS
        ):
            self._audio_timeline_breaks += 1
        self._next_audio_sample_timestamp_ns = (
            block.first_sample_timestamp_ns
            + block.sample_count * 1_000_000_000 // block.sample_rate
        )

    def _measure_published_event(self, event: Any) -> None:
        self._events_read += 1
        if isinstance(event, dict) and isinstance(event.get("data_painted"), int):
            self._data_round_trip.add(monotonic_now_ns() - event["data_painted"])

    def _measure_frame(self, ctx: RuntimeContextLimitedAccess, frame: VideoFrame) -> None:
        read_ns = monotonic_now_ns()
        if self._previous_frame_timestamp_ns is not None:
            self._frame_interval.add(frame.timestamp_ns - self._previous_frame_timestamp_ns)
        self._previous_frame_timestamp_ns = frame.timestamp_ns
        if self._window_first_frame_read_ns is None:
            self._window_first_frame_read_ns = read_ns
        self._frames_read += 1
        if self._frames_read % self._frames_between_data_messages == 0:
            ctx.outputs.write(DATA_OUTPUT_PORT, msgpack.pack({"sent_ns": monotonic_now_ns()}))
        if self._frames_read % self._frames_between_reports == 0:
            self._report(frame, read_ns)

    def _report(self, frame: VideoFrame, read_ns: int) -> None:
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
        )
        self._window_first_frame_read_ns = None

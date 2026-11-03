# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`CaptureToDownstreamLatencyProbe`: how late and how often camera frames reach a downstream node.

A frame's `timestamp_ns` is its capture instant on the machine's monotonic
clock, so the probe measures the clock when the frame is read, less the clock
when it was captured.
"""

from __future__ import annotations

import dataclasses
from typing import Annotated

from tatolab.stream import RuntimeContextLimitedAccess, VideoFrame, log, node
from tatolab.stream.clock import monotonic_now_ns

from ._duration_samples import DurationSamplesInOneReportingWindow

VIDEO_INPUT_PORT = "video_from_upstream"


@dataclasses.dataclass
class CaptureToDownstreamLatencyProbeConfig:
    """What a `CaptureToDownstreamLatencyProbe` is configured with."""

    camera_label: Annotated[str, "Names the camera under test in every report."] = "camera"
    frames_between_reports: Annotated[int, "How many frames one report summarises."] = 600


@node(description="Logs the frame rate and capture-to-read latency of the frames it receives")
class CaptureToDownstreamLatencyProbe:
    """Video frames in; a report every `frames_between_reports` frames."""

    def __init__(self, config: CaptureToDownstreamLatencyProbeConfig) -> None:
        self._camera_label = config.camera_label
        self._frames_between_reports = config.frames_between_reports
        self._capture_to_read = DurationSamplesInOneReportingWindow()
        self._capture_interval = DurationSamplesInOneReportingWindow()
        self._frames_read = 0
        self._window_first_read_ns: "int | None" = None
        self._previous_capture_ns: "int | None" = None

    @node.input(delivery_profile="ordered", description="Frames from the camera under test")
    def video_from_upstream(self) -> VideoFrame: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        while (payload := ctx.inputs.read(VIDEO_INPUT_PORT)) is not None:
            read_ns = monotonic_now_ns()
            frame = VideoFrame.from_bytes(payload)
            self._capture_to_read.add(read_ns - frame.timestamp_ns)
            if self._previous_capture_ns is not None:
                self._capture_interval.add(frame.timestamp_ns - self._previous_capture_ns)
            self._previous_capture_ns = frame.timestamp_ns
            if self._window_first_read_ns is None:
                self._window_first_read_ns = read_ns
            self._frames_read += 1
            if self._frames_read % self._frames_between_reports == 0:
                self._report(frame, read_ns)

    def _report(self, frame: VideoFrame, read_ns: int) -> None:
        window_first_read_ns = self._window_first_read_ns or read_ns
        window_seconds = max(read_ns - window_first_read_ns, 1) / 1_000_000_000
        frames_per_second = (self._frames_between_reports - 1) / window_seconds
        log.info(
            f"CaptureToDownstreamLatencyProbe[{self._camera_label}]: "
            f"{frame.width}x{frame.height} frames_read={self._frames_read} "
            f"fps={frames_per_second:.2f} "
            f"capture_to_read_ms {self._capture_to_read.summary_in_milliseconds()} | "
            f"capture_interval_ms {self._capture_interval.summary_in_milliseconds()}"
        )
        self._window_first_read_ns = None

# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`CaptureToDownstreamLatencyProbe`: how late and how often camera frames reach a downstream node.

A frame's `timestamp_ns` is its capture instant on the stream's clock, which in
a real-time stream is the machine's monotonic clock, so in a real-time stream
the probe measures the clock when the frame is read, less the clock when it was
captured.
"""

from __future__ import annotations

from typing import Annotated, Callable

from tatolab.stream import (
    Description,
    Texture,
    Input,
    InputPin,
    NumberRange,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    log,
    node,
)
from tatolab.stream.clock import monotonic_now_ns

from ._duration_samples import DurationSamplesInOneReportingWindow

@node.meta(description="Logs the frame rate and capture-to-read latency of the frames it receives")
@node(execution="reactive")
def CaptureToDownstreamLatencyProbe(
    *,
    camera_label: Input[
        Annotated[str, Description("Names the camera under test in every report.")]
    ] = Input("camera"),
    frames_between_reports: Input[
        Annotated[int, NumberRange(minimum=1), Description("How many frames one report summarises.")]
    ] = Input(600),
    video_from_upstream: InputPin[
        Annotated[
            Texture, StreamDelivery("ordered"), Description("Frames from the camera under test")
        ]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Video frames in; a report every `frames_between_reports` frames.

    Its label and its count follow their values from the next frame on.
    """
    capture_to_read = DurationSamplesInOneReportingWindow()
    capture_interval = DurationSamplesInOneReportingWindow()
    frames_read = 0
    window_first_read_ns: "int | None" = None
    previous_capture_ns: "int | None" = None

    def report(frame: Texture, read_ns: int, frames_in_a_report: int) -> None:
        window_seconds = max(read_ns - (window_first_read_ns or read_ns), 1) / 1_000_000_000
        frames_per_second = (frames_in_a_report - 1) / window_seconds
        log.info(
            f"CaptureToDownstreamLatencyProbe[{camera_label.get()}]: "
            f"{frame.width}x{frame.height} frames_read={frames_read} "
            f"fps={frames_per_second:.2f} "
            f"capture_to_read_ms {capture_to_read.summary_in_milliseconds()} | "
            f"capture_interval_ms {capture_interval.summary_in_milliseconds()}"
        )

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal frames_read, window_first_read_ns, previous_capture_ns
        del ctx
        while (frame := video_from_upstream.read()) is not None:
            read_ns = monotonic_now_ns()
            capture_to_read.add(read_ns - frame.timestamp_ns)
            if previous_capture_ns is not None:
                capture_interval.add(frame.timestamp_ns - previous_capture_ns)
            previous_capture_ns = frame.timestamp_ns
            if window_first_read_ns is None:
                window_first_read_ns = read_ns
            frames_read += 1
            frames_in_a_report = frames_between_reports.get()
            if frames_read % frames_in_a_report == 0:
                report(frame, read_ns, frames_in_a_report)
                window_first_read_ns = None

    return each_frame

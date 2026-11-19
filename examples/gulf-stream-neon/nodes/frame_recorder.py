# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Writes every program frame it receives into a video file, in order.

Importable as `nodes.frame_recorder:FrameRecorder`. Each frame is read back
through `frame.cpu()` and piped into ffmpeg as raw RGBA and kept lossless (FFV1), for the edit; with the stream on a
stepped clock, no frame is ever dropped, however slow the GPU is.
"""

from __future__ import annotations

import subprocess
from dataclasses import dataclass
from pathlib import Path

import numpy
from tatolab.stream import Image, Input, RuntimeContextFullAccess, RuntimeContextLimitedAccess, log, node


@dataclass
class FrameRecorderConfig:
    """FrameRecorder's settings: where the take goes, relative to the project."""

    output_path: str = "capture/take.mkv"
    frames_per_second: int = 30


@node.meta(
    category="Gulf Stream Neon",
    description="An ordered sink: every frame it receives, read back and piped into ffmpeg, so a stepped take records frame-exact.",
)
@node.process
class FrameRecorder:
    """An ordered sink that pipes frames into ffmpeg."""

    def __init__(self, config: FrameRecorderConfig) -> None:
        self.config = config

    program = Input(Image, delivery="ordered", description="The frames to record")

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        path = Path(self.config.output_path)
        if not path.is_absolute():
            path = Path(__file__).resolve().parent.parent / path
        path.parent.mkdir(parents=True, exist_ok=True)
        self.path = path
        self.encoder: subprocess.Popen | None = None
        self.frames_written = 0

    def _start_encoder(self, width: int, height: int) -> None:
        self.encoder = subprocess.Popen(
            [
                "ffmpeg", "-y", "-loglevel", "error",
                "-f", "rawvideo", "-pix_fmt", "rgba", "-s", f"{width}x{height}",
                "-r", str(self.config.frames_per_second), "-i", "-",
                "-c:v", "ffv1", "-level", "3", "-slices", "24", "-threads", "16", "-pix_fmt", "yuv444p",
                str(self.path),
            ],
            stdin=subprocess.PIPE,
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(self.program)
        if frame is None:
            return
        if self.encoder is None:
            self._start_encoder(frame.width, frame.height)
        with frame.cpu() as pixels:
            if pixels.dtype.kind == "f":
                # An HDR frame: a quick Reinhard curve and sRGB-ish gamma, enough to look at it.
                linear = numpy.asarray(pixels[:, :, :3], dtype=numpy.float32)
                display = numpy.power(linear / (1.0 + linear), 1.0 / 2.2)
                rgba = numpy.empty(pixels.shape[:2] + (4,), numpy.uint8)
                rgba[:, :, :3] = numpy.clip(display * 255.0, 0, 255).astype(numpy.uint8)
                rgba[:, :, 3] = 255
            else:
                rgba = numpy.ascontiguousarray(pixels[:, :, :4])
        self.encoder.stdin.write(rgba.tobytes())
        self.frames_written += 1
        if self.frames_written % 30 == 0:
            log.info("recorded", frames=self.frames_written, path=str(self.path))

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        if self.encoder is not None:
            self.encoder.stdin.close()
            self.encoder.wait(timeout=120)
            log.info("take closed", frames=self.frames_written, path=str(self.path))

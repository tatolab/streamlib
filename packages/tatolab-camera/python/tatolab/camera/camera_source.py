# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`CameraSource`: a camera whose every frame reaches the GPU with no CPU copy.

`start()` lends the `video` output to this wheel's Rust, which adopts each
capture buffer once through the module host table — a V4L2 driver buffer
exported as a DMA-BUF on Linux, an AVFoundation IOSurface on macOS — and per
frame converts it on the GPU into a texture from the node's output pool and
publishes a `VideoFrame` naming it. Python runs at start and stop, never at
frame cadence.
"""

from __future__ import annotations

import dataclasses
import sys
from typing import Annotated

from tatolab.stream import RuntimeContextFullAccess, VideoFrame, log, node

from . import _native

VIDEO_OUTPUT_PORT = "video"

if sys.platform == "darwin":
    _CameraCapture = _native.AvFoundationZeroCopyCameraCapture
else:
    _CameraCapture = _native.V4l2ZeroCopyCameraCapture


@dataclasses.dataclass
class CameraSourceConfig:
    """What a `CameraSource` is configured with."""

    device_id: Annotated[
        "str | None",
        "A V4L2 node such as `/dev/video0` on Linux, an AVFoundation camera's unique ID on "
        "macOS; the first camera found when absent, on macOS the built-in one first.",
    ] = None
    max_width: Annotated[int, "The widest frame size negotiated."] = 1920
    max_height: Annotated[int, "The tallest frame size negotiated."] = 1080
    output_pool_rotation_depth: Annotated[
        int, "How many converted frames the output pool rotates before reusing a free slot."
    ] = 4


@node(
    execution="manual",
    description=(
        "Captures a camera — V4L2 on Linux, AVFoundation on macOS — and converts each frame "
        "on the GPU, with no CPU copy. On macOS a stream someone loads or starts asks for camera "
        "access without waiting, and frames begin once the user allows it; a stream the runtime "
        "re-loads on its own never asks, and is refused at start until access has been answered."
    ),
)
class CameraSource:
    """A camera, published as RGBA `VideoFrame`s."""

    def __init__(self, config: CameraSourceConfig) -> None:
        self._config = config
        self._capture: "_CameraCapture | None" = None

    @node.output(description="RGBA frames from the camera, as VideoFrame records")
    def video(self) -> VideoFrame: ...

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        module_host_table = ctx.open_module_host_table(output_ports=[VIDEO_OUTPUT_PORT])
        if sys.platform == "darwin":
            avfoundation_capture = _native.AvFoundationZeroCopyCameraCapture(
                module_host_table,
                VIDEO_OUTPUT_PORT,
                self._config.device_id,
                self._config.max_width,
                self._config.max_height,
                self._config.output_pool_rotation_depth,
            )
            self._capture = avfoundation_capture
            log.info(
                f"CameraSource: {avfoundation_capture.device_name} "
                f"({avfoundation_capture.device_id}) streams {avfoundation_capture.fourcc} "
                f"{avfoundation_capture.width}x{avfoundation_capture.height} at "
                f"{avfoundation_capture.nominal_frames_per_second} fps"
            )
            return
        v4l2_capture = _native.V4l2ZeroCopyCameraCapture(
            module_host_table,
            VIDEO_OUTPUT_PORT,
            self._config.device_id,
            self._config.max_width,
            self._config.max_height,
            self._config.output_pool_rotation_depth,
        )
        self._capture = v4l2_capture
        log.info(
            f"CameraSource: {v4l2_capture.device_path} ({v4l2_capture.card_name}) streams "
            f"{v4l2_capture.fourcc} {v4l2_capture.width}x{v4l2_capture.height} at "
            f"{v4l2_capture.nominal_frames_per_second} fps, {v4l2_capture.bytes_per_line} bytes "
            f"a row, colour primaries={v4l2_capture.colour_primaries} "
            f"transfer={v4l2_capture.colour_transfer} matrix={v4l2_capture.colour_matrix} "
            f"range={v4l2_capture.colour_range}"
        )

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_capturing()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_capturing()

    def _stop_capturing(self) -> None:
        if self._capture is None:
            return
        self._capture.stop()
        self._capture = None

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""`CameraSource`: a camera whose every frame reaches the GPU with no CPU copy.

`setup()` lends the `video` output to this wheel's Rust, which opens the
camera and adopts each
capture buffer once through the module host table — a V4L2 driver buffer
exported as a DMA-BUF on Linux, an AVFoundation IOSurface on macOS — and per
frame converts it on the GPU into a texture from the node's output pool and
publishes an Image frame in its slot once `start()` runs. Python runs at setup,
start and stop, never at frame cadence.
"""

from __future__ import annotations

import dataclasses
import sys
from typing import Annotated

from tatolab.stream import Image, Output, RuntimeContextFullAccess, log, node

from . import _native

VIDEO_OUTPUT_PORT = "video"
CONVERTED_FRAME_TEXTURE_FORMAT = "rgba8_unorm"

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


@node.meta(
    description=(
        "Captures a camera — V4L2 on Linux, AVFoundation on macOS — and converts each frame "
        "on the GPU, with no CPU copy. On macOS a stream someone loads or starts asks for camera "
        "access without waiting, and frames begin once the user allows it; a stream the runtime "
        "re-loads on its own never asks, and is refused at setup until access has been answered."
    ),
)
@node.process(execution="manual", paced_by_real_time=True)
class CameraSource:
    """A camera, published as RGBA Image frames."""

    def __init__(self, config: CameraSourceConfig) -> None:
        self._config = config
        self._capture: "_CameraCapture | None" = None

    video = Output(
        Image,
        formats=(CONVERTED_FRAME_TEXTURE_FORMAT,),
        description="RGBA frames from the camera, each colour as captured",
    )

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        """Lend `video` to the native half and open the camera, so a camera that
        cannot be opened, or that camera access refuses, refuses the node here."""
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
                f"({avfoundation_capture.device_id}) opened in {avfoundation_capture.fourcc} "
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
            f"CameraSource: {v4l2_capture.device_path} ({v4l2_capture.card_name}) opened in "
            f"{v4l2_capture.fourcc} {v4l2_capture.width}x{v4l2_capture.height} at "
            f"{v4l2_capture.nominal_frames_per_second} fps, {v4l2_capture.bytes_per_line} bytes "
            f"a row, colour primaries={v4l2_capture.colour_primaries} "
            f"transfer={v4l2_capture.colour_transfer} matrix={v4l2_capture.colour_matrix} "
            f"range={v4l2_capture.colour_range}"
        )

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        if self._capture is None:
            raise RuntimeError("CameraSource: start() ran before setup() opened the camera")
        self._capture.start()

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

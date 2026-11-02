# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`CameraSource`: a V4L2 camera whose every frame reaches the GPU with no CPU copy.

`start()` lends the `video` output to this wheel's Rust, which exports each
V4L2 driver buffer as a DMA-BUF, adopts it once through the module host table,
and per frame converts it on the GPU into a texture from the node's output
pool and publishes a `VideoFrame` naming it. Python runs at start and stop,
never at frame cadence.
"""

from __future__ import annotations

import dataclasses
from typing import Annotated

from tatolab.stream import RuntimeContextFullAccess, VideoFrame, log, node

from . import _native

VIDEO_OUTPUT_PORT = "video"


@dataclasses.dataclass
class CameraSourceConfig:
    """What a `CameraSource` is configured with."""

    device_path: Annotated[
        "str | None", "A V4L2 node such as `/dev/video0`; the first capture device when absent."
    ] = None
    max_width: Annotated[int, "The widest frame size negotiated."] = 1920
    max_height: Annotated[int, "The tallest frame size negotiated."] = 1080
    output_pool_rotation_depth: Annotated[
        int, "How many converted frames the output pool rotates before reusing a free slot."
    ] = 4


@node(
    execution="manual",
    description="Captures a V4L2 camera and converts each frame on the GPU, with no CPU copy",
)
class CameraSource:
    """A V4L2 camera, published as RGBA `VideoFrame`s."""

    def __init__(self, config: CameraSourceConfig) -> None:
        self._config = config
        self._capture: "_native.V4l2ZeroCopyCameraCapture | None" = None

    @node.output(description="RGBA frames from the camera, as VideoFrame records")
    def video(self) -> VideoFrame: ...

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        module_host_table = ctx.open_module_host_table(output_ports=[VIDEO_OUTPUT_PORT])
        capture = _native.V4l2ZeroCopyCameraCapture(
            module_host_table,
            VIDEO_OUTPUT_PORT,
            self._config.device_path,
            self._config.max_width,
            self._config.max_height,
            self._config.output_pool_rotation_depth,
        )
        self._capture = capture
        log.info(
            f"CameraSource: {capture.device_path} ({capture.card_name}) streams "
            f"{capture.fourcc} {capture.width}x{capture.height} at "
            f"{capture.nominal_frames_per_second} fps, {capture.bytes_per_line} bytes a row, "
            f"colour primaries={capture.colour_primaries} transfer={capture.colour_transfer} "
            f"matrix={capture.colour_matrix} range={capture.colour_range}"
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

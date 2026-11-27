# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`CameraSource`: a camera whose every frame reaches the GPU with no CPU copy.

The node's body lends the `video` output to this wheel's Rust once, and its one
computed value is the open camera: the Rust opens it and adopts each capture
buffer once through the module host table — a V4L2 driver buffer exported as a
DMA-BUF on Linux, an AVFoundation IOSurface on macOS — and per frame converts
it on the GPU into a texture from the node's output pool and publishes an Image
frame in its slot. Python runs only when an input changes, never at frame
cadence.
"""

from __future__ import annotations

import sys
from typing import Annotated

from tatolab.stream import (
    Description,
    Input,
    NumberRange,
    OutputPin,
    RuntimeContextFullAccess,
    Texture,
    TexturePixelFormats,
    computed,
    log,
    node,
)

from . import _native

VIDEO_OUTPUT_PORT = "video"
CONVERTED_FRAME_TEXTURE_FORMAT = "rgba8_unorm"

if sys.platform == "darwin":
    _CameraCapture = _native.AvFoundationZeroCopyCameraCapture
else:
    _CameraCapture = _native.V4l2ZeroCopyCameraCapture


def _log_the_camera_opened(capture: "_CameraCapture") -> None:
    if sys.platform == "darwin":
        log.info(
            f"CameraSource: {capture.device_name} ({capture.device_id}) opened in "
            f"{capture.fourcc} {capture.width}x{capture.height} at "
            f"{capture.nominal_frames_per_second} fps"
        )
        return
    log.info(
        f"CameraSource: {capture.device_path} ({capture.card_name}) opened in "
        f"{capture.fourcc} {capture.width}x{capture.height} at "
        f"{capture.nominal_frames_per_second} fps, {capture.bytes_per_line} bytes "
        f"a row, colour primaries={capture.colour_primaries} "
        f"transfer={capture.colour_transfer} matrix={capture.colour_matrix} "
        f"range={capture.colour_range}"
    )


@node.meta(
    description=(
        "Captures a camera — V4L2 on Linux, AVFoundation on macOS — and converts each frame "
        "on the GPU, with no CPU copy. On macOS a stream someone loads or starts asks for camera "
        "access without waiting, and frames begin once the user allows it; a stream the runtime "
        "re-loads on its own never asks, and its camera fails to open until access has been "
        "answered."
    ),
)
@node(execution="manual", paced_by_real_time=True)
def CameraSource(
    *,
    device_id: Input[
        Annotated[
            str | None,
            Description(
                "A V4L2 node such as `/dev/video0` on Linux, an AVFoundation camera's unique ID "
                "on macOS; the first camera found when unset, on macOS the built-in one first."
            ),
        ]
    ] = Input(None),
    max_width: Input[
        Annotated[int, NumberRange(minimum=1), Description("The widest frame size negotiated.")]
    ] = Input(1920),
    max_height: Input[
        Annotated[int, NumberRange(minimum=1), Description("The tallest frame size negotiated.")]
    ] = Input(1080),
    output_pool_rotation_depth: Input[
        Annotated[
            int,
            NumberRange(minimum=1),
            Description(
                "How many converted frames the output pool rotates before reusing a free slot."
            ),
        ]
    ] = Input(4),
    video: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(CONVERTED_FRAME_TEXTURE_FORMAT),
            Description("RGBA frames from the camera, each colour as captured"),
        ]
    ],
    ctx: RuntimeContextFullAccess,
) -> None:
    """A camera, published as RGBA pictures.

    The open camera is a computed value: a change to any input closes the capture it has and
    opens the camera again while the stream plays, so two captures never publish on `video` at
    once. A camera that fails to open shows on the node, and nothing publishes until the next
    set.
    """
    del video  # Lent to the native half, which publishes on it.
    module_host_table = ctx.open_module_host_table(output_ports=[VIDEO_OUTPUT_PORT])

    open_now: "list[_CameraCapture]" = []

    def open_the_camera() -> "_CameraCapture":
        # The capture this one replaces closes first, so two cameras never publish on `video`.
        while open_now:
            open_now.pop().close()
        capture = _CameraCapture(
            module_host_table,
            VIDEO_OUTPUT_PORT,
            device_id.get() or None,
            max_width.get(),
            max_height.get(),
            output_pool_rotation_depth.get(),
        )
        open_now.append(capture)
        _log_the_camera_opened(capture)
        try:
            capture.start()
        except BaseException:
            open_now.remove(capture)
            capture.close()
            raise
        return capture

    computed(open_the_camera)

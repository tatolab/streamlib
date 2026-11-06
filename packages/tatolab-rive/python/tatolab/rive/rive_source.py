# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`RiveSource`: a Rive animation drawn straight into the stream's frames.

`start()` lends the `video` output and the `data` input to this wheel's Rust.
A thread there creates a Vulkan device on the runtime's GPU through the
consumer RHI, builds rive-runtime's Vulkan renderer on it, and per frame draws
the `.riv` state machine into a texture from the node's output pool and
publishes an Image frame in its slot. No frame is copied, and Python never runs
at frame cadence.
"""

from __future__ import annotations

import dataclasses
from typing import Annotated

from tatolab.stream import Image, Input, Output, Records, RuntimeContextFullAccess, node

from . import _native

VIDEO_OUTPUT_PORT = "video"
RENDERED_FRAME_TEXTURE_FORMAT = "rgba8_unorm"
DATA_INPUT_PORT = "data"


@dataclasses.dataclass
class RiveSourceConfig:
    """What a `RiveSource` is configured with."""

    riv_path: Annotated[str, "The `.riv` file to play."]
    artboard: Annotated["str | None", "The artboard to play; the file's default when absent."] = None
    state_machine: Annotated[
        "str | None", "The artboard's state machine to play; its default when absent."
    ] = None
    width: Annotated[int, "Frame width in pixels; the artboard is fit inside, centred."] = 1920
    height: Annotated[int, "Frame height in pixels."] = 1080
    frame_rate: Annotated[int, "Frames drawn and published per second."] = 60
    output_pool_rotation_depth: Annotated[
        int, "How many frames the output pool rotates before reusing a free slot."
    ] = 4


@node(
    execution="manual",
    description="Plays a Rive state machine, drawing each frame straight into the stream on the GPU",
)
class RiveSource:
    """A `.riv` file's state machine, published as RGBA Image frames over transparency.

    Each `data` record is a map from view-model property path to value,
    applied to the artboard's default view-model instance: a number to a number
    property, text to a string or enum property, a boolean to a boolean
    property, `0xAARRGGBB` or `"#RRGGBB"` to a colour property, and anything but
    `false` fires a trigger. A path the instance lacks, or a value its property
    cannot take, is logged once and skipped.
    """

    def __init__(self, config: RiveSourceConfig) -> None:
        self._config = config
        self._rendering: "_native.RiveSceneRendering | None" = None

    data = Input(
        Records,
        delivery="ordered",
        description="Maps from view-model property path to value",
    )
    video = Output(
        Image,
        formats=(RENDERED_FRAME_TEXTURE_FORMAT,),
        description="RGBA frames of the animation, colour unspecified",
    )

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        """Lend both ports to this wheel's frame thread.

        The device and the renderer are built on that thread, so their start-up
        never spends the helper's start-up budget; it logs what it plays.
        """
        module_host_table = ctx.open_module_host_table(
            output_ports=[VIDEO_OUTPUT_PORT], input_ports=[DATA_INPUT_PORT]
        )
        self._rendering = _native.RiveSceneRendering(
            module_host_table,
            VIDEO_OUTPUT_PORT,
            DATA_INPUT_PORT,
            self._config.riv_path,
            self._config.artboard,
            self._config.state_machine,
            self._config.width,
            self._config.height,
            self._config.frame_rate,
            self._config.output_pool_rotation_depth,
        )

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_rendering()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_rendering()

    def _stop_rendering(self) -> None:
        if self._rendering is None:
            return
        self._rendering.stop()
        self._rendering = None

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""`RiveSource`: a Rive animation drawn straight into the stream's frames.

`start()` lends the `video` output and the `data` input to this wheel's Rust,
once `riv_path` names a `.riv` file that is there; until then the node reports
itself not ready, naming the setting or the file, and the runtime sends
transparent frames on `video` meanwhile.
A thread there creates a Vulkan device on the runtime's GPU through the
consumer RHI, builds rive-runtime's Vulkan renderer on it, and per frame draws
the `.riv` state machine into a texture from the node's output pool and
publishes an Image frame in its slot. No frame is copied, and Python never runs
at frame cadence.
"""

from __future__ import annotations

import dataclasses
import os
from typing import Annotated, Any

from tatolab.stream import Image, Input, Output, Records, RuntimeContextFullAccess, node

from . import _native

VIDEO_OUTPUT_PORT = "video"
RENDERED_FRAME_TEXTURE_FORMAT = "rgba8_unorm"
DATA_INPUT_PORT = "data"


@dataclasses.dataclass
class RiveSourceConfig:
    """What a `RiveSource` is configured with."""

    riv_path: Annotated[
        "str | None",
        "The `.riv` file to play; until it names one that is there, the node is not ready and sends transparent frames.",
    ] = None
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


@node.meta(
    description="Plays a Rive state machine, drawing each frame straight into the stream on the GPU",
)
@node.process(execution="manual")
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
        self._context: "RuntimeContextFullAccess | None" = None
        self._module_host_table: Any = None

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
        """Lend both ports to this wheel's frame thread, or report the node not
        ready while `riv_path` names no `.riv` file that is there.

        The device and the renderer are built on that thread, so their start-up
        never spends the helper's start-up budget; it logs what it plays.
        """
        self._context = ctx
        self._start_rendering_or_report_why_not()

    def configure(self, config: RiveSourceConfig) -> None:
        """Play what `config` names from now on, once the node has started."""
        self._stop_rendering()
        self._config = config
        if self._context is not None:
            self._start_rendering_or_report_why_not()

    def _start_rendering_or_report_why_not(self) -> None:
        ctx = self._context
        assert ctx is not None
        riv_path = self._config.riv_path
        if not riv_path:
            self._report_not_ready("`riv_path` is not set; it takes the `.riv` file to play")
            return
        if not os.path.isfile(riv_path):
            self._report_not_ready(f"the `.riv` file `riv_path` names, {riv_path}, is missing")
            return
        if self._module_host_table is None:
            self._module_host_table = ctx.open_module_host_table(
                output_ports=[VIDEO_OUTPUT_PORT], input_ports=[DATA_INPUT_PORT]
            )
        try:
            self._rendering = _native.RiveSceneRendering(
                self._module_host_table,
                VIDEO_OUTPUT_PORT,
                DATA_INPUT_PORT,
                riv_path,
                self._config.artboard,
                self._config.state_machine,
                self._config.width,
                self._config.height,
                self._config.frame_rate,
                self._config.output_pool_rotation_depth,
            )
        except RuntimeError as refusal:
            self._report_not_ready(str(refusal))
            return
        ctx.report_ready()

    def _report_not_ready(self, reason: str) -> None:
        assert self._context is not None
        self._context.report_not_ready(
            f"RiveSource: {reason}",
            image_width=max(self._config.width, 1),
            image_height=max(self._config.height, 1),
        )

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._context = None
        self._stop_rendering()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_rendering()

    def _stop_rendering(self) -> None:
        if self._rendering is None:
            return
        self._rendering.stop()
        self._rendering = None

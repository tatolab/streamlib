# Copyright (c) 2025 Tatolab LLC
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

import os
from typing import Annotated, Any

from tatolab.stream import (
    Description,
    InputPin,
    NumberRange,
    OutputPin,
    RuntimeContextFullAccess,
    SettingOnly,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    node,
)

from . import _native
from .view_model_values import ViewModelValues

VIDEO_OUTPUT_PORT = "video"
RENDERED_FRAME_TEXTURE_FORMAT = "rgba8_unorm"
DATA_INPUT_PORT = "data"


@node.meta(
    description="Plays a Rive state machine, drawing each frame straight into the stream on the GPU",
)
@node(execution="manual")
class RiveSource:
    """A `.riv` file's state machine, published as RGBA pictures over transparency.

    Each setting is read at start, where the animation is loaded, so a change loads it again.
    Each `data` message's values are applied to the artboard's default view-model instance; a
    path the instance lacks, or a value its property cannot take, is logged once and skipped.
    """

    riv_path: Annotated[
        str | None,
        SettingOnly(),
        Description(
            "The `.riv` file to play; until it names one that is there, the node is not ready "
            "and sends transparent frames."
        ),
    ] = None
    artboard: Annotated[
        str | None, SettingOnly(), Description("The artboard to play; the file's default when unset.")
    ] = None
    state_machine: Annotated[
        str | None,
        SettingOnly(),
        Description("The artboard's state machine to play; its default when unset."),
    ] = None
    width: Annotated[
        int,
        SettingOnly(),
        NumberRange(minimum=1),
        Description("Frame width in pixels; the artboard is fit inside, centred."),
    ] = 1920
    height: Annotated[
        int, SettingOnly(), NumberRange(minimum=1), Description("Frame height in pixels.")
    ] = 1080
    frame_rate: Annotated[
        int,
        SettingOnly(),
        NumberRange(minimum=1),
        Description("Frames drawn and published per second."),
    ] = 60
    output_pool_rotation_depth: Annotated[
        int,
        SettingOnly(),
        NumberRange(minimum=1),
        Description("How many frames the output pool rotates before reusing a free slot."),
    ] = 4
    data: InputPin[
        Annotated[
            ViewModelValues,
            StreamDelivery("ordered"),
            Description("Values for the artboard's view-model properties"),
        ]
    ]
    video: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(RENDERED_FRAME_TEXTURE_FORMAT),
            Description("RGBA frames of the animation, colour unspecified"),
        ]
    ]

    def __init__(self) -> None:
        self._rendering: "_native.RiveSceneRendering | None" = None
        self._context: "RuntimeContextFullAccess | None" = None
        self._module_host_table: Any = None

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        """Lend both ports to this wheel's frame thread, or report the node not
        ready while `riv_path` names no `.riv` file that is there.

        The device and the renderer are built on that thread, so their start-up
        never spends the helper's start-up budget; it logs what it plays.
        """
        self._context = ctx
        self._start_rendering_or_report_why_not()

    def _start_rendering_or_report_why_not(self) -> None:
        ctx = self._context
        assert ctx is not None
        riv_path = self.riv_path
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
                self.artboard,
                self.state_machine,
                self.width,
                self.height,
                self.frame_rate,
                self.output_pool_rotation_depth,
            )
        except RuntimeError as refusal:
            self._report_not_ready(str(refusal))
            return
        ctx.report_ready()

    def _report_not_ready(self, reason: str) -> None:
        assert self._context is not None
        self._context.report_not_ready(
            f"RiveSource: {reason}",
            image_width=max(self.width, 1),
            image_height=max(self.height, 1),
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

# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`RiveSource`: a Rive animation drawn straight into the stream's frames.

The node's body lends the `video` output and the `data` input to this wheel's
Rust once. The scene is a computed value: each build reads the `.riv` file and
starts a thread there that creates a Vulkan device on the runtime's GPU through
the consumer RHI, builds rive-runtime's Vulkan renderer on it, and per frame
draws the state machine into a texture from the node's output pool and
publishes an Image frame in its slot. No frame is copied, and Python never runs
at frame cadence.

Changing any input while the stream plays builds a new scene off the frame
thread, while the one before keeps publishing; the new one is taken at the
node's next tick and the one before is closed, releasing its device and its
render targets. A size change publishes into slots of the new size, and the
output pool releases the old ones once nothing holds them. A build that fails
leaves the scene before it publishing.
"""

from __future__ import annotations

import os
from typing import Annotated

from tatolab.stream import (
    Description,
    Input,
    InputPin,
    NumberRange,
    OutputPin,
    RuntimeContextFullAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    computed,
    node,
)

from . import _native
from .view_model_values import ViewModelValues

VIDEO_OUTPUT_PORT = "video"
RENDERED_FRAME_TEXTURE_FORMAT = "rgba8_unorm"
DATA_INPUT_PORT = "data"


class RiveFileNotNamed(ValueError):
    """`riv_path` is unset, so the node has no `.riv` file to play."""


class RiveFileMissing(FileNotFoundError):
    """`riv_path` names a `.riv` file that is not there."""


def the_riv_file_named_by(riv_path: "str | None") -> str:
    """`riv_path`, once it names a file that is there."""
    if not riv_path:
        raise RiveFileNotNamed("RiveSource: `riv_path` is not set; it takes the `.riv` file to play")
    if not os.path.isfile(riv_path):
        raise RiveFileMissing(f"RiveSource: the `.riv` file `riv_path` names, {riv_path}, is missing")
    return riv_path


@node.meta(
    description="Plays a Rive state machine, drawing each frame straight into the stream on the GPU",
)
@node(execution="manual")
def RiveSource(
    *,
    riv_path: Input[
        Annotated[
            str | None,
            Description(
                "The `.riv` file to play; until it names one that is there, the node plays nothing."
            ),
        ]
    ] = Input(None),
    artboard: Input[
        Annotated[str | None, Description("The artboard to play; the file's default when unset.")]
    ] = Input(None),
    state_machine: Input[
        Annotated[
            str | None, Description("The artboard's state machine to play; its default when unset.")
        ]
    ] = Input(None),
    width: Input[
        Annotated[
            int,
            NumberRange(minimum=1),
            Description("Frame width in pixels; the artboard is fit inside, centred."),
        ]
    ] = Input(1920),
    height: Input[
        Annotated[int, NumberRange(minimum=1), Description("Frame height in pixels.")]
    ] = Input(1080),
    frame_rate: Input[
        Annotated[int, NumberRange(minimum=1), Description("Frames drawn and published per second.")]
    ] = Input(60),
    output_pool_rotation_depth: Input[
        Annotated[
            int,
            NumberRange(minimum=1),
            Description("How many frames the output pool rotates before reusing a free slot."),
        ]
    ] = Input(4),
    data: InputPin[
        Annotated[
            ViewModelValues,
            StreamDelivery("ordered"),
            Description("Values for the artboard's view-model properties"),
        ]
    ],
    video: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(RENDERED_FRAME_TEXTURE_FORMAT),
            Description("RGBA frames of the animation, colour unspecified"),
        ]
    ],
    ctx: RuntimeContextFullAccess,
) -> None:
    """A `.riv` file's state machine, published as RGBA pictures over transparency.

    Each `data` message's values are applied to the artboard's default view-model instance; a
    path the instance lacks, or a value its property cannot take, is logged once and skipped.
    """
    module_host_table = ctx.open_module_host_table(
        output_ports=[VIDEO_OUTPUT_PORT], input_ports=[DATA_INPUT_PORT]
    )

    def open_the_scene() -> _native.RiveSceneRendering:
        return _native.RiveSceneRendering(
            module_host_table,
            VIDEO_OUTPUT_PORT,
            DATA_INPUT_PORT,
            the_riv_file_named_by(riv_path.get()),
            artboard.get(),
            state_machine.get(),
            width.get(),
            height.get(),
            frame_rate.get(),
            output_pool_rotation_depth.get(),
        )

    computed(open_the_scene)

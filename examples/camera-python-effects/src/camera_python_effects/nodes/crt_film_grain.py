# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""CRT tube simulation and film grain, as one fullscreen pass.

Every dial is a value input read each frame, so dragging `barrel_curve` to 0.0 in the
Studio flattens the tube while the stream plays, without touching the shader.
"""

from __future__ import annotations

import struct
from typing import Annotated, Callable

from tatolab.stream import (
    Description,
    Input,
    NumberRange,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    Texture,
    node,
)

from ..single_pass_video_effect import (
    VideoFromUpstream,
    VideoToDownstream,
    each_frame_of_a_single_pass_video_effect,
)

# `float2 frame_extent_in_pixels;` then elapsed seconds and the seven dials below.
CRT_PUSH_CONSTANT_FORMAT = "<10f"
CRT_PUSH_CONSTANT_SIZE = struct.calcsize(CRT_PUSH_CONSTANT_FORMAT)


@node.meta(description="80s CRT tube and film grain over the whole frame")
@node(execution="reactive")
def CrtFilmGrain(
    *,
    barrel_curve: Input[
        Annotated[
            float,
            NumberRange(minimum=0.0, maximum=1.0),
            Description("How far the tube bulges; 0 is a flat screen."),
        ]
    ] = Input(0.35),
    scanline_intensity: Input[
        Annotated[
            float, NumberRange(minimum=0.0, maximum=1.0), Description("How dark the scanlines are.")
        ]
    ] = Input(0.5),
    chromatic_aberration: Input[
        Annotated[
            float,
            NumberRange(minimum=0.0, maximum=0.02),
            Description("How far red and blue split from green, as a fraction of the frame."),
        ]
    ] = Input(0.0025),
    grain_intensity: Input[
        Annotated[
            float, NumberRange(minimum=0.0, maximum=1.0), Description("How strong the film grain is.")
        ]
    ] = Input(0.12),
    grain_speed: Input[
        Annotated[
            float,
            NumberRange(minimum=0.0, maximum=4.0),
            Description("How fast the grain changes; 1 is 24 grain frames a second."),
        ]
    ] = Input(1.0),
    vignette_intensity: Input[
        Annotated[
            float,
            NumberRange(minimum=0.0, maximum=1.0),
            Description("How much the corners darken."),
        ]
    ] = Input(0.6),
    brightness: Input[
        Annotated[
            float, NumberRange(minimum=0.0, maximum=2.0), Description("The picture's overall gain.")
        ]
    ] = Input(1.05),
    video_from_upstream: VideoFromUpstream,
    video_to_downstream: VideoToDownstream,
    ctx: RuntimeContextFullAccess,
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Barrel curve, scanlines, aberration, vignette and 24 fps grain."""

    def push_constants_for(frame: Texture, elapsed_seconds: float) -> bytes:
        return struct.pack(
            CRT_PUSH_CONSTANT_FORMAT,
            float(frame.width),
            float(frame.height),
            elapsed_seconds,
            barrel_curve.get(),
            scanline_intensity.get(),
            chromatic_aberration.get(),
            grain_intensity.get(),
            grain_speed.get(),
            vignette_intensity.get(),
            brightness.get(),
        )

    return each_frame_of_a_single_pass_video_effect(
        ctx,
        label="CrtFilmGrain",
        fragment_shader_file_name="crt_film_grain_fragment.slang",
        push_constant_size=CRT_PUSH_CONSTANT_SIZE,
        push_constants_for=push_constants_for,
        video_from_upstream=video_from_upstream,
        video_to_downstream=video_to_downstream,
    )

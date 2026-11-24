# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""CRT tube simulation and film grain, as one fullscreen pass.

Every dial is a field read each frame, so dragging `barrel_curve` to 0.0 in the
Studio flattens the tube while the stream plays, without touching the shader.
"""

from __future__ import annotations

import struct
from typing import Annotated, ClassVar

from tatolab.stream import Description, Texture, NumberRange, node

from ..single_pass_video_effect import SinglePassVideoEffect

# `float2 frame_extent_in_pixels;` then elapsed seconds and the seven dials below.
CRT_PUSH_CONSTANT_FORMAT = "<10f"
CRT_PUSH_CONSTANT_SIZE = struct.calcsize(CRT_PUSH_CONSTANT_FORMAT)


@node.meta(description="80s CRT tube and film grain over the whole frame")
@node
class CrtFilmGrain(SinglePassVideoEffect):
    """Barrel curve, scanlines, aberration, vignette and 24 fps grain."""

    fragment_shader_file_name: ClassVar[str] = "crt_film_grain_fragment.slang"
    push_constant_size: ClassVar[int] = CRT_PUSH_CONSTANT_SIZE

    barrel_curve: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=1.0),
        Description("How far the tube bulges; 0 is a flat screen."),
    ] = 0.35
    scanline_intensity: Annotated[
        float, NumberRange(minimum=0.0, maximum=1.0), Description("How dark the scanlines are.")
    ] = 0.5
    chromatic_aberration: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=0.02),
        Description("How far red and blue split from green, as a fraction of the frame."),
    ] = 0.0025
    grain_intensity: Annotated[
        float, NumberRange(minimum=0.0, maximum=1.0), Description("How strong the film grain is.")
    ] = 0.12
    grain_speed: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=4.0),
        Description("How fast the grain changes; 1 is 24 grain frames a second."),
    ] = 1.0
    vignette_intensity: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=1.0),
        Description("How much the corners darken."),
    ] = 0.6
    brightness: Annotated[
        float, NumberRange(minimum=0.0, maximum=2.0), Description("The picture's overall gain.")
    ] = 1.05

    def push_constants_for(self, frame: Texture, elapsed_seconds: float) -> bytes:
        return struct.pack(
            CRT_PUSH_CONSTANT_FORMAT,
            float(frame.width),
            float(frame.height),
            elapsed_seconds,
            self.barrel_curve,
            self.scanline_intensity,
            self.chromatic_aberration,
            self.grain_intensity,
            self.grain_speed,
            self.vignette_intensity,
            self.brightness,
        )

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""CRT tube simulation and film grain, as one fullscreen pass.

Every dial is a constructor keyword with an ordinary Python default, so
`rt.add(CrtFilmGrain, config={"barrel_curve": 0.0})` flattens the tube without
touching the shader.
"""

from __future__ import annotations

import struct

from streamlib import VideoFrame, processor

from ..single_pass_video_effect import SinglePassVideoEffect

# `vec2 frame_extent_in_pixels;` then elapsed seconds and the seven dials below.
CRT_PUSH_CONSTANT_FORMAT = "<10f"
CRT_PUSH_CONSTANT_SIZE = struct.calcsize(CRT_PUSH_CONSTANT_FORMAT)


@processor(description="80s CRT tube and film grain over the whole frame")
class CrtFilmGrain(SinglePassVideoEffect):
    """Barrel curve, scanlines, aberration, vignette and 24 fps grain."""

    fragment_shader_file_name = "crt_film_grain.frag"
    push_constant_size = CRT_PUSH_CONSTANT_SIZE

    def __init__(
        self,
        barrel_curve: float = 0.35,
        scanline_intensity: float = 0.5,
        chromatic_aberration: float = 0.0025,
        grain_intensity: float = 0.12,
        grain_speed: float = 1.0,
        vignette_intensity: float = 0.6,
        brightness: float = 1.05,
    ) -> None:
        self.barrel_curve = barrel_curve
        self.scanline_intensity = scanline_intensity
        self.chromatic_aberration = chromatic_aberration
        self.grain_intensity = grain_intensity
        self.grain_speed = grain_speed
        self.vignette_intensity = vignette_intensity
        self.brightness = brightness

    def push_constants_for(self, frame: VideoFrame, elapsed_seconds: float) -> bytes:
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

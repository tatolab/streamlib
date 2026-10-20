# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""CRT tube simulation and film grain, as one fullscreen pass.

Every dial is a `CrtFilmGrainConfig` field with an ordinary Python default, so
`stream_builder.add(CrtFilmGrain, config={"barrel_curve": 0.0})` flattens the
tube without touching the shader.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass

from tatolab.stream import VideoFrame, node

from ..single_pass_video_effect import SinglePassVideoEffect

# `vec2 frame_extent_in_pixels;` then elapsed seconds and the seven dials below.
CRT_PUSH_CONSTANT_FORMAT = "<10f"
CRT_PUSH_CONSTANT_SIZE = struct.calcsize(CRT_PUSH_CONSTANT_FORMAT)


@dataclass
class CrtFilmGrainConfig:
    """CrtFilmGrain's settings, as `stream_builder.add(..., config={...})` spells them."""

    barrel_curve: float = 0.35
    scanline_intensity: float = 0.5
    chromatic_aberration: float = 0.0025
    grain_intensity: float = 0.12
    grain_speed: float = 1.0
    vignette_intensity: float = 0.6
    brightness: float = 1.05


@node(description="80s CRT tube and film grain over the whole frame")
class CrtFilmGrain(SinglePassVideoEffect):
    """Barrel curve, scanlines, aberration, vignette and 24 fps grain."""

    fragment_shader_file_name = "crt_film_grain.frag"
    push_constant_size = CRT_PUSH_CONSTANT_SIZE

    def __init__(self, config: CrtFilmGrainConfig) -> None:
        self.crt_film_grain_config = config

    def push_constants_for(self, frame: VideoFrame, elapsed_seconds: float) -> bytes:
        dials = self.crt_film_grain_config
        return struct.pack(
            CRT_PUSH_CONSTANT_FORMAT,
            float(frame.width),
            float(frame.height),
            elapsed_seconds,
            dials.barrel_curve,
            dials.scanline_intensity,
            dials.chromatic_aberration,
            dials.grain_intensity,
            dials.grain_speed,
            dials.vignette_intensity,
            dials.brightness,
        )

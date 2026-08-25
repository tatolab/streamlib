# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""CRT tube simulation and film grain, as one fullscreen pass.

Every dial is config, so `rt.add(CrtFilmGrain, config={"barrel_curve": 0.0})`
turns the tube flat without touching the shader.
"""

from __future__ import annotations

import struct

from streamlib import RuntimeContextFullAccess, VideoFrame, processor

from ..single_pass_video_effect import SinglePassVideoEffect

# `vec2 frame_extent_in_pixels;` then elapsed seconds and the seven dials below.
CRT_PUSH_CONSTANT_FORMAT = "<10f"
CRT_PUSH_CONSTANT_SIZE = struct.calcsize(CRT_PUSH_CONSTANT_FORMAT)

DEFAULT_DIALS: "dict[str, float]" = {
    "barrel_curve": 0.35,
    "scanline_intensity": 0.5,
    "chromatic_aberration": 0.0025,
    "grain_intensity": 0.12,
    "grain_speed": 1.0,
    "vignette_intensity": 0.6,
    "brightness": 1.05,
}


@processor(description="80s CRT tube and film grain over the whole frame")
class CrtFilmGrain(SinglePassVideoEffect):
    """Barrel curve, scanlines, aberration, vignette and 24 fps grain."""

    fragment_shader_file_name = "crt_film_grain.frag"
    push_constant_size = CRT_PUSH_CONSTANT_SIZE

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        super().setup(ctx)
        self.dials = {
            dial: float(ctx.config.get(dial, default))
            for dial, default in DEFAULT_DIALS.items()
        }

    def push_constants_for(self, frame: VideoFrame, elapsed_seconds: float) -> bytes:
        return struct.pack(
            CRT_PUSH_CONSTANT_FORMAT,
            float(frame.width),
            float(frame.height),
            elapsed_seconds,
            self.dials["barrel_curve"],
            self.dials["scanline_intensity"],
            self.dials["chromatic_aberration"],
            self.dials["grain_intensity"],
            self.dials["grain_speed"],
            self.dials["vignette_intensity"],
            self.dials["brightness"],
        )

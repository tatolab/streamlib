# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Intermittent glitch flashes over a continuous cyberpunk grade.

The look lives entirely in `shaders/cyberpunk_glitch_fragment.slang`; what stays in
Python is when a flash fires, how long it lasts and how hard it hits. The
shader gets that as three numbers a frame.
"""

from __future__ import annotations

import random
import struct
from typing import Callable

from tatolab.stream import RuntimeContextFullAccess, RuntimeContextLimitedAccess, Texture, node

from ..single_pass_video_effect import (
    VideoFromUpstream,
    VideoToDownstream,
    each_frame_of_a_single_pass_video_effect,
)

# `float2 frame_extent_in_pixels; float elapsed_seconds; float glitch_intensity;
#  float glitch_seed; float dramatic_glitch;`
GLITCH_PUSH_CONSTANT_FORMAT = "<6f"
GLITCH_PUSH_CONSTANT_SIZE = struct.calcsize(GLITCH_PUSH_CONSTANT_FORMAT)


class GlitchFlashSchedule:
    """When a flash fires, and what kind — a plain state machine on elapsed time.

    One timer fires every 0–8 s after a 2 s cooldown; each firing is an even
    coin toss between a dramatic tear (0.3–0.8 s, near-full intensity) and a
    subtle one (0.1–0.3 s, half that).
    """

    COOLDOWN_SECONDS = 2.0
    LONGEST_WAIT_SECONDS = 8.0

    def __init__(self) -> None:
        self.intensity = 0.0
        self.is_dramatic = False
        self.seed = 0.0
        self._running = False
        self._started_at_seconds = 0.0
        self._duration_seconds = 0.0
        self._in_cooldown = False
        self._cooldown_ends_at_seconds = 0.0
        self._next_flash_at_seconds = random.uniform(0.0, self.LONGEST_WAIT_SECONDS)

    def advance_to(self, elapsed_seconds: float) -> None:
        if self._running:
            if elapsed_seconds - self._started_at_seconds > self._duration_seconds:
                self._running = False
                self.is_dramatic = False
                self.intensity = 0.0
                self._in_cooldown = True
                self._cooldown_ends_at_seconds = elapsed_seconds + self.COOLDOWN_SECONDS
            return

        if self._in_cooldown:
            if elapsed_seconds >= self._cooldown_ends_at_seconds:
                self._in_cooldown = False
                self._next_flash_at_seconds = elapsed_seconds + random.uniform(
                    0.0, self.LONGEST_WAIT_SECONDS
                )
            return

        if elapsed_seconds < self._next_flash_at_seconds:
            return

        self._running = True
        self._started_at_seconds = elapsed_seconds
        self.seed = elapsed_seconds
        self.is_dramatic = random.random() < 0.5
        if self.is_dramatic:
            self._duration_seconds = random.uniform(0.3, 0.8)
            self.intensity = random.uniform(0.8, 1.0)
        else:
            self._duration_seconds = random.uniform(0.1, 0.3)
            self.intensity = random.uniform(0.3, 0.6)


@node.meta(description="Cyberpunk colour grade with intermittent glitch flashes")
@node(execution="reactive")
def CyberpunkGlitch(
    *,
    video_from_upstream: VideoFromUpstream,
    video_to_downstream: VideoToDownstream,
    ctx: RuntimeContextFullAccess,
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """The grade is always on; the glitch is not."""
    flash_schedule = GlitchFlashSchedule()

    def push_constants_for(frame: Texture, elapsed_seconds: float) -> bytes:
        flash_schedule.advance_to(elapsed_seconds)
        return struct.pack(
            GLITCH_PUSH_CONSTANT_FORMAT,
            float(frame.width),
            float(frame.height),
            elapsed_seconds,
            flash_schedule.intensity,
            flash_schedule.seed,
            1.0 if flash_schedule.is_dramatic else 0.0,
        )

    return each_frame_of_a_single_pass_video_effect(
        ctx,
        label="CyberpunkGlitch",
        fragment_shader_file_name="cyberpunk_glitch_fragment.slang",
        push_constant_size=GLITCH_PUSH_CONSTANT_SIZE,
        push_constants_for=push_constants_for,
        video_from_upstream=video_from_upstream,
        video_to_downstream=video_to_downstream,
    )

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes for `GlslPixelEffect`, from a helper process.

numpy appears here only to check the pixels afterwards, the way a test would.
"""

import json
import os
import re
import traceback
from typing import Any, Callable, TypedDict

import numpy

from streamlib import (
    GlslPixelEffect,
    GpuContextLimitedAccess,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    log,
    processor,
)

RESULT_MARKER = "MARKER:PROBE_RESULT "

# Neither a multiple of the workgroup tile, so the edge tiles' bounds check runs.
FRAME_WIDTH = 60
FRAME_HEIGHT = 36

INVERT_WITH_STRENGTH_GLSL = """\
vec4 effect(vec4 source, ivec2 at) {
    return vec4(mix(source.rgb, 1.0 - source.rgb, dials.strength), source.a);
}
"""

IDENTITY_GLSL = """\
vec4 effect(vec4 source, ivec2 at) {
    return source;
}
"""

MIRROR_THROUGH_TEXEL_HELPER_GLSL = """\
vec4 effect(vec4 source, ivec2 at) {
    return streamlib_source_at(ivec2(streamlib_extent.x - 1 - at.x, at.y));
}
"""

CLAMPED_PAST_THE_RIGHT_EDGE_GLSL = """\
vec4 effect(vec4 source, ivec2 at) {
    return streamlib_source_at(at + ivec2(1000, 0));
}
"""

TEXEL_CENTRES_THROUGH_UV_HELPER_GLSL = """\
vec4 effect(vec4 source, ivec2 at) {
    return streamlib_source_uv((vec2(at) + 0.5) / vec2(streamlib_extent));
}
"""

# The mistake is on the body's third line.
UNDEFINED_FUNCTION_ON_LINE_THREE_GLSL = """\
vec4 effect(vec4 source, ivec2 at) {
    vec4 shifted = source;
    return no_such_function(shifted);
}
"""
UNDEFINED_FUNCTION_LINE = 3

EFFECT_GLSL_BY_NAME = {
    "invert": INVERT_WITH_STRENGTH_GLSL,
    "identity": IDENTITY_GLSL,
}

HELPER_EFFECT_GLSL_BY_NAME = {
    "mirror_through_texel_helper": MIRROR_THROUGH_TEXEL_HELPER_GLSL,
    "clamped_past_the_right_edge": CLAMPED_PAST_THE_RIGHT_EDGE_GLSL,
    "texel_centres_through_uv_helper": TEXEL_CENTRES_THROUGH_UV_HELPER_GLSL,
}


def _report(probe_body: Callable[[], "dict[str, Any]"]) -> None:
    try:
        observation = probe_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(RESULT_MARKER + json.dumps({"pid": os.getpid(), **observation}))


def _pixels_of(gpu: GpuContextLimitedAccess, surface_id: str) -> numpy.ndarray:
    with gpu.resolve_surface(surface_id) as surface:
        surface.lock(read_only=True)
        pixels = surface.as_numpy().copy()
        surface.unlock()
    return pixels


def _mismatched_pixels(observed: numpy.ndarray, expected: numpy.ndarray) -> int:
    return int((observed != expected).any(axis=2).sum())


class InvertingEffectProbeConfig(TypedDict, total=False):
    effect: str


@processor
class InvertingEffectProbe:
    """Applies an effect with a strength dial to the first frame and reports
    how many output pixels differ from `255 - source`.

    `effect="identity"` is the negative control: its output is the source,
    which the invert check must fail.
    """

    @input(delivery_profile="ordered")
    def video_from_upstream(self) -> VideoFrame: ...

    def __init__(self, config: InvertingEffectProbeConfig) -> None:
        self.effect_glsl = EFFECT_GLSL_BY_NAME[config.get("effect", "invert")]
        self.reported = False

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.effect = GlslPixelEffect.compile(
            ctx.gpu_full_access, effect_glsl=self.effect_glsl, dials={"strength": "float"}
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None or self.reported:
            return
        self.reported = True
        gpu = ctx.gpu_limited_access

        def apply_and_compare() -> "dict[str, Any]":
            output_bag = self.effect.apply_to_frame(gpu, frame, dials={"strength": 1.0})
            source = _pixels_of(gpu, frame.surface_id)
            expected = source.copy()
            expected[:, :, :3] = 255 - expected[:, :, :3]
            return {
                "output_extent": [output_bag["width"], output_bag["height"]],
                "timestamp_carried": output_bag["timestamp_ns"] == frame.timestamp_ns,
                "source_is_not_uniform": bool((source != source[0, 0]).any()),
                "mismatched_pixels": _mismatched_pixels(
                    _pixels_of(gpu, output_bag["surface_id"]), expected
                ),
            }

        _report(apply_and_compare)


@processor
class PreDeclaredHelpersProbe:
    """Applies one effect per pre-declared helper to the first frame and
    reports each one's mismatch against the same picture made with numpy."""

    @input(delivery_profile="ordered")
    def video_from_upstream(self) -> VideoFrame: ...

    def __init__(self) -> None:
        self.reported = False

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.effects = {
            name: GlslPixelEffect.compile(ctx.gpu_full_access, effect_glsl=effect_glsl)
            for name, effect_glsl in HELPER_EFFECT_GLSL_BY_NAME.items()
        }

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None or self.reported:
            return
        self.reported = True
        gpu = ctx.gpu_limited_access

        def apply_each() -> "dict[str, Any]":
            source = _pixels_of(gpu, frame.surface_id)
            expected_by_name = {
                "mirror_through_texel_helper": source[:, ::-1],
                "clamped_past_the_right_edge": numpy.repeat(
                    source[:, -1:], source.shape[1], axis=1
                ),
                "texel_centres_through_uv_helper": source,
            }
            return {
                name: _mismatched_pixels(
                    _pixels_of(gpu, effect.apply_to_frame(gpu, frame)["surface_id"]),
                    expected_by_name[name],
                )
                for name, effect in self.effects.items()
            }

        _report(apply_each)


@processor
class CompilerDiagnosticLineProbe:
    """Compiles a body with a mistake on its third line and reports the line
    numbers the compiler's diagnostic names."""

    @input(delivery_profile="ordered")
    def video_from_upstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        def compile_and_read_the_diagnostic() -> "dict[str, Any]":
            try:
                GlslPixelEffect.compile(
                    ctx.gpu_full_access, effect_glsl=UNDEFINED_FUNCTION_ON_LINE_THREE_GLSL
                )
            except Exception as refusal:  # noqa: BLE001 — the refusal is the subject
                diagnostic = str(refusal)
                return {
                    "diagnostic": diagnostic,
                    "reported_lines": sorted(
                        {int(line) for line in re.findall(r":(\d+): error", diagnostic)}
                    ),
                }
            raise AssertionError("the body compiled; it should have been refused")

        _report(compile_and_read_the_diagnostic)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        pass


@processor
class CopyRefusedFrameProbe:
    """Applies an effect to a `bgra` frame, as a camera may publish, which the
    engine copy refuses to land, and reports what the effect said."""

    @input(delivery_profile="ordered")
    def video_from_upstream(self) -> VideoFrame: ...

    def __init__(self) -> None:
        self.reported = False

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.effect = GlslPixelEffect.compile(
            ctx.gpu_full_access, effect_glsl=IDENTITY_GLSL
        )
        self.bgra_pixel_buffer = ctx.gpu_full_access.acquire_pixel_buffer(
            FRAME_WIDTH, FRAME_HEIGHT, "bgra"
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None or self.reported:
            return
        self.reported = True
        bgra_frame = VideoFrame(
            surface_id=self.bgra_pixel_buffer.surface_id,
            width=FRAME_WIDTH,
            height=FRAME_HEIGHT,
            timestamp_ns=frame.timestamp_ns,
        )

        def apply_to_the_bgra_frame() -> "dict[str, Any]":
            try:
                self.effect.apply_to_frame(ctx.gpu_limited_access, bgra_frame)
            except ValueError as refusal:
                return {"refusal": str(refusal), "bgra_surface_id": bgra_frame.surface_id}
            raise AssertionError("the frame was landed; the copy should have refused it")

        _report(apply_to_the_bgra_frame)

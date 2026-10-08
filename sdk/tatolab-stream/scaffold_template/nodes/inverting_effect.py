# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Pixels on the GPU: the effect the app wires between its source and its window.

Importable as `nodes.inverting_effect:InvertingEffect`, which is the
name the engine spawns this node's child interpreter with.
"""

from tatolab.stream import (
    GlslPixelEffect,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    node,
)

# The whole effect: the output pixel for the source pixel at `at`, with each
# channel in 0.0-1.0. Edit it and re-run `streamlib dev`.
INVERT_GLSL = """
vec4 effect(vec4 source, ivec2 at) {
    // Color channels only — inverting alpha would erase the picture.
    return vec4(1.0 - source.rgb, source.a);
}
"""


@node
class InvertingEffect:
    """Inverts every frame's colors on the GPU and passes it on."""

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> VideoFrame: ...

    @node.output()
    def video_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.inverting_pixel_effect = GlslPixelEffect.compile(
            ctx.gpu_full_access, effect_glsl=INVERT_GLSL
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None:
            return
        ctx.outputs.write(
            "video_to_downstream",
            self.inverting_pixel_effect.apply_to_frame(ctx.gpu_limited_access, frame),
        )

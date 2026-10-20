# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One effect host, any number of looks: a GLSL `effect` function taken from config.

Importable as `nodes.shader_effect:ShaderEffect`, which is the name the engine
spawns this node's child interpreter with — and the `type` an agent passes to
`add_node` to put a look into the running stream.

The host is written once. A look is the `vec4 effect(vec4 source, ivec2 at)`
function a config carries: `GlslPixelEffect` compiles it at `setup()` and runs
it over every frame on the GPU, so the pixels never leave it. `looks/` holds
three to start from.
"""

from dataclasses import dataclass
from pathlib import Path

from tatolab.stream import (
    GlslPixelEffect,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    msgpack,
    node,
)

SHIPPED_LOOKS_DIRECTORY = Path(__file__).parent / "looks"


def shipped_look_glsl(look_file_name: str) -> str:
    """The `effect` function of one look in `looks/`, e.g. `vignette.glsl`."""
    return (SHIPPED_LOOKS_DIRECTORY / look_file_name).read_text(encoding="utf-8")


@dataclass
class ShaderEffectConfig:
    """The look: a GLSL `vec4 effect(vec4 source, ivec2 at)` function."""

    effect_glsl: str


@node(description="Runs the GLSL effect function its config carries over every frame")
class ShaderEffect:
    """Frame in, the same frame through a GLSL `effect` function out."""

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> VideoFrame: ...

    @node.output()
    def video_to_downstream(self) -> VideoFrame: ...

    def __init__(self, config: ShaderEffectConfig) -> None:
        self.effect_glsl = config.effect_glsl

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        try:
            self.look_pixel_effect = GlslPixelEffect.compile(
                ctx.gpu_full_access, effect_glsl=self.effect_glsl
            )
        except (RuntimeError, ValueError) as refusal:
            raise RuntimeError(
                f"ShaderEffect could not build its look from `effect_glsl`: {refusal}"
            ) from refusal

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None:
            return
        look_frame_record = self.look_pixel_effect.apply_to_frame(
            ctx.gpu_limited_access, frame
        )
        ctx.outputs.write(
            "video_to_downstream",
            msgpack.pack(look_frame_record),
            timestamp_ns=frame.timestamp_ns,
        )

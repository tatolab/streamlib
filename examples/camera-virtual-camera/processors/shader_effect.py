# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""One effect host, any number of looks: a fragment shader taken from config.

Importable as `processors.shader_effect:ShaderEffect`, which is the name the
engine spawns this processor's child interpreter with — and the name an agent
passes to `add_processor` to put a look into the running graph.

The host is written once. A look is the GLSL a `config` carries: the engine
compiles it at `setup()`, draws it over every frame as one fullscreen pass,
and the pixels never leave the GPU. `shaders/` holds three to start from.

What a fragment shader gets: the frame as a `sampler2D` under the name
`sampled_input_binding_name` says (`upstream_frame` unless the config names
another), `screen_uv` at location 0 running 0..1 from the top left, and one
colour output. It declares no other binding and no push constants.
"""

from dataclasses import dataclass
from pathlib import Path
from typing import Any

import cupy
from streamlib import (
    ProcessorOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,  # noqa: A004 — streamlib's port decorator
    output,
    processor,
)

SHIPPED_SHADERS_DIRECTORY = Path(__file__).parent / "shaders"

FULLSCREEN_TRIANGLE_VERTEX_GLSL = (
    SHIPPED_SHADERS_DIRECTORY / "fullscreen_triangle.vert"
).read_text(encoding="utf-8")

# One format end to end: the camera publishes RGBA8 and a `VirtualCameraSink`
# samples RGBA8 on its way to the device's buffers.
LANDING_AND_RENDERED_FRAME_TEXTURE_FORMAT = "rgba8_unorm"

# The texture each incoming frame lands in and the shader samples.
SAMPLED_LANDING_TEXTURE_USAGE = ["texture_binding"]

# The texture the pass renders into and the next processor samples.
RENDERED_OUTPUT_TEXTURE_USAGE = ["render_attachment", "texture_binding"]


@dataclass
class ShaderEffectConfig:
    """The look: fragment GLSL, and the name it gives the frame it samples."""

    fragment_glsl: str
    sampled_input_binding_name: str = "upstream_frame"


class VideoFrameWithTheBagItArrivedIn:
    """A typed read's target that keeps the bag beside the frame cast from it.

    The frame is constructed while the read is offering its claim, so it holds
    the camera's pixels still for the landing copy; the bag is what gets
    forwarded, because every key on it still describes the picture.
    """

    def __init__(self, **bag: Any) -> None:
        self.bag = bag
        self.video_frame = VideoFrame(**bag)


@processor(description="Draws the fragment shader its config carries over every frame")
class ShaderEffect:
    """Frame in, the same frame through a fragment shader out."""

    @input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    @output()
    def video_to_downstream(self) -> None: ...

    def __init__(self, config: ShaderEffectConfig) -> None:
        self.fragment_glsl = config.fragment_glsl
        self.sampled_input_binding_name = config.sampled_input_binding_name

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        try:
            self.graphics_kernel = ctx.gpu_full_access.create_graphics_kernel(
                color_attachment_formats=[LANDING_AND_RENDERED_FRAME_TEXTURE_FORMAT],
                vertex_source=FULLSCREEN_TRIANGLE_VERTEX_GLSL,
                fragment_source=self.fragment_glsl,
                bindings={
                    self.sampled_input_binding_name: ("sampled_texture", ["fragment"])
                },
                label="ShaderEffect",
            )
        except RuntimeError as refusal:
            raise RuntimeError(
                f"ShaderEffect could not build its pass from `fragment_glsl` sampling "
                f"`{self.sampled_input_binding_name}` "
                f"(`sampled_input_binding_name`): {refusal}"
            ) from refusal
        # Depth 1: the draw returns with the GPU work retired, and nothing
        # outside this processor ever names a landing texture.
        self.landing_texture_ring = ProcessorOutputTextureRing(
            LANDING_AND_RENDERED_FRAME_TEXTURE_FORMAT,
            SAMPLED_LANDING_TEXTURE_USAGE,
            depth=1,
        )
        self.rendered_output_texture_ring = ProcessorOutputTextureRing(
            LANDING_AND_RENDERED_FRAME_TEXTURE_FORMAT, RENDERED_OUTPUT_TEXTURE_USAGE
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        arrival = ctx.inputs.read(
            "video_from_upstream", into=VideoFrameWithTheBagItArrivedIn
        )
        if arrival is None:
            return
        frame = arrival.video_frame

        # A camera publishes buffer-backed frames and a draw binds
        # texture-backed surfaces only, so each frame is copied device-to-device
        # into a texture this processor owns.
        landing_texture = self.landing_texture_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, frame.width, frame.height
        )
        with landing_texture.as_device_tensor() as writable_landing_texture:
            cupy.from_dlpack(writable_landing_texture)[...] = cupy.from_dlpack(frame)

        rendered_output_texture = (
            self.rendered_output_texture_ring.next_texture_for_this_frame(
                ctx.gpu_limited_access, frame.width, frame.height
            )
        )
        self.graphics_kernel.draw(
            bindings={self.sampled_input_binding_name: landing_texture},
            color_targets=[rendered_output_texture],
            extent=(frame.width, frame.height),
            vertex_count=3,
        )

        # The upstream bag forwarded whole, with only the surface swapped: the
        # capture stamp and the colour metadata still describe this picture,
        # and a `VirtualCameraSink` sets the device's colorimetry from them.
        rendered_bag = dict(arrival.bag)
        rendered_bag["surface_id"] = rendered_output_texture.surface_id
        # A per-frame layout override describes the surface it was published
        # with, and this is a different surface.
        rendered_bag.pop("texture_layout", None)
        ctx.outputs.write("video_to_downstream", rendered_bag)

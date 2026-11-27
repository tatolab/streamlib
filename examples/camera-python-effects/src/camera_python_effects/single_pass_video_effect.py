# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""One fullscreen graphics pass over one incoming frame.

The shape both full-frame effects in this stream share: build the kernel once
in the node's body where the capability is Full, then per frame take a colour
target from the output ring, draw the incoming frame into it, and publish the
target's texture. An effect supplies its fragment shader and packs its own push
constants; everything else — the pin types, kernel construction, the ring, the
draw, the write — is here so each effect module is only its effect.
"""

from __future__ import annotations

from typing import Annotated, Callable

from tatolab.stream import (
    Description,
    InputPin,
    NodeOutputTextureRing,
    OutputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    TextureToWrite,
)

from .elapsed_seconds_since_first_process import ElapsedSecondsSinceFirstProcess
from .gpu_surface_conventions import (
    COLOR_TARGET_TEXTURE_USAGE,
    TEXTURE_FORMAT,
    read_shader_source,
)

__all__ = [
    "SHARED_VERTEX_SHADER_FILE_NAME",
    "VideoFromUpstream",
    "VideoToDownstream",
    "each_frame_of_a_single_pass_video_effect",
]

# The input port and the name the fragment shader samples the frame at.
VIDEO_FROM_UPSTREAM = "video_from_upstream"

SHARED_VERTEX_SHADER_FILE_NAME = "fullscreen_triangle_vertex.slang"

VideoFromUpstream = InputPin[
    Annotated[Texture, StreamDelivery("newest"), Description("The frame to draw the effect over")]
]
"""The pin type of an effect's `video_from_upstream` input."""

VideoToDownstream = OutputPin[
    Annotated[
        Texture,
        TexturePixelFormats(TEXTURE_FORMAT),
        Description("The same frame through the effect"),
    ]
]
"""The pin type of an effect's `video_to_downstream` output."""


def each_frame_of_a_single_pass_video_effect(
    ctx: RuntimeContextFullAccess,
    *,
    label: str,
    fragment_shader_file_name: str,
    push_constant_size: int,
    push_constants_for: Callable[[Texture, float], bytes],
    video_from_upstream: VideoFromUpstream,
    video_to_downstream: VideoToDownstream,
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Build the effect's kernel from `fragment_shader_file_name` and return the function that
    draws each frame of `video_from_upstream` through it into `video_to_downstream`, its push
    constants packed by `push_constants_for(frame, elapsed_seconds)`."""
    elapsed_seconds_since_first_process = ElapsedSecondsSinceFirstProcess()
    output_ring = NodeOutputTextureRing(TEXTURE_FORMAT, COLOR_TARGET_TEXTURE_USAGE)
    graphics_kernel = ctx.gpu_full_access.create_graphics_kernel(
        color_attachment_formats=[TEXTURE_FORMAT],
        vertex_source=read_shader_source(SHARED_VERTEX_SHADER_FILE_NAME),
        fragment_source=read_shader_source(fragment_shader_file_name),
        bindings={VIDEO_FROM_UPSTREAM: ("sampled_texture", ["fragment"])},
        push_constant_size=push_constant_size,
        label=label,
    )

    def each_frame(frame_ctx: RuntimeContextLimitedAccess) -> None:
        frame = video_from_upstream.read()
        if frame is None:
            return
        elapsed_seconds = elapsed_seconds_since_first_process.seconds_at(frame_ctx.time)

        color_target = output_ring.next_texture_for_this_frame(
            frame_ctx.gpu_limited_access, frame.width, frame.height
        )
        graphics_kernel.draw(
            bindings={VIDEO_FROM_UPSTREAM: frame.surface_id},
            color_targets=[color_target],
            extent=(frame.width, frame.height),
            vertex_count=3,
            push_constants=push_constants_for(frame, elapsed_seconds),
        )
        video_to_downstream.write(
            TextureToWrite.of_texture(color_target, frame.colour),
            timestamp_ns=frame.timestamp_ns,
        )

    return each_frame

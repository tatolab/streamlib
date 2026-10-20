# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One fullscreen graphics pass over one incoming frame.

The shape both full-frame effects in this stream share: build the kernel once
in `setup()` where the capability is Full, then per frame take a colour target
from the output ring, draw the incoming frame into it, and publish the target's
surface id. A subclass supplies its fragment shader and packs its own push
constants; everything else — ports, kernel construction, the ring, the draw,
the record — is here so each effect module is only its effect.
"""

from __future__ import annotations

from abc import ABC, abstractmethod

from tatolab.stream import (
    NodeOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    node,
)

from .elapsed_seconds_since_first_process import ElapsedSecondsSinceFirstProcess
from .gpu_surface_conventions import (
    COLOR_TARGET_TEXTURE_USAGE,
    TEXTURE_FORMAT,
    read_shader_source,
)

__all__ = ["SinglePassVideoEffect"]

# The input port and the name the fragment shader samples the frame at.
VIDEO_FROM_UPSTREAM = "video_from_upstream"

SHARED_VERTEX_SHADER_FILE_NAME = "fullscreen_triangle.vert"

_CLASS_ATTRIBUTES_EVERY_EFFECT_DECLARES = {
    "fragment_shader_file_name": str,
    "push_constant_size": int,
}


class SinglePassVideoEffect(ABC):
    """Base for an effect that is one fragment shader over one input frame."""

    fragment_shader_file_name: str
    push_constant_size: int

    def __init_subclass__(cls, **class_keywords: object) -> None:
        super().__init_subclass__(**class_keywords)
        for (
            attribute_name,
            attribute_type,
        ) in _CLASS_ATTRIBUTES_EVERY_EFFECT_DECLARES.items():
            if not isinstance(getattr(cls, attribute_name, None), attribute_type):
                raise TypeError(
                    f"{cls.__name__} subclasses SinglePassVideoEffect without the "
                    f"class attribute `{attribute_name}: {attribute_type.__name__}`"
                )
        if getattr(cls.push_constants_for, "__isabstractmethod__", False):
            raise TypeError(
                f"{cls.__name__} subclasses SinglePassVideoEffect without defining "
                f"`push_constants_for(self, frame, elapsed_seconds) -> bytes`"
            )

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> VideoFrame: ...

    @node.output()
    def video_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.elapsed_seconds_since_first_process = ElapsedSecondsSinceFirstProcess()
        self.output_ring = NodeOutputTextureRing(
            TEXTURE_FORMAT, COLOR_TARGET_TEXTURE_USAGE
        )
        self.graphics_kernel = ctx.gpu_full_access.create_graphics_kernel(
            color_attachment_formats=[TEXTURE_FORMAT],
            vertex_source=read_shader_source(SHARED_VERTEX_SHADER_FILE_NAME),
            fragment_source=read_shader_source(self.fragment_shader_file_name),
            bindings={VIDEO_FROM_UPSTREAM: ("sampled_texture", ["fragment"])},
            push_constant_size=self.push_constant_size,
            label=type(self).__name__,
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(VIDEO_FROM_UPSTREAM, into=VideoFrame)
        if frame is None:
            return
        elapsed_seconds = self.elapsed_seconds_since_first_process.seconds_at(ctx.time)

        color_target = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, frame.width, frame.height
        )
        self.graphics_kernel.draw(
            bindings={VIDEO_FROM_UPSTREAM: frame.surface_id},
            color_targets=[color_target],
            extent=(frame.width, frame.height),
            vertex_count=3,
            push_constants=self.push_constants_for(frame, elapsed_seconds),
        )
        ctx.outputs.write(
            "video_to_downstream",
            VideoFrame(
                surface_id=color_target.surface_id,
                width=frame.width,
                height=frame.height,
                timestamp_ns=frame.timestamp_ns,
                color_info=frame.color_info,
            ).to_bytes(),
            timestamp_ns=frame.timestamp_ns,
        )

    @abstractmethod
    def push_constants_for(self, frame: VideoFrame, elapsed_seconds: float) -> bytes:
        """This effect's push-constant block for one frame, packed as its shader declares it."""

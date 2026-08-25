# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One fullscreen graphics pass over one incoming frame.

The shape both full-frame effects in this app share: build the kernel once in
`setup()` where the capability is Full, then per frame acquire a colour target,
draw the incoming frame into it, and publish the target's surface id. A
subclass supplies its fragment shader and packs its own push constants;
everything else — ports, kernel construction, the acquire, the draw, the bag —
is here so each effect module is only its effect.
"""

from __future__ import annotations

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    output,
)

from .gpu_surface_conventions import (
    COLOR_TARGET_TEXTURE_USAGE,
    TEXTURE_FORMAT,
    read_shader_source,
    video_frame_bag_naming,
)

__all__ = ["SinglePassVideoEffect"]

NANOSECONDS_PER_SECOND = 1_000_000_000

# The port the frame arrives on and the name the fragment shader gives the
# texture it samples. Two contracts, deliberately given one word: a read
# resolves against the port and a draw against the shader's own name, and an
# effect reads straighter when the frame is called the same thing at both ends.
VIDEO_FROM_UPSTREAM = "video_from_upstream"

SHARED_VERTEX_SHADER_FILE_NAME = "fullscreen_triangle.vert"


class SinglePassVideoEffect:
    """Base for an effect that is one fragment shader over one input frame."""

    # What a subclass fills in.
    fragment_shader_file_name: str
    push_constant_size: int

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.first_process_at_ns: int | None = None
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

        # Anchored on the first frame rather than on setup: the helper's
        # interpreter starts well before traffic reaches it, and an effect that
        # animates from `t = 0` should start when the picture does.
        if self.first_process_at_ns is None:
            self.first_process_at_ns = ctx.time
        elapsed_seconds = (ctx.time - self.first_process_at_ns) / NANOSECONDS_PER_SECOND

        color_target = ctx.gpu_limited_access.acquire_texture(
            frame.width, frame.height, TEXTURE_FORMAT, COLOR_TARGET_TEXTURE_USAGE
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
            video_frame_bag_naming(
                color_target.surface_id, frame.width, frame.height, frame.timestamp_ns
            ),
        )

    def push_constants_for(self, frame: VideoFrame, elapsed_seconds: float) -> bytes:
        raise NotImplementedError

    @input(delivery_profile="latest")
    def video_from_upstream(self) -> VideoFrame: ...

    @output()
    def video_to_downstream(self) -> VideoFrame: ...

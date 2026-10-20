# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Blends the graded video, the skia overlay and the avatar stage into one frame.

The fan-in of the graph, and the one node with more than one input. It
paces on the video: the overlay and the skeleton arrive on their own cadences
and each is held until something newer turns up, so an avatar rendered at a
third of the camera's rate shows its last frame rather than a gap.
"""

from __future__ import annotations

import struct

from tatolab.stream import (
    NodeOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    node,
)

from ..elapsed_seconds_since_first_process import ElapsedSecondsSinceFirstProcess
from ..gpu_surface_conventions import (
    COLOR_TARGET_TEXTURE_USAGE,
    TEXTURE_FORMAT,
    read_shader_source,
)
from ..neon_overlay_canvas import ease_out_back
from ..single_pass_video_effect import SHARED_VERTEX_SHADER_FILE_NAME

# `vec2 frame_extent_in_pixels; uint present_layer_mask;
#  float pip_slide_progress; float elapsed_seconds;`
COMPOSITE_PUSH_CONSTANT_FORMAT = "<2fI2f"
COMPOSITE_PUSH_CONSTANT_SIZE = struct.calcsize(COMPOSITE_PUSH_CONSTANT_FORMAT)

VIDEO_LAYER_BIT = 1
OVERLAY_LAYER_BIT = 2
POSE_LAYER_BIT = 4

# One word per layer, serving both its input port and the binding the fragment
# shader samples it at.
VIDEO_FROM_UPSTREAM = "video_from_upstream"
OVERLAY_FROM_NEON_SOURCE = "overlay_from_neon_source"
AVATAR_FROM_POSE_SCENE = "avatar_from_pose_scene"

# How long the picture-in-picture takes to slide in, and how long it waits
# before it starts — the beat that makes it read as a cut-in rather than
# something that was always there.
PIP_SLIDE_IN_SECONDS = 0.8
PIP_HOLD_OFF_SECONDS = 3.0


def pip_slide_progress_at(elapsed_seconds: float) -> float:
    """0.0 fully off-screen, 1.0 docked — swinging past the dock on the way.

    The overshoot is the point: the frame slams in, blows a little past its
    mark and settles, the way the game's HUD panels arrive. The shader takes
    a progress above 1.0 as a slide past the docked position.
    """
    slid = (elapsed_seconds - PIP_HOLD_OFF_SECONDS) / PIP_SLIDE_IN_SECONDS
    clamped = min(max(slid, 0.0), 1.0)
    return ease_out_back(clamped, overshoot=1.9)


@node(description="Three-layer compositor with a sliding picture-in-picture")
class BreakingNewsCompositor:
    """Video underneath, overlay over it, the android's stage inside the PiP."""

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> VideoFrame: ...

    @node.input(delivery_profile="newest")
    def overlay_from_neon_source(self) -> VideoFrame: ...

    @node.input(delivery_profile="newest")
    def avatar_from_pose_scene(self) -> VideoFrame: ...

    @node.output()
    def video_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.graphics_kernel = ctx.gpu_full_access.create_graphics_kernel(
            color_attachment_formats=[TEXTURE_FORMAT],
            vertex_source=read_shader_source(SHARED_VERTEX_SHADER_FILE_NAME),
            fragment_source=read_shader_source("breaking_news_composite.frag"),
            bindings={
                VIDEO_FROM_UPSTREAM: ("sampled_texture", ["fragment"]),
                OVERLAY_FROM_NEON_SOURCE: ("sampled_texture", ["fragment"]),
                AVATAR_FROM_POSE_SCENE: ("sampled_texture", ["fragment"]),
            },
            push_constant_size=COMPOSITE_PUSH_CONSTANT_SIZE,
            label="BreakingNewsCompositor",
        )
        # Held across ticks: the two side layers pace themselves, and a frame
        # that has not been replaced is still the newest one there is.
        self.latest_overlay: VideoFrame | None = None
        self.latest_avatar_scene: VideoFrame | None = None
        self.elapsed_seconds_since_first_process = ElapsedSecondsSinceFirstProcess()
        self.output_ring = NodeOutputTextureRing(
            TEXTURE_FORMAT, COLOR_TARGET_TEXTURE_USAGE
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        video = ctx.inputs.read(VIDEO_FROM_UPSTREAM, into=VideoFrame)
        if video is None:
            return
        elapsed_seconds = self.elapsed_seconds_since_first_process.seconds_at(ctx.time)

        newer_overlay = ctx.inputs.read(OVERLAY_FROM_NEON_SOURCE, into=VideoFrame)
        if newer_overlay is not None:
            self.latest_overlay = newer_overlay
        newer_avatar_scene = ctx.inputs.read(AVATAR_FROM_POSE_SCENE, into=VideoFrame)
        if newer_avatar_scene is not None:
            self.latest_avatar_scene = newer_avatar_scene

        # Every declared binding is supplied on every draw — the kernel holds
        # no binding state — so a layer that has not arrived binds the video
        # and is masked out of the blend instead.
        present_layer_mask = VIDEO_LAYER_BIT
        overlay_surface_id = video.surface_id
        if self.latest_overlay is not None:
            present_layer_mask |= OVERLAY_LAYER_BIT
            overlay_surface_id = self.latest_overlay.surface_id
        avatar_surface_id = video.surface_id
        if self.latest_avatar_scene is not None:
            present_layer_mask |= POSE_LAYER_BIT
            avatar_surface_id = self.latest_avatar_scene.surface_id

        composited = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, video.width, video.height
        )
        self.graphics_kernel.draw(
            bindings={
                VIDEO_FROM_UPSTREAM: video.surface_id,
                OVERLAY_FROM_NEON_SOURCE: overlay_surface_id,
                AVATAR_FROM_POSE_SCENE: avatar_surface_id,
            },
            color_targets=[composited],
            extent=(video.width, video.height),
            vertex_count=3,
            push_constants=struct.pack(
                COMPOSITE_PUSH_CONSTANT_FORMAT,
                float(video.width),
                float(video.height),
                present_layer_mask,
                pip_slide_progress_at(elapsed_seconds),
                elapsed_seconds,
            ),
        )
        ctx.outputs.write(
            "video_to_downstream",
            VideoFrame(
                surface_id=composited.surface_id,
                width=video.width,
                height=video.height,
                timestamp_ns=video.timestamp_ns,
                color_info=video.color_info,
            ).to_bytes(),
            timestamp_ns=video.timestamp_ns,
        )

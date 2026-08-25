# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Drives the android from the camera: your dancing, its dancing.

Three third-party worlds meet in this one helper process, none of them the
engine's: cupy reads the camera frame as a GPU tensor and decimates it,
MediaPipe lifts a 3D pose out of the pixels on the CPU, and ModernGL renders
the posed android on its own stage. The engine sees a frame in and a frame
out — everything between is ordinary Python packages doing what they do.

When nobody is in frame the smoother glides the android into its idle sway
instead of freezing it, and glides it back onto you when you return.
"""

from __future__ import annotations

import cupy

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    ProcessorOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    log,
    output,
    processor,
)

from ..avatar_rig import (
    SmoothedPose,
    idle_joints,
    resolve_pose_with_fallbacks,
    solve_segment_placements,
)
from ..avatar_scene import AvatarSceneRenderer
from ..gpu_surface_conventions import (
    SAMPLED_ONLY_TEXTURE_USAGE,
    video_frame_bag_naming,
)
from ..pose_tracking import PoseTracker
from ..single_pass_video_effect import NANOSECONDS_PER_SECOND

# Every third pixel of a 1080p frame — 640x360, plenty for a detector that
# resizes to 256 internally, and small enough that the device-to-host hop
# costs a millisecond.
CAMERA_DECIMATION_STRIDE = 3


@processor(description="3D android on a neon stage, dancing your pose")
class CyberpunkAvatar:
    """Camera frame in, rendered avatar stage out."""

    def __init__(
        self,
        scene_width: int = 960,
        scene_height: int = 675,
        detection_confidence: float = 0.5,
        pose_model_path: "str | None" = None,
    ) -> None:
        self.scene_width = scene_width
        self.scene_height = scene_height
        self.detection_confidence = detection_confidence
        self.pose_model_path = pose_model_path

    @input(delivery_profile="latest")
    def video_from_camera(self) -> VideoFrame: ...

    @output()
    def scene_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # The one setup step that can touch the network (the model fetch, on
        # a cold cache). An idle android beats a dead processor, so a failure
        # here leaves the tracker out and the stage swaying.
        try:
            self.pose_tracker = PoseTracker(
                self.detection_confidence, self.pose_model_path
            )
        except Exception as tracker_failure:  # noqa: BLE001 — degrade, see above
            self.pose_tracker = None
            log.warn(
                "the pose tracker could not start, so the android holds its "
                "idle sway", failure=str(tracker_failure),
            )
        self.scene_renderer = AvatarSceneRenderer(self.scene_width, self.scene_height)
        self.smoothed_pose = SmoothedPose()
        self.output_ring = ProcessorOutputTextureRing(
            "rgba8_unorm", SAMPLED_ONLY_TEXTURE_USAGE
        )
        self.first_process_at_ns: "int | None" = None
        self.previous_elapsed_seconds = 0.0
        self.a_tracker_failure_has_been_reported = False

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_camera", into=VideoFrame)
        if frame is None:
            return
        if self.first_process_at_ns is None:
            self.first_process_at_ns = ctx.time
        elapsed_seconds = (ctx.time - self.first_process_at_ns) / NANOSECONDS_PER_SECOND
        dt_seconds = max(elapsed_seconds - self.previous_elapsed_seconds, 1e-3)
        self.previous_elapsed_seconds = elapsed_seconds

        sample = self.detect_joints_in(frame)
        # Unseen joints stand in a neutral pose anchored to the live torso —
        # a desk camera drives the upper body without buckling the legs — and
        # an empty stage targets the idle sway; the smoother makes every
        # hand-off a glide.
        resolved = (
            resolve_pose_with_fallbacks(sample.joints, sample.visibility)
            if sample is not None
            else None
        )
        target_joints = resolved if resolved is not None else idle_joints(elapsed_seconds)
        settled = self.smoothed_pose.settle(target_joints, dt_seconds)

        scene_pixels = self.scene_renderer.render(
            solve_segment_placements(settled), elapsed_seconds
        )

        scene_texture = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, self.scene_width, self.scene_height
        )
        scene_texture.lock(read_only=False)
        try:
            scene_texture.as_numpy()[...] = scene_pixels
        finally:
            scene_texture.unlock()

        ctx.outputs.write(
            "scene_to_downstream",
            video_frame_bag_naming(
                scene_texture.surface_id,
                self.scene_width,
                self.scene_height,
                frame.timestamp_ns,
            ),
        )

    def detect_joints_in(self, frame: VideoFrame):
        """The tracker's answer, with a failure degrading to the idle stage."""
        if self.pose_tracker is None:
            return None
        try:
            camera_pixels = cupy.from_dlpack(frame)
            decimated_rgb = cupy.asnumpy(
                camera_pixels[::CAMERA_DECIMATION_STRIDE, ::CAMERA_DECIMATION_STRIDE, :3]
            )
            return self.pose_tracker.world_joints(
                decimated_rgb, frame.timestamp_ns // 1_000_000
            )
        except Exception as failure:  # noqa: BLE001 — the stage must not go down
            if not self.a_tracker_failure_has_been_reported:
                self.a_tracker_failure_has_been_reported = True
                log.warn(
                    "pose tracking failed, so the android holds its idle sway; "
                    "the rest of the pipeline is unaffected. Not reported again "
                    "in this process.",
                    failure=str(failure),
                )
            return None

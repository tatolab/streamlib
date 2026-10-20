# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Poses a 3D android from the camera's frames and renders it on its own stage.

Reads `video_from_camera`, decimates the frame's host side with numpy, lifts a
3D pose out of it with MediaPipe and renders the posed android with ModernGL.
Publishes the rendered stage on `scene_to_downstream` and the settled pose on
`pose_to_downstream`, both stamped with the camera frame's capture time. With
nobody in frame the android glides into an idle sway.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy
from tatolab.stream import (
    NodeOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    log,
    msgpack,
    node,
)

from ..avatar_rig import (
    PoseMemory,
    SmoothedPose,
    idle_joints,
    resolve_pose_with_fallbacks,
    solve_segment_placements,
)
from ..avatar_scene import AvatarSceneRenderer
from ..elapsed_seconds_since_first_process import (
    NANOSECONDS_PER_MILLISECOND,
    ElapsedSecondsSinceFirstProcess,
)
from ..gpu_surface_conventions import (
    SAMPLED_ONLY_TEXTURE_USAGE,
    TEXTURE_FORMAT,
    copy_host_pixels_into_ring_texture,
)
from ..pose_tracking import PoseSample, PoseTracker
from ..solved_pose_record import PoseProvenance, SolvedPoseRecord

# Every second pixel of a 1080p frame — 960x540. The detector resizes
# internally, but a desk-framed torso at 640 wide was marginal for it; the
# device-to-host hop at this size still costs under two milliseconds.
CAMERA_DECIMATION_STRIDE = 2

# One line of tracking accounting roughly every ten seconds at camera rate.
TRACKING_ACCOUNTING_EVERY_FRAMES = 300


@dataclass
class CyberpunkAvatarConfig:
    """CyberpunkAvatar's settings, as `stream_builder.add(..., config={...})` spells them."""

    scene_width: int = 960
    scene_height: int = 675
    detection_confidence: float = 0.35
    pose_model_path: str | None = None


@node(description="3D android on a neon stage, dancing your pose")
class CyberpunkAvatar:
    """Camera frame in, rendered avatar stage out."""

    def __init__(self, config: CyberpunkAvatarConfig) -> None:
        self.scene_width = config.scene_width
        self.scene_height = config.scene_height
        self.detection_confidence = config.detection_confidence
        self.pose_model_path = config.pose_model_path

    @node.input(delivery_profile="newest")
    def video_from_camera(self) -> VideoFrame: ...

    @node.output()
    def scene_to_downstream(self) -> VideoFrame: ...

    @node.output(
        description="The resolved pose as data: joints, visibility, provenance"
    )
    def pose_to_downstream(self) -> SolvedPoseRecord: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # The one setup step that can touch the network (the model fetch, on
        # a cold cache). An idle android beats a dead node, so a failure
        # here leaves the tracker out and the stage swaying.
        self.pose_tracker: PoseTracker | None
        try:
            self.pose_tracker = PoseTracker(
                self.detection_confidence, self.pose_model_path
            )
        except Exception as tracker_failure:  # noqa: BLE001 — degrade, see above
            self.pose_tracker = None
            log.warn(
                "the pose tracker could not start, so the android holds its idle sway",
                failure=str(tracker_failure),
            )
        self.scene_renderer = AvatarSceneRenderer(self.scene_width, self.scene_height)
        self.smoothed_pose = SmoothedPose()
        self.pose_memory = PoseMemory(hold_seconds=1.5)
        self.frames_detected = 0
        self.frames_held = 0
        self.frames_idled = 0
        self.output_ring = NodeOutputTextureRing(
            TEXTURE_FORMAT, SAMPLED_ONLY_TEXTURE_USAGE
        )
        self.elapsed_seconds_since_first_process = ElapsedSecondsSinceFirstProcess()
        self.previous_elapsed_seconds = 0.0
        self.a_tracker_failure_has_been_reported = False

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_camera", into=VideoFrame)
        if frame is None:
            return
        elapsed_seconds = self.elapsed_seconds_since_first_process.seconds_at(ctx.time)
        dt_seconds = max(elapsed_seconds - self.previous_elapsed_seconds, 1e-3)
        self.previous_elapsed_seconds = elapsed_seconds

        sample = self.detect_joints_in(frame)
        # Unseen joints stand in a neutral pose anchored to the live torso —
        # a desk camera drives the upper body without buckling the legs. A
        # dropout holds the last good pose for a grace window rather than
        # lurching to idle; only a sustained loss idles the stage, and the
        # smoother makes every hand-off a glide.
        resolved = (
            resolve_pose_with_fallbacks(sample.joints, sample.visibility)
            if sample is not None
            else None
        )
        remembered = self.pose_memory.target_for(resolved, elapsed_seconds)
        target_joints = (
            remembered if remembered is not None else idle_joints(elapsed_seconds)
        )
        if resolved is not None:
            self.frames_detected += 1
            pose_provenance: PoseProvenance = "detected"
        elif remembered is not None:
            self.frames_held += 1
            pose_provenance = "held"
        else:
            self.frames_idled += 1
            pose_provenance = "idle"

        total_frames = self.frames_detected + self.frames_held + self.frames_idled
        if total_frames % TRACKING_ACCOUNTING_EVERY_FRAMES == 0:
            log.info(
                "tracking accounting",
                detected=self.frames_detected,
                held_through_dropouts=self.frames_held,
                idled=self.frames_idled,
            )
        settled = self.smoothed_pose.settle(target_joints, dt_seconds)

        # The pose as a record, keyed to the exact camera frame it was lifted
        # from — what joins the streams downstream. The joints published are
        # the settled ones actually driving the figure.
        solved_pose_record: SolvedPoseRecord = {
            "timestamp_ns": frame.timestamp_ns,
            "provenance": pose_provenance,
            "joints": {
                name: [float(axis) for axis in joint] for name, joint in settled.items()
            },
            "visibility": (
                {name: float(seen) for name, seen in sample.visibility.items()}
                if sample is not None
                else {}
            ),
        }
        ctx.outputs.write(
            "pose_to_downstream",
            msgpack.pack(solved_pose_record),
            timestamp_ns=frame.timestamp_ns,
        )

        scene_pixels = self.scene_renderer.render(
            solve_segment_placements(settled), elapsed_seconds
        )

        scene_texture = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, self.scene_width, self.scene_height
        )
        copy_host_pixels_into_ring_texture(scene_texture, scene_pixels)

        ctx.outputs.write(
            "scene_to_downstream",
            VideoFrame(
                surface_id=scene_texture.surface_id,
                width=self.scene_width,
                height=self.scene_height,
                timestamp_ns=frame.timestamp_ns,
            ).to_bytes(),
            timestamp_ns=frame.timestamp_ns,
        )

    def detect_joints_in(self, frame: VideoFrame) -> PoseSample | None:
        """The tracker's answer, with a failure degrading to the idle stage."""
        if self.pose_tracker is None:
            return None
        try:
            decimated_rgb = numpy.from_dlpack(frame, device="cpu")[
                ::CAMERA_DECIMATION_STRIDE, ::CAMERA_DECIMATION_STRIDE, :3
            ]
            return self.pose_tracker.world_joints(
                decimated_rgb, frame.timestamp_ns // NANOSECONDS_PER_MILLISECOND
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

# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Poses a 3D android from the camera's frames and renders it on its own stage.

Reads `video_from_camera`, decimates the frame's host side with numpy, lifts a
3D pose out of it with MediaPipe and renders the posed android with ModernGL.
Publishes the rendered stage on `scene_to_downstream` and the settled pose on
`pose_to_downstream`, both stamped with the camera frame's capture time. With
nobody in frame the android glides into an idle sway.
"""

from __future__ import annotations

from typing import Annotated, Callable

import numpy
from tatolab.stream import (
    Description,
    Input,
    InputPin,
    NodeOutputTextureRing,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    TextureToWrite,
    computed,
    log,
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
    HOST_DRAWN_FRAME_COLOUR,
    SAMPLED_ONLY_TEXTURE_USAGE,
    TEXTURE_FORMAT,
    copy_host_pixels_into_ring_texture,
)
from ..pose_tracking import PoseSample, PoseTracker
from ..solved_pose_record import PoseProvenance, SolvedPose, SolvedPoseRecord

# Every second pixel of a 1080p frame — 960x540. The detector resizes
# internally, but a desk-framed torso at 640 wide was marginal for it; the
# device-to-host hop at this size still costs under two milliseconds.
CAMERA_DECIMATION_STRIDE = 2

# One line of tracking accounting roughly every ten seconds at camera rate.
TRACKING_ACCOUNTING_EVERY_FRAMES = 300


def pose_tracker_or_none(
    detection_confidence: float, pose_model_path: str | None
) -> PoseTracker | None:
    """A pose tracker, or `None` when it could not start.

    The one step that can touch the network (the model fetch, on a cold cache).
    An idle android beats a dead node, so a failure here leaves the tracker out
    and the stage swaying.
    """
    try:
        return PoseTracker(detection_confidence, pose_model_path)
    except Exception as tracker_failure:  # noqa: BLE001 — degrade, see above
        log.warn(
            "the pose tracker could not start, so the android holds its idle sway",
            failure=str(tracker_failure),
        )
        return None


@node.meta(description="3D android on a neon stage, dancing your pose")
@node(execution="reactive")
def CyberpunkAvatar(
    *,
    scene_width: Input[
        Annotated[int, NumberRange(minimum=1), Description("The stage's width in pixels.")]
    ] = Input(960),
    scene_height: Input[
        Annotated[int, NumberRange(minimum=1), Description("The stage's height in pixels.")]
    ] = Input(675),
    detection_confidence: Input[
        Annotated[
            float,
            NumberRange(minimum=0.0, maximum=1.0),
            Description("How sure the pose detector must be before it reports a person."),
        ]
    ] = Input(0.35),
    pose_model_path: Input[
        Annotated[
            str | None,
            Description("A MediaPipe pose model to load; the default model, fetched once, when unset."),
        ]
    ] = Input(None),
    video_from_camera: InputPin[
        Annotated[Texture, StreamDelivery("newest"), Description("Frames from the camera")]
    ],
    scene_to_downstream: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(TEXTURE_FORMAT),
            Description("The android's rendered stage"),
        ]
    ],
    pose_to_downstream: OutputPin[
        Annotated[
            SolvedPose,
            Description("The resolved pose as data: joints, visibility, provenance"),
        ]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Camera frame in, rendered avatar stage out.

    The tracker is built from the detector's inputs and the stage from its size, each again
    when an input it read changes, while frames go on with the ones already built.
    """
    pose_tracker = computed(
        lambda: pose_tracker_or_none(detection_confidence.get(), pose_model_path.get())
    )
    scene_renderer = computed(
        lambda: AvatarSceneRenderer(scene_width.get(), scene_height.get())
    )
    smoothed_pose = SmoothedPose()
    pose_memory = PoseMemory(hold_seconds=1.5)
    frames_detected = 0
    frames_held = 0
    frames_idled = 0
    output_ring = NodeOutputTextureRing(TEXTURE_FORMAT, SAMPLED_ONLY_TEXTURE_USAGE)
    elapsed_seconds_since_first_process = ElapsedSecondsSinceFirstProcess()
    previous_elapsed_seconds = 0.0
    a_tracker_failure_has_been_reported = False

    def detect_joints_in(frame: Texture) -> PoseSample | None:
        """The tracker's answer, with a failure degrading to the idle stage."""
        nonlocal a_tracker_failure_has_been_reported
        tracker = pose_tracker.get()
        if tracker is None:
            return None
        try:
            decimated_rgb = numpy.from_dlpack(frame, device="cpu")[
                ::CAMERA_DECIMATION_STRIDE, ::CAMERA_DECIMATION_STRIDE, :3
            ]
            return tracker.world_joints(
                decimated_rgb, frame.timestamp_ns // NANOSECONDS_PER_MILLISECOND
            )
        except Exception as failure:  # noqa: BLE001 — the stage must not go down
            if not a_tracker_failure_has_been_reported:
                a_tracker_failure_has_been_reported = True
                log.warn(
                    "pose tracking failed, so the android holds its idle sway; "
                    "the rest of the pipeline is unaffected. Not reported again "
                    "in this process.",
                    failure=str(failure),
                )
            return None

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal previous_elapsed_seconds, frames_detected, frames_held, frames_idled
        frame = video_from_camera.read()
        if frame is None:
            return
        elapsed_seconds = elapsed_seconds_since_first_process.seconds_at(ctx.time)
        dt_seconds = max(elapsed_seconds - previous_elapsed_seconds, 1e-3)
        previous_elapsed_seconds = elapsed_seconds

        sample = detect_joints_in(frame)
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
        remembered = pose_memory.target_for(resolved, elapsed_seconds)
        target_joints = (
            remembered if remembered is not None else idle_joints(elapsed_seconds)
        )
        if resolved is not None:
            frames_detected += 1
            pose_provenance: PoseProvenance = "detected"
        elif remembered is not None:
            frames_held += 1
            pose_provenance = "held"
        else:
            frames_idled += 1
            pose_provenance = "idle"

        total_frames = frames_detected + frames_held + frames_idled
        if total_frames % TRACKING_ACCOUNTING_EVERY_FRAMES == 0:
            log.info(
                "tracking accounting",
                detected=frames_detected,
                held_through_dropouts=frames_held,
                idled=frames_idled,
            )
        settled = smoothed_pose.settle(target_joints, dt_seconds)

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
        pose_to_downstream.write(
            SolvedPose(solved_pose_record), timestamp_ns=frame.timestamp_ns
        )

        stage = scene_renderer.get()
        scene_pixels = stage.render(
            solve_segment_placements(settled), elapsed_seconds
        )

        scene_texture = output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, stage.width, stage.height
        )
        copy_host_pixels_into_ring_texture(scene_texture, scene_pixels)

        scene_to_downstream.write(
            TextureToWrite.of_texture(scene_texture, HOST_DRAWN_FRAME_COLOUR),
            timestamp_ns=frame.timestamp_ns,
        )

    return each_frame

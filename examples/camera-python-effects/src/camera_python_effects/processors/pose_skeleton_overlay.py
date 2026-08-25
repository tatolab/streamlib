# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Detects a pose in the camera frame and draws a neon skeleton for it.

Both halves are GPU-resident and neither is StreamLib's. Inference reads the
camera frame as a CUDA tensor straight off the typed read — `torch.from_dlpack`
consumes the frame object itself, so no pixel crosses into host memory — and
the drawing is a fragment shader the engine compiles and dispatches, because
no vertex buffer is reachable from a Python processor.

Detection runs at whatever pace it runs at, and it is slower than the camera.
That costs this processor dropped frames and nothing else: it has its own
interpreter and its own OS process, so nothing downstream waits on it.
"""

from __future__ import annotations

import struct

import torch
from ultralytics import YOLO

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    log,
    output,
    processor,
)

from ..gpu_surface_conventions import (
    COLOR_TARGET_TEXTURE_USAGE,
    TEXTURE_FORMAT,
    read_shader_source,
    video_frame_bag_naming,
)
from ..pose_keypoint_packing import (
    COCO_KEYPOINT_COUNT,
    POSE_PUSH_CONSTANT_FORMAT,
    POSE_PUSH_CONSTANT_SIZE,
    pack_keypoints,
)
from ..published_texture_ring import PublishedTextureRing
from ..single_pass_video_effect import (
    NANOSECONDS_PER_SECOND,
    SHARED_VERTEX_SHADER_FILE_NAME,
)

# Both multiples of 32, as the detector's stride needs, and close enough to
# 16:9 that the whole frame can be squashed to fit rather than letterboxed —
# which is what keeps normalized keypoints mapping linearly back onto it.
INFERENCE_HEIGHT = 384
INFERENCE_WIDTH = 640

@processor(description="Neon COCO-17 skeleton drawn from a detected pose")
class PoseSkeletonOverlay:
    """Camera frame in, transparent skeleton layer out."""

    def __init__(
        self,
        pose_model: str = "yolov8n-pose.pt",
        keypoint_confidence_floor: float = 0.5,
        skeleton_scale: float = 0.5,
    ) -> None:
        self.pose_model_weights = pose_model
        self.keypoint_confidence_floor = keypoint_confidence_floor
        # The compositor samples this layer into a picture-in-picture box a
        # quarter of the screen wide, so drawing it at the camera's full extent
        # buys nothing and costs a slot in the pool bucket every other pass is
        # publishing into. Keypoints are frame-normalized, so the shader draws
        # the same skeleton at any extent.
        self.skeleton_scale = skeleton_scale

    @input(delivery_profile="latest")
    def video_from_camera(self) -> VideoFrame: ...

    @output()
    def skeleton_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        log.info("loading the pose model", model=self.pose_model_weights)
        self.pose_model = YOLO(self.pose_model_weights).to("cuda")

        # No sampled bindings at all: the skeleton is drawn from push constants
        # over transparent black, and the camera picture it was detected in is
        # somebody else's layer.
        self.graphics_kernel = ctx.gpu_full_access.create_graphics_kernel(
            color_attachment_formats=[TEXTURE_FORMAT],
            vertex_source=read_shader_source(SHARED_VERTEX_SHADER_FILE_NAME),
            fragment_source=read_shader_source("pose_skeleton.frag"),
            push_constant_size=POSE_PUSH_CONSTANT_SIZE,
            label="PoseSkeletonOverlay",
        )
        self.first_process_at_ns: int | None = None
        self.output_ring = PublishedTextureRing(COLOR_TARGET_TEXTURE_USAGE)
        self.a_detection_failure_has_been_reported = False

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_camera", into=VideoFrame)
        if frame is None:
            return
        if self.first_process_at_ns is None:
            self.first_process_at_ns = ctx.time
        elapsed_seconds = (ctx.time - self.first_process_at_ns) / NANOSECONDS_PER_SECOND

        try:
            packed_keypoints, visible_mask = self.detect_pose_in(frame)
        except Exception as detection_failure:  # noqa: BLE001 — see below
            # Deliberately every failure. This is the one layer of six that
            # depends on a CUDA stack outside the wheel, and the failure that
            # actually happens is an environment one — a cuDNN whose
            # sublibraries disagree, an out-of-memory, a driver mismatch —
            # not a bug in the frame. A raise here would put a traceback on
            # every camera frame forever; an empty skeleton keeps the other
            # five layers on screen and says once what went wrong.
            self.report_the_first_detection_failure(detection_failure)
            packed_keypoints, visible_mask = [0] * COCO_KEYPOINT_COUNT, 0

        skeleton_width = max(int(frame.width * self.skeleton_scale), 1)
        skeleton_height = max(int(frame.height * self.skeleton_scale), 1)
        skeleton_target = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, skeleton_width, skeleton_height
        )
        self.graphics_kernel.draw(
            bindings={},
            color_targets=[skeleton_target],
            extent=(skeleton_width, skeleton_height),
            vertex_count=3,
            push_constants=struct.pack(
                POSE_PUSH_CONSTANT_FORMAT,
                *packed_keypoints,
                visible_mask,
                float(skeleton_width),
                float(skeleton_height),
                elapsed_seconds,
            ),
        )
        ctx.outputs.write(
            "skeleton_to_downstream",
            video_frame_bag_naming(
                skeleton_target.surface_id,
                skeleton_width,
                skeleton_height,
                frame.timestamp_ns,
            ),
        )

    def report_the_first_detection_failure(self, failure: BaseException) -> None:
        """Say once, per process, that the skeleton layer is dark and why."""
        if self.a_detection_failure_has_been_reported:
            return
        self.a_detection_failure_has_been_reported = True
        log.warn(
            "pose detection failed, so the skeleton layer stays empty; the rest of the "
            "pipeline is unaffected. Not reported again in this process.",
            failure=str(failure),
        )

    def detect_pose_in(self, frame: VideoFrame) -> "tuple[list[int], int]":
        """The highest-confidence pose in the frame, packed for the shader.

        The frame arrives as a CUDA tensor and stays one: the channel drop, the
        layout change, the scale and the resize are all device-side, so the
        detector's input is built without a host round trip.
        """
        camera_pixels = torch.from_dlpack(frame)
        rgb_planes = (
            camera_pixels[..., :3].permute(2, 0, 1).float().div(255.0).unsqueeze(0)
        )
        detector_input = torch.nn.functional.interpolate(
            rgb_planes,
            size=(INFERENCE_HEIGHT, INFERENCE_WIDTH),
            mode="bilinear",
            align_corners=False,
        )

        detections = self.pose_model.predict(detector_input, verbose=False)
        if not detections:
            return [0] * COCO_KEYPOINT_COUNT, 0
        keypoints = detections[0].keypoints
        if keypoints is None or keypoints.xyn is None or len(keypoints.xyn) == 0:
            return [0] * COCO_KEYPOINT_COUNT, 0

        # Normalized against the detector's own input, which is the whole frame
        # squashed — so these map straight back onto the frame's UVs.
        normalized_keypoints = keypoints.xyn[0].tolist()
        confidences = (
            keypoints.conf[0].tolist()
            if keypoints.conf is not None
            else [1.0] * len(normalized_keypoints)
        )
        return pack_keypoints(normalized_keypoints, confidences, self.keypoint_confidence_floor)

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""Rectify the lens, then detect: the order a wide-FOV perception stack runs.

Importable as `nodes.undistorting_object_detector:UndistortingObjectDetector`.
The frame stays on the GPU throughout: a compute kernel rectifies it,
`ModelInputTensorKernel` writes the detector's input tensor from it, and the
boxes are drawn back into it through its DLPack view.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass
from pathlib import Path
from typing import TypedDict

import torch
from tatolab.stream import (
    Image,
    ImageFrameToWrite,
    Input,
    ModelInputTensorGeometry,
    ModelInputTensorKernel,
    NodeOutputTextureRing,
    Output,
    Records,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    log,
    node,
)
from ultralytics import YOLO
from ultralytics.engine.results import Results

from nodes.radial_distortion_model import (
    DEFAULT_RADIAL_DISTORTION_K1,
    DEFAULT_RADIAL_DISTORTION_K2,
    FRAME_TEXTURE_FORMAT,
    RADIAL_DISTORTION_MODEL_SLANG,
    STORAGE_AND_SAMPLED_TEXTURE_USAGE,
    largest_recoverable_normalised_radius,
    workgroups_covering,
)

FISHEYE_FRAME_BINDING = "fisheye_frame"
UNDISTORTED_FRAME_BINDING = "undistorted_frame"

# The project directory, which holds `nodes/`. A node's child interpreter may
# start in any working directory, so a relative weights path resolves here.
PROJECT_DIRECTORY = Path(__file__).resolve().parent.parent

# Push constants are little-endian at the wire:
# `float k1; float k2; float largest_recoverable_radius;`.
LENS_COEFFICIENT_AND_RECOVERY_LIMIT_FORMAT = "<3f"
LENS_COEFFICIENT_AND_RECOVERY_LIMIT_SIZE = struct.calcsize(
    LENS_COEFFICIENT_AND_RECOVERY_LIMIT_FORMAT
)

# Newton converges on this polynomial in three steps for the coefficients a
# real lens has; the fourth covers the rest.
NEWTON_ITERATIONS = 4

# YOLOv8's coarsest feature stride. ultralytics takes a tensor source as already
# preprocessed and does not letterbox it, so both spatial dimensions must be
# whole multiples of this.
DETECTOR_INPUT_STRIDE = 32

DETECTION_REPORT_INTERVAL_NS = 1_000_000_000

DETECTION_RECORD_NAMES = ("detection_count", "detections")

# Cycled by class index; RGBA, the texture's own channel order.
BOX_COLOUR_PALETTE_RGBA = (
    (0, 255, 128, 255),
    (255, 96, 0, 255),
    (64, 160, 255, 255),
    (255, 224, 0, 255),
    (255, 64, 192, 255),
    (160, 255, 64, 255),
)

FISHEYE_RECTIFY_SLANG = (
    RADIAL_DISTORTION_MODEL_SLANG
    + f"static const int NEWTON_ITERATIONS = {NEWTON_ITERATIONS};\n"
    + """
[[vk::binding(0, 0)]] Sampler2D fisheye_frame;
[[vk::binding(1, 0)]] [format("rgba8")] RWTexture2D<float4> undistorted_frame;

struct LensCoefficients {
    float k1;
    float k2;
    float largest_recoverable_radius;
};
[[vk::push_constant]] ConstantBuffer<LensCoefficients> lens;

[numthreads(WORKGROUP_TILE_SIZE, WORKGROUP_TILE_SIZE, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    int2 at = int2(invocation.xy);
    int2 extent = frame_extent_of(undistorted_frame);
    if (at.x >= extent.x || at.y >= extent.y) {
        return;
    }

    float recovered_radius = normalised_radius(at, extent);

    // Nothing was carried across beyond this radius. Newton would still find a
    // root, and sampling at it paints the corners with a mirrored, stretched
    // copy of content from elsewhere in the frame.
    if (recovered_radius > lens.largest_recoverable_radius) {
        undistorted_frame[invocation.xy] = float4(0.0, 0.0, 0.0, 1.0);
        return;
    }

    // Solve radius * radial_scale(radius) = recovered_radius for the distorted
    // radius, by Newton from the recovered radius itself.
    float distorted_radius = recovered_radius;
    for (int iteration = 0; iteration < NEWTON_ITERATIONS; ++iteration) {
        float radius_squared = distorted_radius * distorted_radius;
        float residual =
            distorted_radius * radial_scale(distorted_radius, lens.k1, lens.k2)
            - recovered_radius;
        float derivative = 1.0
            + 3.0 * lens.k1 * radius_squared
            + 5.0 * lens.k2 * radius_squared * radius_squared;
        if (abs(derivative) < 1e-6) {
            break;
        }
        distorted_radius -= residual / derivative;
    }

    float2 centre = frame_centre(extent);
    float radius_correction =
        recovered_radius > 1e-6 ? distorted_radius / recovered_radius : 1.0;
    float2 source_texel = centre + (float2(at) - centre) * radius_correction;

    // The radius test asks whether a circle holds anything; the frame is a
    // rectangle. A texel straight above the centre can reach past the top edge,
    // where the sampler would clamp it into a smeared band.
    float2 last_texel = float2(extent) - 1.0;
    if (any(source_texel < float2(0.0)) || any(source_texel > last_texel)) {
        undistorted_frame[invocation.xy] = float4(0.0, 0.0, 0.0, 1.0);
        return;
    }

    // Level 0 named: a compute stage has no derivatives to choose one by.
    float3 sampled = fisheye_frame.SampleLevel(
        texel_to_sampler_coordinates(source_texel, extent), 0.0
    ).rgb;
    undistorted_frame[invocation.xy] = float4(sampled, 1.0);
}
"""
)


class DetectedObject(TypedDict):
    """One detection as a `detections_to_downstream` record carries it, its box in frame pixels."""

    class_index: int
    class_name: str
    confidence: float
    box_xyxy: list[int]


@dataclass
class UndistortingObjectDetectorConfig:
    """UndistortingObjectDetector's settings, as its node's config in the graph document spells them."""

    radial_distortion_k1: float = DEFAULT_RADIAL_DISTORTION_K1
    radial_distortion_k2: float = DEFAULT_RADIAL_DISTORTION_K2
    detection_confidence_threshold: float = 0.35
    detection_model_weights: str = "yolov8n.pt"
    """An ultralytics detection checkpoint; a relative path resolves against the project directory, where a missing known checkpoint is downloaded."""


@node.meta(
    description="Rectifies the fisheye frame, detects objects in it, and draws their boxes on",
)
@node.process
class UndistortingObjectDetector:
    """Fisheye frame in; the rectified frame with its boxes drawn on, and the detections, out."""

    fisheye_frame_from_upstream = Input(
        Image, delivery="newest", description="Barrelled frames from the lens"
    )
    annotated_frame_to_downstream = Output(
        Image, description="The rectified frames with their boxes drawn on"
    )
    detections_to_downstream = Output(
        Records,
        names=DETECTION_RECORD_NAMES,
        description="One record per annotated frame: its detections, boxes in frame pixels",
    )

    def __init__(self, config: UndistortingObjectDetectorConfig) -> None:
        radial_distortion_k1 = float(config.radial_distortion_k1)
        radial_distortion_k2 = float(config.radial_distortion_k2)
        self.largest_recoverable_radius = largest_recoverable_normalised_radius(
            radial_distortion_k1, radial_distortion_k2
        )
        self.lens_coefficient_push_constants = struct.pack(
            LENS_COEFFICIENT_AND_RECOVERY_LIMIT_FORMAT,
            radial_distortion_k1,
            radial_distortion_k2,
            self.largest_recoverable_radius,
        )
        self.detection_confidence_threshold = float(
            config.detection_confidence_threshold
        )
        self.detection_model_weights_path = (
            PROJECT_DIRECTORY / config.detection_model_weights
        )
        self.next_detection_report_at_ns = 0

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.undistorted_frame_ring = NodeOutputTextureRing(
            FRAME_TEXTURE_FORMAT, STORAGE_AND_SAMPLED_TEXTURE_USAGE
        )
        self.fisheye_rectify_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=FISHEYE_RECTIFY_SLANG,
            push_constant_size=LENS_COEFFICIENT_AND_RECOVERY_LIMIT_SIZE,
            bindings={
                FISHEYE_FRAME_BINDING: "sampled_texture",
                UNDISTORTED_FRAME_BINDING: "storage_image",
            },
        )
        # Padded at the right and bottom rather than resized, so a box the
        # model reports is already in frame coordinates.
        self.detector_input_kernel = ModelInputTensorKernel.compile(
            ctx.gpu_full_access,
            fit="pad_bottom_right",
            pad_to_multiple_of=DETECTOR_INPUT_STRIDE,
            channel_order="rgb",
            layout="nchw",
            dtype="float32",
            scale=1.0 / 255.0,
        )
        # A frame's DLPack capsule imports onto the accelerator this torch
        # build drives, so the model and the box colours go there too.
        detection_device = torch.accelerator.current_accelerator(check_available=True)
        if detection_device is None:
            raise RuntimeError(
                "torch.accelerator.current_accelerator() is None: this torch "
                "build sees no usable GPU, and the detector reads the frame "
                "where the engine left it, on the GPU"
            )
        self.detection_model = YOLO(str(self.detection_model_weights_path))
        self.detection_model.to(detection_device)
        self.box_colours = [
            torch.tensor(colour, dtype=torch.uint8, device=detection_device)
            for colour in BOX_COLOUR_PALETTE_RGBA
        ]
        log.info(
            "rectifier and detector ready",
            detection_model_weights=str(self.detection_model_weights_path),
            largest_recoverable_radius=round(self.largest_recoverable_radius, 4),
            detection_confidence_threshold=self.detection_confidence_threshold,
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(self.fisheye_frame_from_upstream)
        if frame is None:
            return

        undistorted_frame_texture = (
            self.undistorted_frame_ring.next_texture_for_this_frame(
                ctx.gpu_limited_access, frame.width, frame.height
            )
        )
        # The lens published a kernel's texture-backed output, so its surface
        # id binds straight into this dispatch with no landing copy.
        self.fisheye_rectify_kernel.dispatch(
            bindings={
                FISHEYE_FRAME_BINDING: frame.surface_id,
                UNDISTORTED_FRAME_BINDING: undistorted_frame_texture,
            },
            group_count=(
                workgroups_covering(frame.width),
                workgroups_covering(frame.height),
                1,
            ),
            push_constants=self.lens_coefficient_push_constants,
        )

        detector_model_input = self.detector_input_kernel.apply_to_surface(
            ctx.gpu_limited_access, undistorted_frame_texture
        )
        with detector_model_input.tensor_surface as detector_input_tensor_surface:
            detections = self._detections_in(
                torch.from_dlpack(detector_input_tensor_surface),
                detector_model_input.geometry,
            )

        with undistorted_frame_texture.as_device_tensor() as rectified_pixels:
            self._draw_boxes_into(torch.from_dlpack(rectified_pixels), detections)

        self._report_detections(ctx.time, detections)
        # Both stamped with the capture's timestamp, which is how a consumer
        # pairs a frame with its detections.
        ctx.outputs.write(
            self.annotated_frame_to_downstream,
            ImageFrameToWrite.of_texture(undistorted_frame_texture, frame.colour),
            timestamp_ns=frame.timestamp_ns,
        )
        ctx.outputs.write(
            self.detections_to_downstream,
            {"detection_count": len(detections), "detections": detections},
            timestamp_ns=frame.timestamp_ns,
        )

    def _detections_in(
        self,
        detector_input_batch: torch.Tensor,
        detector_input_geometry: ModelInputTensorGeometry,
    ) -> list[DetectedObject]:
        """Run the detector over the prepared input on the GPU; boxes come back in frame pixels."""
        width = detector_input_geometry.source_width
        height = detector_input_geometry.source_height
        (detection_result,) = self.detection_model.predict(
            source=detector_input_batch,
            conf=self.detection_confidence_threshold,
            verbose=False,
        )
        assert isinstance(detection_result, Results)
        boxes = detection_result.boxes
        if boxes is None or len(boxes) == 0:
            return []

        # One host transfer of a few dozen numbers: the slicing that draws the
        # boxes needs Python ints. The pixels stay on the GPU.
        corners = (
            detector_input_geometry.boxes_to_source(boxes.xyxy)
            .round()
            .to(torch.int32)
            .tolist()
        )
        class_indices = torch.as_tensor(boxes.cls).to(torch.int32).tolist()
        confidences = torch.as_tensor(boxes.conf).tolist()
        return [
            {
                "class_index": class_index,
                "class_name": self.detection_model.names[class_index],
                "confidence": round(confidence, 4),
                "box_xyxy": [
                    max(0, min(int(left), width - 1)),
                    max(0, min(int(top), height - 1)),
                    max(0, min(int(right), width)),
                    max(0, min(int(bottom), height)),
                ],
            }
            for (left, top, right, bottom), class_index, confidence in zip(
                corners, class_indices, confidences
            )
        ]

    def _draw_boxes_into(
        self, rectified_frame: torch.Tensor, detections: list[DetectedObject]
    ) -> None:
        """Outline each detection in the frame itself, on the GPU, four slice assignments an edge."""
        height, _, _ = rectified_frame.shape
        # Scaled with the frame, with a floor so an edge stays visible at small
        # capture sizes.
        edge = max(2, height // 240)
        for detection in detections:
            left, top, right, bottom = detection["box_xyxy"]
            if right - left < edge or bottom - top < edge:
                continue
            colour = self.box_colours[detection["class_index"] % len(self.box_colours)]
            rectified_frame[top : top + edge, left:right] = colour
            rectified_frame[bottom - edge : bottom, left:right] = colour
            rectified_frame[top:bottom, left : left + edge] = colour
            rectified_frame[top:bottom, right - edge : right] = colour

    def _report_detections(
        self, monotonic_now_ns: int, detections: list[DetectedObject]
    ) -> None:
        """Log a summary of the detections, at most once a second."""
        if monotonic_now_ns < self.next_detection_report_at_ns:
            return
        self.next_detection_report_at_ns = (
            monotonic_now_ns + DETECTION_REPORT_INTERVAL_NS
        )
        log.info(
            "detections on the rectified frame",
            detection_count=len(detections),
            class_names=sorted({detection["class_name"] for detection in detections}),
        )

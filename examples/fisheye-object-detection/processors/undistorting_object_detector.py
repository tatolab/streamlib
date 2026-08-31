# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""Rectify the lens, then detect — the order a wide-FOV perception stack runs.

A detector trained on rectilinear photographs reads a barrelled frame worst
exactly where a wide lens bends it most: the periphery, which on a drone is
where the obstacle you have not hit yet lives. So the distortion is undone
before the model sees anything, on the GPU, with the frame never leaving it.

Two things happen inside one scope here, and that is the point of the module:

    with undistorted_frame_texture.as_device_tensor() as rectified_pixels:
        rectified_frame = torch.from_dlpack(rectified_pixels)
        ...detect on it, then draw the boxes back into it...

Entering the scope hands the engine's texture out as a linear DLPack view, so
`torch.from_dlpack` is the whole read and the tensor is GPU-resident. The same
tensor is the write door: boxes drawn into it are blitted back when the scope
closes, ordered by the engine ahead of its own next read. No fence, no
timeline, no `torch.cuda.synchronize()`, and no copy through the host.
"""

from __future__ import annotations

import struct
from typing import Any

import torch
from ultralytics import YOLO

from processors.radial_distortion_model import (
    RADIAL_DISTORTION_MODEL_GLSL,
    largest_recoverable_normalised_radius,
    workgroups_covering,
)
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

FISHEYE_FRAME_INPUT_PORT = "fisheye_frame_from_upstream"
ANNOTATED_FRAME_OUTPUT_PORT = "annotated_frame_to_downstream"

FISHEYE_FRAME_BINDING = "fisheye_frame"
UNDISTORTED_FRAME_BINDING = "undistorted_frame"

TEXTURE_FORMAT = "rgba8_unorm"
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

# Three `float`s, little-endian at the wire: the lens's own two coefficients
# plus the radius past which this shader must not try to reconstruct anything.
LENS_COEFFICIENT_AND_RECOVERY_LIMIT_FORMAT = "<3f"
LENS_COEFFICIENT_AND_RECOVERY_LIMIT_SIZE = struct.calcsize(
    LENS_COEFFICIENT_AND_RECOVERY_LIMIT_FORMAT
)

# Newton converges on this polynomial in three steps for any coefficient pair
# a real lens produces; the fourth is there for the ones it does not.
NEWTON_ITERATIONS = 4

# YOLOv8's coarsest feature stride. A tensor handed to `predict` is taken as
# already preprocessed — ultralytics does not letterbox one — so both of its
# spatial dimensions must be whole multiples of this, and it is the caller who
# makes them so.
DETECTOR_INPUT_STRIDE = 32

# One summary line a second rather than one a frame: at camera cadence the
# per-frame spelling buries every other record in the log, and every record a
# helper writes crosses to the parent's pipeline to get there.
DETECTION_REPORT_INTERVAL_NS = 1_000_000_000

# Box colours, cycled by class index so two classes in one frame are told
# apart at a glance. RGBA, matching the texture's own channel order.
BOX_COLOUR_PALETTE_RGBA = (
    (0, 255, 128, 255),
    (255, 96, 0, 255),
    (64, 160, 255, 255),
    (255, 224, 0, 255),
    (255, 64, 192, 255),
    (160, 255, 64, 255),
)

FISHEYE_RECTIFY_GLSL = (
    RADIAL_DISTORTION_MODEL_GLSL
    + f"#define NEWTON_ITERATIONS {NEWTON_ITERATIONS}\n"
    + """
layout(set = 0, binding = 0) uniform sampler2D fisheye_frame;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D undistorted_frame;

layout(push_constant) uniform LensCoefficients {
    float k1;
    float k2;
    float largest_recoverable_radius;
} lens;

void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    ivec2 extent = imageSize(undistorted_frame);
    if (at.x >= extent.x || at.y >= extent.y) {
        return;
    }

    float recovered_radius = normalised_radius(at, extent);

    // The lens sampled its whole frame out of an inner disc, so nothing was
    // ever carried across beyond this radius. Newton would still return a
    // root here — the polynomial does not stop existing — and sampling at it
    // paints the corners with a mirrored, stretched copy of content from
    // somewhere else in the frame. Honest black instead.
    if (recovered_radius > lens.largest_recoverable_radius) {
        imageStore(undistorted_frame, at, vec4(0.0, 0.0, 0.0, 1.0));
        return;
    }

    // Solve radius * radial_scale(radius) = recovered_radius for the radius in
    // the distorted frame this output pixel came from. Newton from the
    // recovered radius itself, which is within a few percent of the answer for
    // any coefficients a lens actually has.
    float distorted_radius = recovered_radius;
    for (int iteration = 0; iteration < NEWTON_ITERATIONS; ++iteration) {
        float radius_squared = distorted_radius * distorted_radius;
        float residual =
            distorted_radius * radial_scale(distorted_radius, lens.k1, lens.k2)
            - recovered_radius;
        float derivative = 1.0
            + 3.0 * lens.k1 * radius_squared
            + 5.0 * lens.k2 * radius_squared * radius_squared;
        // At the polynomial's stationary point the step is unbounded. The
        // radius guard above has already handled the pixels that reach it;
        // this keeps float noise at the boundary from throwing the rest.
        if (abs(derivative) < 1e-6) {
            break;
        }
        distorted_radius -= residual / derivative;
    }

    // Same direction, corrected distance. The centre pixel has no direction to
    // preserve and needs no correction either.
    vec2 centre = frame_centre(extent);
    float radius_correction =
        recovered_radius > 1e-6 ? distorted_radius / recovered_radius : 1.0;
    vec2 source_texel = centre + (vec2(at) - centre) * radius_correction;

    // The radius test above is necessary and not sufficient: it asks whether a
    // circle of that radius holds anything, and the frame is a rectangle. A
    // pixel straight above the centre reaches a source radius near 1 — the
    // half-diagonal — while the top edge is only the half-height away, so it
    // lands off the frame entirely and the sampler would clamp it into a
    // smeared band along the edge. This is the same test the lens shader makes
    // for the same reason, and together they mask exactly the pixels no source
    // texel maps onto.
    vec2 last_texel = vec2(extent) - 1.0;
    if (any(lessThan(source_texel, vec2(0.0)))
        || any(greaterThan(source_texel, last_texel))) {
        imageStore(undistorted_frame, at, vec4(0.0, 0.0, 0.0, 1.0));
        return;
    }

    vec3 sampled = texture(
        fisheye_frame, texel_to_sampler_coordinates(source_texel, extent)
    ).rgb;
    imageStore(undistorted_frame, at, vec4(sampled, 1.0));
}
"""
)


@processor(description="Rectifies the fisheye frame, then detects objects in it")
class UndistortingObjectDetector:
    """Fisheye frame in, the rectified picture with its detections drawn on out."""

    def __init__(
        self,
        radial_distortion_k1: float = -0.25,
        radial_distortion_k2: float = 0.0,
        detection_confidence_threshold: float = 0.35,
        detection_model_weights: str = "yolov8n.pt",
    ) -> None:
        self.largest_recoverable_radius = largest_recoverable_normalised_radius(
            float(radial_distortion_k1), float(radial_distortion_k2)
        )
        self.lens_coefficient_push_constants = struct.pack(
            LENS_COEFFICIENT_AND_RECOVERY_LIMIT_FORMAT,
            float(radial_distortion_k1),
            float(radial_distortion_k2),
            self.largest_recoverable_radius,
        )
        self.detection_confidence_threshold = float(detection_confidence_threshold)
        self.detection_model_weights = detection_model_weights
        self.next_detection_report_at_ns = 0

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.undistorted_frame_ring = ProcessorOutputTextureRing(
            TEXTURE_FORMAT, STORAGE_AND_SAMPLED_TEXTURE_USAGE
        )
        self.fisheye_rectify_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=FISHEYE_RECTIFY_GLSL,
            push_constant_size=LENS_COEFFICIENT_AND_RECOVERY_LIMIT_SIZE,
            bindings={
                FISHEYE_FRAME_BINDING: "sampled_texture",
                UNDISTORTED_FRAME_BINDING: "storage_image",
            },
        )
        # Weights land beside the app on first run and are cached there after.
        # The model goes to the GPU here, in `setup()`, so the first frame pays
        # for a forward pass and not for loading a network.
        self.detection_model = YOLO(self.detection_model_weights)
        self.detection_model.to("cuda")
        self.box_colours = [
            torch.tensor(colour, dtype=torch.uint8, device="cuda")
            for colour in BOX_COLOUR_PALETTE_RGBA
        ]
        log.info(
            "rectifier and detector ready",
            detection_model_weights=self.detection_model_weights,
            largest_recoverable_radius=round(self.largest_recoverable_radius, 4),
            detection_confidence_threshold=self.detection_confidence_threshold,
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(FISHEYE_FRAME_INPUT_PORT, into=VideoFrame)
        if frame is None:
            return

        undistorted_frame_texture = (
            self.undistorted_frame_ring.next_texture_for_this_frame(
                ctx.gpu_limited_access, frame.width, frame.height
            )
        )
        # No landing copy on this hop, unlike the lens's. The frame upstream
        # published is a kernel's own output — texture-backed already — so its
        # surface id binds straight into the dispatch, across the process
        # boundary between the two helpers and with no copy anywhere.
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

        with undistorted_frame_texture.as_device_tensor() as rectified_pixels:
            rectified_frame = torch.from_dlpack(rectified_pixels)
            detections = self._detections_in(rectified_frame)
            self._draw_boxes_into(rectified_frame, detections)

        self._report_detections(ctx.time, detections)
        ctx.outputs.write(
            ANNOTATED_FRAME_OUTPUT_PORT,
            {
                "surface_id": undistorted_frame_texture.surface_id,
                "width": frame.width,
                "height": frame.height,
                "timestamp_ns": frame.timestamp_ns,
                # A bag is whatever its producer writes, so the detections ride
                # it beside the picture they were found in. A consumer that
                # only wants the frame — the window on this port — reads the
                # keys it knows and ignores the rest.
                "detection_count": len(detections),
                "detections": detections,
            },
        )

    def _detections_in(self, rectified_frame: torch.Tensor) -> "list[dict[str, Any]]":
        """Run the detector over the rectified frame, without leaving the GPU.

        A `torch.Tensor` source is taken as already preprocessed — ultralytics
        letterboxes arrays, never tensors — so the batch is built here in the
        shape the model wants: RGB, channels-first, batched, and scaled into
        `[0, 1]`. Every step of that is a device operation on a device tensor.
        """
        height, width, _ = rectified_frame.shape
        detector_input = (
            rectified_frame[..., :3]
            .permute(2, 0, 1)
            .unsqueeze(0)
            .contiguous()
            .float()
            .div_(255.0)
        )
        # Padded rather than resized, and at the right and bottom rather than
        # centred, so a box the model reports is already in frame coordinates
        # and nothing has to be scaled back.
        pad_right = -width % DETECTOR_INPUT_STRIDE
        pad_bottom = -height % DETECTOR_INPUT_STRIDE
        if pad_right or pad_bottom:
            detector_input = torch.nn.functional.pad(
                detector_input, (0, pad_right, 0, pad_bottom)
            )

        result = self.detection_model.predict(
            source=detector_input,
            conf=self.detection_confidence_threshold,
            verbose=False,
        )[0]
        boxes = result.boxes
        if boxes is None or len(boxes) == 0:
            return []

        # One host transfer, of a few dozen numbers, because the drawing below
        # is Python slicing the tensor and Python needs the indices. The pixels
        # stay where they are.
        corners = boxes.xyxy.round().to(torch.int32).tolist()
        class_indices = boxes.cls.to(torch.int32).tolist()
        confidences = boxes.conf.tolist()
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
        self, rectified_frame: torch.Tensor, detections: "list[dict[str, Any]]"
    ) -> None:
        """Outline each detection in the frame itself, on the GPU.

        Four slice assignments an edge, which is the whole of it: the frame is
        an ordinary torch tensor here, and writing into it is what publishes
        the annotation when the enclosing scope closes.
        """
        height, _, _ = rectified_frame.shape
        # Scaled with the frame so the outline reads the same at any capture
        # size, with a floor for the small ones: below 480 rows the ratio alone
        # rounds an edge down to a single pixel, which a window scaling to fit
        # can drop entirely.
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
        self, monotonic_now_ns: int, detections: "list[dict[str, Any]]"
    ) -> None:
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

    @input(
        delivery_profile="newest",
        description="Barrelled frames from the lens, as VideoFrame bags",
    )
    def fisheye_frame_from_upstream(self) -> VideoFrame: ...

    @output(description="The rectified frames with their detections drawn on")
    def annotated_frame_to_downstream(self) -> VideoFrame: ...

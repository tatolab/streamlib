# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The wire contract between the pose detector and its shader.

WIRE CONTRACT — matches `shaders/pose_skeleton.frag` byte for byte: 17
`packUnorm2x16` keypoints, a visibility bitmask, the frame extent, and elapsed
seconds. 84 bytes, inside the 128 Vulkan guarantees for a push-constant range;
a `vec2` per keypoint would need 136 for the keypoints alone.

Separate from the processor so the packing can be tested without loading a
detector — and so the two halves of the contract, this and the GLSL block, are
each in one place.
"""

from __future__ import annotations

import struct

__all__ = [
    "COCO_KEYPOINT_COUNT",
    "POSE_PUSH_CONSTANT_FORMAT",
    "POSE_PUSH_CONSTANT_SIZE",
    "UNORM_16_MAXIMUM",
    "pack_keypoints",
]

COCO_KEYPOINT_COUNT = 17

POSE_PUSH_CONSTANT_FORMAT = f"<{COCO_KEYPOINT_COUNT}I I 2f f"
POSE_PUSH_CONSTANT_SIZE = struct.calcsize(POSE_PUSH_CONSTANT_FORMAT)

UNORM_16_MAXIMUM = 65535


def pack_keypoints(
    normalized_keypoints: "list[tuple[float, float]]",
    keypoint_confidences: "list[float]",
    confidence_floor: float,
) -> "tuple[list[int], int]":
    """The 17 packed keypoints and the visibility mask the shader reads.

    A keypoint below the floor is packed as zero and left out of the mask — the
    shader skips it and every bone that ends on it, so a partly-visible subject
    draws the part that is there instead of a limb snapped to a corner. Same
    for one the detector put outside the frame.
    """
    packed = [0] * COCO_KEYPOINT_COUNT
    visible_mask = 0
    for index in range(min(COCO_KEYPOINT_COUNT, len(normalized_keypoints))):
        if keypoint_confidences[index] < confidence_floor:
            continue
        x, y = normalized_keypoints[index]
        if not (0.0 <= x <= 1.0 and 0.0 <= y <= 1.0):
            continue
        packed[index] = round(x * UNORM_16_MAXIMUM) | (
            round(y * UNORM_16_MAXIMUM) << 16
        )
        visible_mask |= 1 << index
    return packed, visible_mask

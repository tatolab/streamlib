# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The detector-to-shader packing, unpacked the way the shader unpacks it."""

from __future__ import annotations

from camera_python_effects.pose_keypoint_packing import (
    COCO_KEYPOINT_COUNT,
    UNORM_16_MAXIMUM,
    pack_keypoints,
)

EVERY_KEYPOINT_CONFIDENT = [1.0] * COCO_KEYPOINT_COUNT


def unpack_unorm_2x16(packed: int) -> "tuple[float, float]":
    """`unpackUnorm2x16`, as the shader spells it — x low, y high."""
    return (
        (packed & 0xFFFF) / UNORM_16_MAXIMUM,
        (packed >> 16) / UNORM_16_MAXIMUM,
    )


def test_a_packed_keypoint_unpacks_to_where_it_was() -> None:
    keypoints = [(index / 32.0, 1.0 - index / 32.0) for index in range(COCO_KEYPOINT_COUNT)]
    packed, visible_mask = pack_keypoints(keypoints, EVERY_KEYPOINT_CONFIDENT, 0.5)

    assert visible_mask == (1 << COCO_KEYPOINT_COUNT) - 1
    for index, (expected_x, expected_y) in enumerate(keypoints):
        unpacked_x, unpacked_y = unpack_unorm_2x16(packed[index])
        # One part in 65535 is the whole precision of the format.
        assert abs(unpacked_x - expected_x) < 1e-4
        assert abs(unpacked_y - expected_y) < 1e-4


def test_the_frame_corners_survive_the_round_trip() -> None:
    """The endpoints are where a fixed-point packing loses a subject's hands."""
    packed, _ = pack_keypoints(
        [(0.0, 0.0), (1.0, 1.0)] + [(0.5, 0.5)] * 15, EVERY_KEYPOINT_CONFIDENT, 0.5
    )
    assert unpack_unorm_2x16(packed[0]) == (0.0, 0.0)
    assert unpack_unorm_2x16(packed[1]) == (1.0, 1.0)


def test_a_keypoint_below_the_floor_is_left_out_of_the_mask() -> None:
    confidences = [0.9] * COCO_KEYPOINT_COUNT
    confidences[7] = 0.2
    packed, visible_mask = pack_keypoints(
        [(0.5, 0.5)] * COCO_KEYPOINT_COUNT, confidences, 0.5
    )
    assert visible_mask & (1 << 7) == 0
    assert packed[7] == 0
    assert visible_mask & (1 << 6) != 0


def test_a_keypoint_outside_the_frame_is_dropped_rather_than_wrapped() -> None:
    """A packing that wrapped would snap the limb to the opposite edge."""
    keypoints: "list[tuple[float, float]]" = [(0.5, 0.5)] * COCO_KEYPOINT_COUNT
    keypoints[3] = (1.4, 0.5)
    keypoints[4] = (0.5, -0.2)
    _, visible_mask = pack_keypoints(keypoints, EVERY_KEYPOINT_CONFIDENT, 0.5)
    assert visible_mask & (1 << 3) == 0
    assert visible_mask & (1 << 4) == 0


def test_a_detector_returning_nothing_packs_an_empty_skeleton() -> None:
    packed, visible_mask = pack_keypoints([], [], 0.5)
    assert visible_mask == 0
    assert packed == [0] * COCO_KEYPOINT_COUNT


def test_fewer_keypoints_than_the_skeleton_has_are_taken_as_they_come() -> None:
    """A detector with its own keypoint count must not raise here."""
    packed, visible_mask = pack_keypoints([(0.25, 0.75)] * 5, [1.0] * 5, 0.5)
    assert visible_mask == 0b11111
    assert packed[5:] == [0] * (COCO_KEYPOINT_COUNT - 5)

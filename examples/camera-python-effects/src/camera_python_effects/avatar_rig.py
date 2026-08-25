# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Turns 33 MediaPipe world landmarks into posed android body segments.

The character is a hard-surface robot: every body part is a rigid primitive
locked to one bone, so posing it needs no skin weights, no bind poses and no
rig retargeting — just a position, an orientation and a length per segment.
Cylindrical limbs are symmetric about their own axis, which makes the roll
ambiguity a 2D detector's depth cannot resolve simply invisible.

Pure numpy, streamlib-free and GL-free: given joints it answers matrices, so
every pose the solver can strike is testable without a camera or a context.
"""

from __future__ import annotations

import math
from dataclasses import dataclass

import numpy

__all__ = [
    "JOINT_NAMES",
    "SegmentPlacement",
    "SmoothedPose",
    "idle_joints",
    "solve_segment_placements",
]

# The MediaPipe pose-landmark indices this rig reads, by name.
_LANDMARKS = {
    "nose": 0,
    "left_ear": 7,
    "right_ear": 8,
    "left_shoulder": 11,
    "right_shoulder": 12,
    "left_elbow": 13,
    "right_elbow": 14,
    "left_wrist": 15,
    "right_wrist": 16,
    "left_hip": 23,
    "right_hip": 24,
    "left_knee": 25,
    "right_knee": 26,
    "left_ankle": 27,
    "right_ankle": 28,
    "left_foot": 31,
    "right_foot": 32,
}

JOINT_NAMES = tuple(_LANDMARKS)


@dataclass
class SegmentPlacement:
    """One rigid body part: which primitive, where, and how it glows."""

    primitive: str  # "prism" | "sphere" | "box"
    model_matrix: numpy.ndarray  # 4x4, column-major-ready
    base_colour: "tuple[float, float, float]"
    emissive: float


GUNMETAL = (0.10, 0.11, 0.14)
DARK_PLATE = (0.06, 0.065, 0.09)
VISOR_MAGENTA = (1.0, 0.10, 0.65)
ACCENT_YELLOW = (0.99, 0.93, 0.04)
CORE_CYAN = (0.0, 0.95, 1.0)


def _normalized(vector: numpy.ndarray) -> numpy.ndarray:
    length = float(numpy.linalg.norm(vector))
    return vector / length if length > 1e-8 else numpy.array([0.0, 1.0, 0.0])


def _basis_from_y(y_axis: numpy.ndarray, forward_hint: numpy.ndarray) -> numpy.ndarray:
    """A right-handed 3x3 basis whose +Y is `y_axis`, rolled toward the hint."""
    y = _normalized(y_axis)
    x = numpy.cross(forward_hint, y)
    if numpy.linalg.norm(x) < 1e-6:
        x = numpy.cross(numpy.array([0.0, 0.0, 1.0]), y)
    x = _normalized(x)
    z = _normalized(numpy.cross(x, y))
    return numpy.stack([x, y, z], axis=1)


def _placement(
    primitive: str,
    start: numpy.ndarray,
    end: numpy.ndarray,
    thickness: float,
    colour: "tuple[float, float, float]",
    emissive: float,
    forward_hint: numpy.ndarray,
    depth: "float | None" = None,
) -> SegmentPlacement:
    """A primitive stretched from `start` to `end`, `thickness` across."""
    along = end - start
    length = max(float(numpy.linalg.norm(along)), 1e-4)
    rotation = _basis_from_y(along, forward_hint)
    model = numpy.identity(4)
    model[:3, :3] = rotation @ numpy.diag([thickness, length, depth or thickness])
    model[:3, 3] = start
    return SegmentPlacement(primitive, model, colour, emissive)


def _orb(
    centre: numpy.ndarray,
    radius: float,
    colour: "tuple[float, float, float]",
    emissive: float,
) -> SegmentPlacement:
    model = numpy.identity(4)
    model[:3, :3] = numpy.identity(3) * radius
    model[:3, 3] = centre
    return SegmentPlacement("sphere", model, colour, emissive)


class SmoothedPose:
    """Exponential smoothing over the joint set, tuned per axis.

    MediaPipe's video mode smooths already; this settles what remains, with
    depth smoothed harder than the image plane because monocular z is the
    noisy axis. dt-aware so the response does not change with frame rate.
    """

    def __init__(self, plane_hz: float = 12.0, depth_hz: float = 4.0) -> None:
        self._plane_hz = plane_hz
        self._depth_hz = depth_hz
        self._state: "dict[str, numpy.ndarray] | None" = None

    def settle(
        self, joints: "dict[str, numpy.ndarray]", dt_seconds: float
    ) -> "dict[str, numpy.ndarray]":
        if self._state is None:
            self._state = {name: joint.copy() for name, joint in joints.items()}
            return self._state
        plane_alpha = 1.0 - math.exp(-2.0 * math.pi * self._plane_hz * dt_seconds)
        depth_alpha = 1.0 - math.exp(-2.0 * math.pi * self._depth_hz * dt_seconds)
        blend = numpy.array([plane_alpha, plane_alpha, depth_alpha])
        for name, joint in joints.items():
            held = self._state[name]
            held += (joint - held) * blend
        return self._state


def idle_joints(elapsed_seconds: float) -> "dict[str, numpy.ndarray]":
    """A standing pose with a slow breathing sway, for when nobody is in frame."""
    sway = math.sin(elapsed_seconds * 0.9) * 0.02
    breathe = math.sin(elapsed_seconds * 1.7) * 0.008
    j: "dict[str, numpy.ndarray]" = {}

    def at(name: str, x: float, y: float, z: float) -> None:
        j[name] = numpy.array([x + sway, y + breathe, z])

    at("left_hip", 0.10, 0.92, 0.0)
    at("right_hip", -0.10, 0.92, 0.0)
    at("left_shoulder", 0.18, 1.42, 0.0)
    at("right_shoulder", -0.18, 1.42, 0.0)
    at("left_elbow", 0.24, 1.14, 0.02)
    at("right_elbow", -0.24, 1.14, 0.02)
    at("left_wrist", 0.27, 0.88, 0.06)
    at("right_wrist", -0.27, 0.88, 0.06)
    at("left_knee", 0.11, 0.50, 0.02)
    at("right_knee", -0.11, 0.50, 0.02)
    at("left_ankle", 0.12, 0.08, 0.0)
    at("right_ankle", -0.12, 0.08, 0.0)
    at("left_foot", 0.13, 0.02, 0.14)
    at("right_foot", -0.13, 0.02, 0.14)
    at("nose", 0.0, 1.62, 0.09)
    at("left_ear", 0.07, 1.60, 0.0)
    at("right_ear", -0.07, 1.60, 0.0)
    return j


def joints_from_world_landmarks(landmarks) -> "dict[str, numpy.ndarray]":
    """MediaPipe world landmarks → this rig's joint set, mirrored.

    World landmarks are metres with y down; the scene is y up. x is negated
    too, so the android moves like a mirror — raise your right hand and the
    figure facing you raises the hand on your right, which is how every
    filter-style effect reads as "me".
    """
    return {
        name: numpy.array(
            [-landmarks[index].x, -landmarks[index].y, -landmarks[index].z]
        )
        for name, index in _LANDMARKS.items()
    }


def _grounded(joints: "dict[str, numpy.ndarray]") -> "dict[str, numpy.ndarray]":
    """Pelvis over the origin, feet on the floor — the stage is the frame."""
    pelvis = (joints["left_hip"] + joints["right_hip"]) / 2.0
    floor = min(float(joints[name][1]) for name in ("left_ankle", "right_ankle"))
    shift = numpy.array([pelvis[0], floor - 0.06, pelvis[2]])
    return {name: joint - shift for name, joint in joints.items()}


def solve_segment_placements(
    joints: "dict[str, numpy.ndarray]",
) -> "list[SegmentPlacement]":
    """The full android, as rigid primitives posed over the joint set."""
    j = _grounded(joints)

    pelvis = (j["left_hip"] + j["right_hip"]) / 2.0
    chest = (j["left_shoulder"] + j["right_shoulder"]) / 2.0
    across_shoulders = _normalized(j["left_shoulder"] - j["right_shoulder"])
    spine_up = _normalized(chest - pelvis)
    torso_forward = _normalized(numpy.cross(across_shoulders, spine_up))

    head_centre = (j["left_ear"] + j["right_ear"]) / 2.0
    head_up = _normalized(head_centre - chest)
    neck_base = chest + spine_up * 0.06

    placements = [
        # Torso: a chest plate over a narrower waist block.
        _placement("box", pelvis + spine_up * 0.16, chest + spine_up * 0.05,
                   0.30, GUNMETAL, 0.0, torso_forward, depth=0.17),
        _placement("box", pelvis - spine_up * 0.08, pelvis + spine_up * 0.18,
                   0.24, DARK_PLATE, 0.0, torso_forward, depth=0.15),
        # Chest core — the arc-reactor-ish glow — and the yellow service badge.
        _orb(chest - spine_up * 0.10 + torso_forward * 0.105, 0.05, CORE_CYAN, 1.0),
        _placement("box",
                   chest + across_shoulders * 0.10 - spine_up * 0.28 + torso_forward * 0.088,
                   chest + across_shoulders * 0.10 - spine_up * 0.21 + torso_forward * 0.088,
                   0.05, ACCENT_YELLOW, 0.85, torso_forward, depth=0.012),
        # Neck and head; the visor is a thin emissive slab across the face.
        _placement("prism", neck_base, head_centre, 0.06, DARK_PLATE, 0.0, torso_forward),
        _placement("box", head_centre - head_up * 0.075, head_centre + head_up * 0.135,
                   0.19, GUNMETAL, 0.0, torso_forward, depth=0.21),
        _placement("box",
                   head_centre + head_up * 0.005 + torso_forward * 0.107,
                   head_centre + head_up * 0.062 + torso_forward * 0.107,
                   0.15, VISOR_MAGENTA, 1.0, torso_forward, depth=0.02),
    ]

    for side in ("left", "right"):
        shoulder = j[f"{side}_shoulder"]
        elbow = j[f"{side}_elbow"]
        wrist = j[f"{side}_wrist"]
        hip = j[f"{side}_hip"]
        knee = j[f"{side}_knee"]
        ankle = j[f"{side}_ankle"]
        foot = j[f"{side}_foot"]
        hand_tip = wrist + _normalized(wrist - elbow) * 0.15

        placements += [
            _orb(shoulder, 0.062, GUNMETAL, 0.0),
            _placement("prism", shoulder, elbow, 0.062, GUNMETAL, 0.0, torso_forward),
            _orb(elbow, 0.048, DARK_PLATE, 0.0),
            _orb(elbow, 0.022, CORE_CYAN, 1.0),
            _placement("prism", elbow, wrist, 0.052, GUNMETAL, 0.0, torso_forward),
            _placement("box", wrist, hand_tip, 0.055, DARK_PLATE, 0.0, torso_forward, depth=0.09),
            _placement("prism", hip, knee, 0.088, GUNMETAL, 0.0, torso_forward),
            _orb(knee, 0.055, DARK_PLATE, 0.0),
            _orb(knee, 0.024, CORE_CYAN, 1.0),
            _placement("prism", knee, ankle, 0.068, GUNMETAL, 0.0, torso_forward),
            _placement("box", ankle, foot + numpy.array([0.0, -0.01, 0.0]),
                       0.06, DARK_PLATE, 0.0, numpy.array([0.0, 1.0, 0.0]), depth=0.05),
        ]

    return placements

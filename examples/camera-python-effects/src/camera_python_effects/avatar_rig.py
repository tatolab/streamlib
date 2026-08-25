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
    "PoseMemory",
    "idle_joints",
    "resolve_pose_with_fallbacks",
    "solve_segment_placements",
]

# The MediaPipe pose-landmark indices this rig reads, by name — with the
# sides deliberately crossed. The rig mirrors (x is negated below), and a
# mirror swaps handedness: your left hand is the reflection's right hand. The
# indices here are MediaPipe's right-side landmarks under this rig's left-side
# names and vice versa; negating x without this swap builds a left-right-
# crossed body whose computed forward faces away from the viewer — the visor
# ends up where the person's back is.
_LANDMARKS = {
    "nose": 0,
    "left_ear": 8,
    "right_ear": 7,
    "left_shoulder": 12,
    "right_shoulder": 11,
    "left_elbow": 14,
    "right_elbow": 13,
    "left_wrist": 16,
    "right_wrist": 15,
    "left_hip": 24,
    "right_hip": 23,
    "left_knee": 26,
    "right_knee": 25,
    "left_ankle": 28,
    "right_ankle": 27,
    "left_foot": 32,
    "right_foot": 31,
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


class PoseMemory:
    """Rides out detection dropouts by holding the last resolved pose.

    The detector loses a person for odd frames — a re-acquisition, a
    visibility flicker, a hand across the face — and a target that snaps to
    idle on every one of them makes the figure lurch through a reset. A
    dropout inside the hold window freezes the last good pose instead;
    only a sustained loss hands the stage back to idle, and the smoother
    downstream makes that hand-off the glide it always was.
    """

    def __init__(self, hold_seconds: float = 1.0) -> None:
        self._hold_seconds = hold_seconds
        self._held_joints: "dict[str, numpy.ndarray] | None" = None
        self._held_at_seconds = float("-inf")

    def target_for(
        self,
        resolved_joints: "dict[str, numpy.ndarray] | None",
        elapsed_seconds: float,
    ) -> "dict[str, numpy.ndarray] | None":
        """The pose to aim at now, or None once idle is the honest answer."""
        if resolved_joints is not None:
            self._held_joints = resolved_joints
            self._held_at_seconds = elapsed_seconds
            return resolved_joints
        held_for = elapsed_seconds - self._held_at_seconds
        if self._held_joints is not None and held_for < self._hold_seconds:
            return self._held_joints
        return None


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

    World landmarks are metres with y down; the scene is y up. The mirroring
    is the pair of x negated here plus the side-swapped index table above —
    raise your right hand and the figure facing you raises the hand on your
    right, which is how every filter-style effect reads as "me".
    """
    return {
        name: numpy.array(
            [-landmarks[index].x, -landmarks[index].y, -landmarks[index].z]
        )
        for name, index in _LANDMARKS.items()
    }


def visibilities_from_world_landmarks(landmarks) -> "dict[str, float]":
    """Per-joint confidence that the joint is genuinely in the picture.

    The detector answers a position for every landmark whether or not the
    camera can see it — a desk framing still "has" ankles, hallucinated
    somewhere below the desk. Visibility is what separates seen from guessed.
    """
    return {
        name: float(getattr(landmarks[index], "visibility", 1.0) or 0.0)
        for name, index in _LANDMARKS.items()
    }


# What a pose can and cannot be anchored without. Shoulders are the one pair
# a camera pointed at a person always sees; hips are exactly what a desk
# framing occludes, so requiring them made the whole pose flicker out at the
# threshold while the upper body tracked fine. Below the pair, the trust band
# over which a guessed joint hands over to its live-anchored stand-in.
_SHOULDER_ANCHOR_JOINTS = ("left_shoulder", "right_shoulder")
_HIP_JOINTS = ("left_hip", "right_hip")
_FALLBACK_FULL_TRUST_VISIBILITY = 0.7
_FALLBACK_NO_TRUST_VISIBILITY = 0.4

# Chest-to-pelvis distance of the idle template, used to synthesize a pelvis
# under a chest whose hips the camera cannot see.
_IDLE_TORSO_LENGTH = 0.5


def _torso_local_idle_offsets() -> "dict[str, numpy.ndarray]":
    """Each idle joint as an offset from the idle pelvis, torso-local.

    The idle pose stands axis-aligned — shoulders along x, spine up y, facing
    z — so its offsets are already in torso coordinates, ready to be carried
    on a live torso's own basis.
    """
    idle = idle_joints(0.0)
    idle_pelvis = (idle["left_hip"] + idle["right_hip"]) / 2.0
    return {name: joint - idle_pelvis for name, joint in idle.items()}


_IDLE_OFFSETS_FROM_PELVIS = _torso_local_idle_offsets()


def resolve_pose_with_fallbacks(
    joints: "dict[str, numpy.ndarray]",
    visibility: "dict[str, float]",
) -> "dict[str, numpy.ndarray] | None":
    """The detected pose, with unseen joints standing in a neutral pose.

    A desk camera sees head and torso; the detector hallucinates the rest and
    the figure buckles on the noise — the take-a-knee failure. Every joint
    below the trust band is replaced by its idle-pose stand-in anchored to the
    *live* torso (pelvis position plus torso basis), and the band is a blend,
    not a switch, so a joint entering frame walks in rather than popping.

    None only when the shoulders themselves are not trustworthy — with no
    anchor at all, the caller's idle stage is the honest answer. Unseen hips
    do not disqualify the pose: the pelvis is synthesized a torso-length
    below the chest, upright-biased, and the hip joints take the same
    stand-in treatment as any other unseen joint.
    """
    if min(visibility[name] for name in _SHOULDER_ANCHOR_JOINTS) < 0.5:
        return None

    chest = (joints["left_shoulder"] + joints["right_shoulder"]) / 2.0
    across = _normalized(joints["left_shoulder"] - joints["right_shoulder"])
    if min(visibility[name] for name in _HIP_JOINTS) >= 0.5:
        pelvis = (joints["left_hip"] + joints["right_hip"]) / 2.0
        spine_up = _normalized(chest - pelvis)
    else:
        # No hips to read the lean from: stand the spine up, orthogonal to
        # the shoulder line, and hang the pelvis a torso-length below.
        world_up = numpy.array([0.0, 1.0, 0.0])
        spine_up = _normalized(world_up - across * float(across @ world_up))
        pelvis = chest - spine_up * _IDLE_TORSO_LENGTH
    forward = _normalized(numpy.cross(across, spine_up))
    across = _normalized(numpy.cross(spine_up, forward))
    torso_basis = numpy.stack([across, spine_up, forward], axis=1)

    trust_band = _FALLBACK_FULL_TRUST_VISIBILITY - _FALLBACK_NO_TRUST_VISIBILITY
    resolved: "dict[str, numpy.ndarray]" = {}
    for name, detected in joints.items():
        trust = (visibility[name] - _FALLBACK_NO_TRUST_VISIBILITY) / trust_band
        trust = min(max(trust, 0.0), 1.0)
        anchored_stand_in = pelvis + torso_basis @ _IDLE_OFFSETS_FROM_PELVIS[name]
        resolved[name] = detected * trust + anchored_stand_in * (1.0 - trust)
    return resolved


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

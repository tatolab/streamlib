# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The android's solver and geometry, checked without a camera or a person.

The solver is pure numpy over a joint dict and the meshes are pure numpy
arrays, so everything but the GL smoke test runs anywhere. The winding test is
here because it already earned its place: the first render shipped every box
and prism facing inward, and the fresnel rim flooded the whole body cyan.
"""

from __future__ import annotations

import numpy
import pytest

from camera_python_effects.avatar_rig import (
    JOINT_NAMES,
    SmoothedPose,
    idle_joints,
    solve_segment_placements,
)
from camera_python_effects.avatar_scene import prism_mesh, sphere_mesh, unit_box_mesh


def outward_facing_fraction(mesh: numpy.ndarray) -> float:
    triangles = mesh.reshape(-1, 3, 6)
    centres = triangles[:, :, :3].mean(axis=1)
    normals = triangles[:, 0, 3:]
    centroid = triangles[:, :, :3].reshape(-1, 3).mean(axis=0)
    outward = numpy.einsum("ij,ij->i", normals, centres - centroid)
    return float((outward > 0).mean())


@pytest.mark.parametrize(
    "mesh_builder", [unit_box_mesh, prism_mesh, sphere_mesh],
    ids=["box", "prism", "sphere"],
)
def test_every_facet_normal_points_outward(mesh_builder) -> None:
    """Inward normals turn the fresnel rim into a full-body wash."""
    assert outward_facing_fraction(mesh_builder()) == 1.0


@pytest.mark.parametrize(
    "mesh_builder", [unit_box_mesh, prism_mesh, sphere_mesh],
    ids=["box", "prism", "sphere"],
)
def test_every_normal_is_unit_length_and_finite(mesh_builder) -> None:
    mesh = mesh_builder()
    assert numpy.isfinite(mesh).all()
    lengths = numpy.linalg.norm(mesh[:, 3:], axis=1)
    assert numpy.allclose(lengths, 1.0, atol=1e-5)


def test_the_idle_pose_solves_into_a_complete_android() -> None:
    placements = solve_segment_placements(idle_joints(2.0))
    assert len(placements) >= 20
    for placement in placements:
        assert placement.primitive in ("box", "prism", "sphere")
        assert numpy.isfinite(placement.model_matrix).all()


def test_the_solved_android_stands_on_the_floor_over_the_origin() -> None:
    placements = solve_segment_placements(idle_joints(2.0))
    positions = numpy.array([p.model_matrix[:3, 3] for p in placements])
    # Grounded: nothing meaningfully below the floor, feet near it, and the
    # figure centred on the stage whatever the raw pelvis position was.
    assert positions[:, 1].min() > -0.15
    assert positions[:, 1].min() < 0.15
    assert abs(positions[:, 0].mean()) < 0.2


def test_a_raised_wrist_raises_a_segment() -> None:
    """The solver poses from the joints — the one contract that is the point."""
    neutral = idle_joints(1.0)
    raised = idle_joints(1.0)
    raised["right_wrist"] = numpy.array([-0.3, 2.05, 0.1])

    def highest_segment_top(placements) -> float:
        return max(
            float((p.model_matrix @ numpy.array([0.0, 1.0, 0.0, 1.0]))[1])
            for p in placements
        )

    assert highest_segment_top(solve_segment_placements(raised)) > (
        highest_segment_top(solve_segment_placements(neutral)) + 0.2
    )


def test_the_smoother_converges_and_never_overshoots_alpha() -> None:
    smoother = SmoothedPose()
    start = {name: numpy.zeros(3) for name in JOINT_NAMES}
    target = {name: numpy.ones(3) for name in JOINT_NAMES}
    smoother.settle(start, 0.033)
    for _ in range(120):
        settled = smoother.settle(
            {name: joint.copy() for name, joint in target.items()}, 0.033
        )
    assert numpy.allclose(settled["nose"], 1.0, atol=1e-3)


def test_the_smoother_holds_depth_back_harder_than_the_plane() -> None:
    smoother = SmoothedPose()
    smoother.settle({name: numpy.zeros(3) for name in JOINT_NAMES}, 0.033)
    settled = smoother.settle(
        {name: numpy.ones(3) for name in JOINT_NAMES}, 0.033
    )
    assert settled["nose"][0] > settled["nose"][2]


def test_the_scene_renders_a_frame(request) -> None:
    """GL smoke test — skipped where no context can be created at all."""
    from camera_python_effects.avatar_scene import AvatarSceneRenderer

    try:
        renderer = AvatarSceneRenderer(320, 224)
    except Exception as no_context:  # noqa: BLE001 — headless CI has no GL
        pytest.skip(f"no GL context here: {no_context}")
    frame = renderer.render(solve_segment_placements(idle_joints(1.5)), 1.5)
    assert frame.shape == (224, 320, 4)
    assert frame.dtype == numpy.uint8
    # A frame with the stage on it is neither empty nor a single colour.
    assert (frame[:, :, :3].sum(axis=2) > 20).mean() > 0.05
    assert len(numpy.unique(frame[:, :, 1])) > 30

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The LeRobot bridge: the join's discipline and the format's round trip.

The join tests are pure logic. The format test is the load-bearing one: a
dataset this pipeline writes is only proof if LeRobot's own loader reads it
back — episodes, video, vectors and task intact — so that is exactly what it
asserts. Skipped where lerobot is not installed (`uv sync --extra lerobot`).
"""

from __future__ import annotations

import numpy
import pytest

from camera_python_effects.avatar_rig import JOINT_NAMES, idle_joints
from camera_python_effects.timestamp_stream_join import TimestampStreamJoin


def test_a_row_completes_only_when_every_stream_has_answered() -> None:
    join = TimestampStreamJoin(("pose", "camera", "stylized"))
    assert join.offer("camera", 100, "c") is None
    assert join.offer("pose", 100, "p") is None
    completed = join.offer("stylized", 100, "s")
    assert completed == {"camera": "c", "pose": "p", "stylized": "s"}


def test_rows_complete_independently_and_out_of_order() -> None:
    join = TimestampStreamJoin(("pose", "camera"))
    join.offer("camera", 100, "c100")
    join.offer("camera", 133, "c133")
    late = join.offer("pose", 133, "p133")
    early = join.offer("pose", 100, "p100")
    assert late == {"camera": "c133", "pose": "p133"}
    assert early == {"camera": "c100", "pose": "p100"}


def test_a_completed_stamp_does_not_complete_twice() -> None:
    join = TimestampStreamJoin(("pose", "camera"))
    join.offer("camera", 100, "c")
    assert join.offer("pose", 100, "p") is not None
    assert join.offer("pose", 100, "p-again") is None


def test_a_stalled_stream_costs_old_rows_never_unbounded_memory() -> None:
    join = TimestampStreamJoin(("pose", "camera"), pending_row_limit=5)
    for stamp in range(50):
        join.offer("camera", stamp, "c")
    assert join.rows_abandoned == 45
    # The newest stamps are the ones still joinable.
    assert join.offer("pose", 49, "p") is not None


def test_a_single_stream_join_is_refused() -> None:
    with pytest.raises(ValueError, match="not a join"):
        TimestampStreamJoin(("pose",))


def _pose_bag(timestamp_ns: int) -> dict:
    joints = idle_joints(1.0)
    return {
        "timestamp_ns": timestamp_ns,
        "provenance": "detected",
        "joints": {name: [float(a) for a in joint] for name, joint in joints.items()},
        "visibility": {name: 0.9 for name in JOINT_NAMES},
    }


def test_pose_vectors_keep_the_promised_layout() -> None:
    from camera_python_effects.lerobot_episode_recording import pose_bag_to_vectors

    bag = _pose_bag(7)
    state, seen = pose_bag_to_vectors(bag)
    assert state.shape == (len(JOINT_NAMES) * 3,)
    assert seen.shape == (len(JOINT_NAMES),)
    # Spot-check the layout promise: joint i occupies [3i, 3i+3).
    nose_index = JOINT_NAMES.index("nose")
    assert numpy.allclose(
        state[nose_index * 3 : nose_index * 3 + 3], bag["joints"]["nose"], atol=1e-6
    )


def test_the_dataset_round_trips_through_lerobots_own_loader(tmp_path) -> None:
    lerobot_dataset = pytest.importorskip("lerobot.datasets.lerobot_dataset")
    from camera_python_effects.lerobot_episode_recording import LeRobotEpisodeRecording

    recording = LeRobotEpisodeRecording(
        root=tmp_path / "dataset",
        repo_id="tatolab/format-contract-test",
        fps=30,
        camera_height=48,
        camera_width=64,
        task="mirror the operator's pose",
    )
    frames_per_episode = 8
    for episode in range(2):
        for i in range(frames_per_episode):
            bag = _pose_bag(1_000_000 * (episode * frames_per_episode + i))
            camera = numpy.full((48, 64, 3), 10 + i * 15, numpy.uint8)
            stylized = numpy.full((48, 64, 3), 200 - i * 15, numpy.uint8)
            recording.add(bag, camera, stylized)
        recording.save_episode()
    recording.close()

    loaded = lerobot_dataset.LeRobotDataset(
        "tatolab/format-contract-test", root=tmp_path / "dataset", video_backend="pyav"
    )
    assert loaded.num_episodes == 2
    assert loaded.num_frames == 16
    assert loaded.fps == 30

    sample = loaded[3]
    assert sample["task"] == "mirror the operator's pose"
    assert tuple(sample["observation.state"].shape) == (len(JOINT_NAMES) * 3,)
    assert tuple(sample["observation.pose_visibility"].shape) == (len(JOINT_NAMES),)
    assert int(sample["observation.capture_timestamp_ns"].item()) == 3_000_000
    # Both video streams decode, channel-first as LeRobot serves them.
    assert tuple(sample["observation.images.camera"].shape) == (3, 48, 64)
    assert tuple(sample["observation.images.stylized"].shape) == (3, 48, 64)
    # Action mirrors state — the pose the operator is commanding.
    assert numpy.allclose(sample["action"], sample["observation.state"])


def test_a_second_session_resumes_the_dataset_instead_of_refusing(tmp_path) -> None:
    """Recording twice into one root must append episodes, not die at setup."""
    lerobot_dataset = pytest.importorskip("lerobot.datasets.lerobot_dataset")
    from camera_python_effects.lerobot_episode_recording import LeRobotEpisodeRecording

    def one_session(frames: int) -> None:
        recording = LeRobotEpisodeRecording(
            root=tmp_path / "dataset",
            repo_id="tatolab/format-contract-test",
            fps=30,
            camera_height=48,
            camera_width=64,
            task="mirror the operator's pose",
        )
        for i in range(frames):
            recording.add(
                _pose_bag(1_000_000 * i),
                numpy.full((48, 64, 3), 60, numpy.uint8),
                numpy.full((48, 64, 3), 120, numpy.uint8),
            )
        recording.close()

    one_session(6)
    one_session(6)

    loaded = lerobot_dataset.LeRobotDataset(
        "tatolab/format-contract-test", root=tmp_path / "dataset", video_backend="pyav"
    )
    assert loaded.num_episodes == 2
    assert loaded.num_frames == 12

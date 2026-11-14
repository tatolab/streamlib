# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Records the camera, the composited picture and the solved pose into a LeRobotDataset.

Reads `video_from_camera`, `stylized_from_compositor` and `pose_from_avatar`,
all descended from one camera frame, joins them on that frame's
`timestamp_ns`, and writes episodes through LeRobot's own writer. Publishes
one record per saved episode on `episodes_to_downstream`.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import numpy
from tatolab.stream import (
    Image,
    ImageFrame,
    Input,
    Output,
    Records,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    log,
    node,
)

from ..elapsed_seconds_since_first_process import NANOSECONDS_PER_SECOND
from ..lerobot_episode_recording import LeRobotEpisodeRecording
from ..solved_pose_record import (
    SOLVED_POSE_RECORD_NAMES,
    solved_pose_record_from_records_message,
)
from ..timestamp_stream_join import TimestampStreamJoin

# 1080p decimated by four — 480x270, the frame size the dataset stores.
# Even dimensions, as video encoders require.
RECORDING_DECIMATION_STRIDE = 4
RECORDING_HEIGHT = 270
RECORDING_WIDTH = 480

POSE_STREAM = "pose"
CAMERA_STREAM = "camera"
STYLIZED_STREAM = "stylized"

EPISODE_RECORD_NAMES = ("episode", "frames_recorded", "seconds_encoding")


@dataclass
class LeRobotRecorderConfig:
    """LeRobotRecorder's settings, as its node's config in the graph document spells them."""

    dataset_root: str = "lerobot_dataset"
    repo_id: str = "tatolab/camera-python-effects-poses"
    fps: int = 30
    episode_seconds: float = 10.0
    task: str = "mirror the operator's pose"
    record_stylized: bool = True


@node.meta(description="Writes camera, stylized view and pose into a LeRobotDataset")
@node.process
class LeRobotRecorder:
    """Three timestamp-joined ports in, imitation-learning episodes out."""

    def __init__(self, config: LeRobotRecorderConfig) -> None:
        self.dataset_root = config.dataset_root
        self.repo_id = config.repo_id
        self.fps = config.fps
        self.frames_per_episode = max(int(config.episode_seconds * config.fps), 1)
        self.task = config.task
        self.record_stylized = config.record_stylized

    # `ordered` on every port: a recorder wants each frame, not the latest one.
    # A frame whose surface was recycled before this lagging reader reached it
    # is counted and skipped; a long episode-encode stall costs dropped rows,
    # never a wedged pipeline.
    video_from_camera = Input(Image, delivery="ordered", description="Frames from the camera")
    stylized_from_compositor = Input(
        Image, delivery="ordered", description="The composited picture"
    )
    pose_from_avatar = Input(
        Records,
        delivery="ordered",
        requires=SOLVED_POSE_RECORD_NAMES,
        description="The avatar's solved pose records",
    )
    episodes_to_downstream = Output(
        Records,
        names=EPISODE_RECORD_NAMES,
        description="One record per saved episode, for observability",
    )

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.recording = LeRobotEpisodeRecording(
            root=self.dataset_root,
            repo_id=self.repo_id,
            fps=self.fps,
            camera_height=RECORDING_HEIGHT,
            camera_width=RECORDING_WIDTH,
            task=self.task,
            record_stylized=self.record_stylized,
        )
        streams = (POSE_STREAM, CAMERA_STREAM) + (
            (STYLIZED_STREAM,) if self.record_stylized else ()
        )
        self.stream_join = TimestampStreamJoin(streams)
        self._completed_rows: list[dict[str, Any]] = []
        self.frames_recorded = 0
        self.frames_skipped_idle = 0
        self.frames_unreadable = 0

    def _frame_pixels(self, frame: ImageFrame) -> numpy.ndarray | None:
        """The frame decimated to recording size, on the CPU, or None.

        Read from the frame's host side and copied out, because the rows
        outlive the frame: a view would hold its pool slot until the episode
        saves. A frame whose surface was recycled before this lagging reader
        reached it is skipped and counted, never raised — recording must not
        be able to take the pipeline down.
        """
        try:
            return numpy.ascontiguousarray(
                numpy.from_dlpack(frame, device="cpu")[
                    ::RECORDING_DECIMATION_STRIDE, ::RECORDING_DECIMATION_STRIDE, :3
                ]
            )
        except Exception:  # noqa: BLE001 — the skip is the contract
            self.frames_unreadable += 1
            return None

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        self._drain_video_port(ctx, self.video_from_camera, CAMERA_STREAM)
        if self.record_stylized:
            self._drain_video_port(ctx, self.stylized_from_compositor, STYLIZED_STREAM)

        while (pose_message := ctx.inputs.read(self.pose_from_avatar)) is not None:
            solved_pose_record = solved_pose_record_from_records_message(pose_message)
            # Idle frames are the stage running without a person; a dataset
            # of them would teach a policy to stand still.
            if solved_pose_record["provenance"] == "idle":
                self.frames_skipped_idle += 1
                continue
            self._offer(
                POSE_STREAM, solved_pose_record["timestamp_ns"], solved_pose_record
            )

        self._flush_completed_rows(ctx)

    def _drain_video_port(
        self,
        ctx: RuntimeContextLimitedAccess,
        video_input: Input[ImageFrame],
        stream_name: str,
    ) -> None:
        """Offer every frame queued on `video_input` to the join as `stream_name`."""
        while (frame := ctx.inputs.read(video_input)) is not None:
            pixels = self._frame_pixels(frame)
            if pixels is not None:
                self._offer(stream_name, frame.timestamp_ns, pixels)

    def _offer(self, stream: str, timestamp_ns: int, value: Any) -> None:
        completed = self.stream_join.offer(stream, timestamp_ns, value)
        if completed is not None:
            self._completed_rows.append(completed)

    def _flush_completed_rows(self, ctx: RuntimeContextLimitedAccess) -> None:
        for row in self._completed_rows:
            self.recording.add(
                row[POSE_STREAM],
                row[CAMERA_STREAM],
                row.get(STYLIZED_STREAM),
            )
            self.frames_recorded += 1
            if self.recording.frames_in_current_episode >= self.frames_per_episode:
                self._save_episode(ctx)
        self._completed_rows = []

    def _save_episode(self, ctx: RuntimeContextLimitedAccess) -> None:
        saving_began = ctx.time
        self.recording.save_episode()
        seconds_encoding = (ctx.time - saving_began) / NANOSECONDS_PER_SECOND
        log.info(
            "episode saved",
            episode=self.recording.episodes_saved,
            frames_recorded=self.frames_recorded,
            rows_abandoned=self.stream_join.rows_abandoned,
            unreadable_frames=self.frames_unreadable,
            seconds_encoding=round(seconds_encoding, 2),
        )
        ctx.outputs.write(
            self.episodes_to_downstream,
            {
                "episode": self.recording.episodes_saved,
                "frames_recorded": self.frames_recorded,
                "seconds_encoding": seconds_encoding,
            },
        )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        self.recording.close()
        log.info(
            "dataset sealed",
            episodes=self.recording.episodes_saved,
            frames=self.frames_recorded,
            skipped_idle=self.frames_skipped_idle,
        )

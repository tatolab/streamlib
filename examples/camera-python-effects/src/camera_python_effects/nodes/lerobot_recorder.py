# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Records the camera, the composited picture and the solved pose into a LeRobotDataset.

Reads `video_from_camera`, `stylized_from_compositor` and `pose_from_avatar`,
all descended from one camera frame, joins them on that frame's
`timestamp_ns`, and writes episodes through LeRobot's own writer. Publishes
one `SavedEpisode` per saved episode on `episodes_to_downstream`.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated, Any, Callable

import numpy
from tatolab.stream import (
    Description,
    Input,
    InputPin,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    computed,
    log,
    node,
)

from ..elapsed_seconds_since_first_process import NANOSECONDS_PER_SECOND
from ..lerobot_episode_recording import LeRobotEpisodeRecording
from ..solved_pose_record import SolvedPose
from ..timestamp_stream_join import TimestampStreamJoin

# 1080p decimated by four — 480x270, the frame size the dataset stores.
# Even dimensions, as video encoders require.
RECORDING_DECIMATION_STRIDE = 4
RECORDING_HEIGHT = 270
RECORDING_WIDTH = 480

POSE_STREAM = "pose"
CAMERA_STREAM = "camera"
STYLIZED_STREAM = "stylized"


@dataclass(frozen=True)
class SavedEpisode:
    """What one saved episode holds, for observability."""

    episode: int
    frames_recorded: int
    seconds_encoding: float


def recording_pixels_of(frame: Texture) -> numpy.ndarray | None:
    """The frame decimated to recording size, on the CPU, or None.

    Read from the frame's host side and copied out, because the rows
    outlive the frame: a view would hold its pool slot until the episode
    saves. A frame whose surface was recycled before this lagging reader
    reached it is None, never raised — recording must not be able to take
    the pipeline down.
    """
    try:
        return numpy.ascontiguousarray(
            numpy.from_dlpack(frame, device="cpu")[
                ::RECORDING_DECIMATION_STRIDE, ::RECORDING_DECIMATION_STRIDE, :3
            ]
        )
    except Exception:  # noqa: BLE001 — the skip is the contract
        return None


@dataclass(frozen=True)
class RecordingSettings:
    """What a recording is opened with: every value input of `LeRobotRecorder`."""

    dataset_root: str
    repo_id: str
    fps: int
    episode_seconds: float
    task: str
    record_stylized: bool


class DatasetBeingRecorded:
    """One open dataset: the join of the three streams and the episodes written from it.

    Closing it seals the dataset.
    """

    def __init__(self, settings: RecordingSettings) -> None:
        self.record_stylized = settings.record_stylized
        self.frames_per_episode = max(int(settings.episode_seconds * settings.fps), 1)
        self.recording = LeRobotEpisodeRecording(
            root=settings.dataset_root,
            repo_id=settings.repo_id,
            fps=settings.fps,
            camera_height=RECORDING_HEIGHT,
            camera_width=RECORDING_WIDTH,
            task=settings.task,
            record_stylized=settings.record_stylized,
        )
        streams = (POSE_STREAM, CAMERA_STREAM) + (
            (STYLIZED_STREAM,) if settings.record_stylized else ()
        )
        self.stream_join = TimestampStreamJoin(streams)
        self._completed_rows: list[dict[str, Any]] = []
        self.frames_recorded = 0
        self.frames_skipped_idle = 0
        self.frames_unreadable = 0

    def drain_video_pin(self, video_input: "InputPin[Texture]", stream_name: str) -> None:
        """Offer every frame queued on `video_input` to the join as `stream_name`."""
        while (frame := video_input.read()) is not None:
            pixels = recording_pixels_of(frame)
            if pixels is None:
                self.frames_unreadable += 1
            else:
                self._offer(stream_name, frame.timestamp_ns, pixels)

    def drain_pose_pin(self, pose_input: "InputPin[SolvedPose]") -> None:
        """Offer every solved pose queued on `pose_input` to the join, but an idle one."""
        while (solved_pose := pose_input.read()) is not None:
            solved_pose_record = solved_pose.record
            # Idle frames are the stage running without a person; a dataset
            # of them would teach a policy to stand still.
            if solved_pose_record["provenance"] == "idle":
                self.frames_skipped_idle += 1
                continue
            self._offer(
                POSE_STREAM, solved_pose_record["timestamp_ns"], solved_pose_record
            )

    def _offer(self, stream: str, timestamp_ns: int, value: Any) -> None:
        completed = self.stream_join.offer(stream, timestamp_ns, value)
        if completed is not None:
            self._completed_rows.append(completed)

    def flush_completed_rows(
        self,
        ctx: RuntimeContextLimitedAccess,
        episodes_to_downstream: "OutputPin[SavedEpisode]",
    ) -> None:
        """Write each joined row, saving an episode, and writing it to `episodes_to_downstream`,
        each time one fills."""
        for row in self._completed_rows:
            self.recording.add(
                row[POSE_STREAM],
                row[CAMERA_STREAM],
                row.get(STYLIZED_STREAM),
            )
            self.frames_recorded += 1
            if self.recording.frames_in_current_episode >= self.frames_per_episode:
                episodes_to_downstream.write(self._save_episode(ctx))
        self._completed_rows = []

    def _save_episode(self, ctx: RuntimeContextLimitedAccess) -> SavedEpisode:
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
        return SavedEpisode(
            episode=self.recording.episodes_saved,
            frames_recorded=self.frames_recorded,
            seconds_encoding=seconds_encoding,
        )

    def close(self) -> None:
        self.recording.close()
        log.info(
            "dataset sealed",
            episodes=self.recording.episodes_saved,
            frames=self.frames_recorded,
            skipped_idle=self.frames_skipped_idle,
        )


@node.meta(description="Writes camera, stylized view and pose into a LeRobotDataset")
@node(execution="reactive")
def LeRobotRecorder(
    *,
    dataset_root: Input[
        Annotated[str, Description("The directory the dataset is written under.")]
    ] = Input("lerobot_dataset"),
    repo_id: Input[
        Annotated[str, Description("The dataset's name, as LeRobot's hub spells it.")]
    ] = Input("tatolab/camera-python-effects-poses"),
    fps: Input[
        Annotated[int, NumberRange(minimum=1), Description("The frame rate the dataset records at.")]
    ] = Input(30),
    episode_seconds: Input[
        Annotated[
            float,
            NumberRange(minimum=0.1),
            Description("How long each episode runs before it is saved."),
        ]
    ] = Input(10.0),
    task: Input[
        Annotated[str, Description("The task every episode is labelled with.")]
    ] = Input("mirror the operator's pose"),
    record_stylized: Input[
        Annotated[bool, Description("Record the composited picture beside the camera's.")]
    ] = Input(True),
    # `ordered` on every pin: a recorder wants each frame, not the latest one.
    # A frame whose surface was recycled before this lagging reader reached it
    # is counted and skipped; a long episode-encode stall costs dropped rows,
    # never a wedged pipeline.
    video_from_camera: InputPin[
        Annotated[Texture, StreamDelivery("ordered"), Description("Frames from the camera")]
    ],
    stylized_from_compositor: InputPin[
        Annotated[Texture, StreamDelivery("ordered"), Description("The composited picture")]
    ],
    pose_from_avatar: InputPin[
        Annotated[SolvedPose, StreamDelivery("ordered"), Description("The avatar's solved poses")]
    ],
    episodes_to_downstream: OutputPin[
        Annotated[SavedEpisode, Description("One per saved episode, for observability")]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Three timestamp-joined pins in, imitation-learning episodes out.

    The dataset is opened from the value inputs, and opened again, the one before sealed, when
    one of them changes.
    """
    dataset = computed(
        lambda: DatasetBeingRecorded(
            RecordingSettings(
                dataset_root=dataset_root.get(),
                repo_id=repo_id.get(),
                fps=fps.get(),
                episode_seconds=episode_seconds.get(),
                task=task.get(),
                record_stylized=record_stylized.get(),
            )
        )
    )

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        recording = dataset.get()
        recording.drain_video_pin(video_from_camera, CAMERA_STREAM)
        if recording.record_stylized:
            recording.drain_video_pin(stylized_from_compositor, STYLIZED_STREAM)
        recording.drain_pose_pin(pose_from_avatar)
        recording.flush_completed_rows(ctx, episodes_to_downstream)

    return each_frame

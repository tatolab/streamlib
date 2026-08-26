# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Records the pipeline into a LeRobotDataset, as one more helper process.

The physical-AI claim, made concrete: the same graph that entertains is a
data-collection rig. This processor taps three streams that all descend from
one camera frame — the raw view, the composited output, and the solved pose —
joins them on the frame's own timestamp, and writes imitation-learning
episodes through LeRobot's official writer.

Isolation is what makes it safe to bolt on: episode saves video-encode for
whole seconds, and those seconds stall exactly one process — this one. The
camera keeps capturing, the effects keep running, the window stays at vsync,
and the recorder catches back up on the frames its channels held.
"""

from __future__ import annotations

import cupy
import numpy

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    log,
    output,
    processor,
)

from ..lerobot_episode_recording import LeRobotEpisodeRecording
from ..timestamp_stream_join import TimestampStreamJoin

# 1080p decimated by four — 480x270, the frame size the dataset stores.
# Even dimensions, as video encoders require.
RECORDING_DECIMATION_STRIDE = 4
RECORDING_HEIGHT = 270
RECORDING_WIDTH = 480

POSE_STREAM = "pose"
CAMERA_STREAM = "camera"
STYLIZED_STREAM = "stylized"


@processor(description="Writes camera, stylized view and pose into a LeRobotDataset")
class LeRobotRecorder:
    """Three timestamp-joined streams in, imitation-learning episodes out."""

    def __init__(
        self,
        dataset_root: str,
        repo_id: str = "tatolab/camera-python-effects-poses",
        fps: int = 30,
        episode_seconds: float = 10.0,
        task: str = "mirror the operator's pose",
        record_stylized: bool = True,
    ) -> None:
        self.dataset_root = dataset_root
        self.repo_id = repo_id
        self.fps = fps
        self.frames_per_episode = max(int(episode_seconds * fps), 1)
        self.task = task
        self.record_stylized = record_stylized

    # `latest`, not `every_sample`, and not by preference: a channel's one
    # publisher shares a single ring config across subscribers, and both of
    # these channels already feed `latest` consumers (the effects chain, the
    # window). At camera cadence the drain below still catches essentially
    # every frame; what a long episode-encode stall costs is dropped rows,
    # counted, never a wedged pipeline.
    @input(delivery_profile="latest")
    def video_from_camera(self) -> VideoFrame: ...

    @input(delivery_profile="latest")
    def stylized_from_compositor(self) -> VideoFrame: ...

    @input(delivery_profile="every_sample")
    def pose_from_avatar(self) -> None: ...

    @output(description="One bag per saved episode, for observability")
    def episodes_to_downstream(self) -> None: ...

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
        self._completed_rows: "list[dict]" = []
        self.frames_recorded = 0
        self.frames_skipped_idle = 0
        self.frames_unreadable = 0

    def _frame_pixels(self, frame: VideoFrame) -> "numpy.ndarray | None":
        """The frame decimated to recording size, on the CPU, or None.

        GPU-side decimation first, so the device-to-host hop carries a
        twelfth of the bytes. A frame whose surface was recycled before this
        lagging consumer reached it is skipped and counted, never raised —
        recording must not be able to take the pipeline down.
        """
        try:
            device_pixels = cupy.from_dlpack(frame)
            return cupy.asnumpy(
                device_pixels[
                    ::RECORDING_DECIMATION_STRIDE, ::RECORDING_DECIMATION_STRIDE, :3
                ]
            )
        except Exception:  # noqa: BLE001 — the skip is the contract
            self.frames_unreadable += 1
            return None

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        while ctx.inputs.has_data("video_from_camera"):
            frame = ctx.inputs.read("video_from_camera", into=VideoFrame)
            if frame is None:
                break
            pixels = self._frame_pixels(frame)
            if pixels is not None:
                self._offer(CAMERA_STREAM, frame.timestamp_ns, pixels)

        if self.record_stylized:
            while ctx.inputs.has_data("stylized_from_compositor"):
                frame = ctx.inputs.read("stylized_from_compositor", into=VideoFrame)
                if frame is None:
                    break
                pixels = self._frame_pixels(frame)
                if pixels is not None:
                    self._offer(STYLIZED_STREAM, frame.timestamp_ns, pixels)

        while ctx.inputs.has_data("pose_from_avatar"):
            pose_bag = ctx.inputs.read("pose_from_avatar")
            if pose_bag is None:
                break
            # Idle frames are the stage running without a person; a dataset
            # of them would teach a policy to stand still.
            if pose_bag.get("provenance") == "idle":
                self.frames_skipped_idle += 1
                continue
            self._offer(POSE_STREAM, int(pose_bag["timestamp_ns"]), pose_bag)

        self._flush_completed_rows(ctx)

    def _offer(self, stream: str, timestamp_ns: int, value) -> None:
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
        seconds_encoding = (ctx.time - saving_began) / 1_000_000_000
        log.info(
            "episode saved",
            episode=self.recording.episodes_saved,
            frames_recorded=self.frames_recorded,
            rows_abandoned=self.stream_join.rows_abandoned,
            unreadable_frames=self.frames_unreadable,
            seconds_encoding=round(seconds_encoding, 2),
        )
        ctx.outputs.write(
            "episodes_to_downstream",
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

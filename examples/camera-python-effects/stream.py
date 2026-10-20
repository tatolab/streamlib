# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Tatolab stream: camera → six Python nodes → H.264, shared on this machine.

`tatolab dev` loads the one `@stream` below by convention — there is no
manifest. `CameraSource` and `H264Encoder` are native built-ins; everything
between them is this project's own Python, in the `camera_python_effects`
package, and each of those nodes runs in its own child process with its own
interpreter.

    CameraSource ─┬─ CameraFrameToTexture ─ CyberpunkGlitch ─ CrtFilmGrain ─┐
                  │                                                         ▼
                  └─ CyberpunkAvatar ──────────────────────▶ BreakingNewsCompositor ─ H264Encoder
                                                                            ▲
                                          NeonOverlaySource ────────────────┘
"""

from tatolab.stream import (
    CameraSource,
    CameraSourceConfig,
    Exposure,
    H264Encoder,
    StreamBuilder,
    stream,
)

from camera_python_effects.nodes.breaking_news_compositor import (
    BreakingNewsCompositor,
)
from camera_python_effects.nodes.camera_frame_to_texture import CameraFrameToTexture
from camera_python_effects.nodes.crt_film_grain import CrtFilmGrain
from camera_python_effects.nodes.cyberpunk_avatar import CyberpunkAvatar
from camera_python_effects.nodes.cyberpunk_glitch import CyberpunkGlitch
from camera_python_effects.nodes.lerobot_recorder import LeRobotRecorder
from camera_python_effects.nodes.neon_overlay_source import NeonOverlaySource

# The capture device: a V4L2 path (`/dev/video2`) on Linux, an AVFoundation
# unique ID on macOS. `None` opens the first capture device found.
CAMERA_DEVICE_ID: str | None = None

# A directory to record LeRobot episodes into — `uv sync --extra lerobot` first.
# `None` leaves the recorder out of the graph.
LEROBOT_DATASET_ROOT: str | None = None

FRAME_WIDTH = 1920
FRAME_HEIGHT = 1080


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The camera through the cyberpunk effects and the avatar, composited, encoded and exposed."""
    camera_config: CameraSourceConfig = {
        "max_width": FRAME_WIDTH,
        "max_height": FRAME_HEIGHT,
    }
    if CAMERA_DEVICE_ID is not None:
        camera_config["device_id"] = CAMERA_DEVICE_ID
    camera = stream_builder.add(CameraSource, config=camera_config)
    camera_texture = stream_builder.add(CameraFrameToTexture)
    glitch = stream_builder.add(CyberpunkGlitch)
    crt = stream_builder.add(CrtFilmGrain)
    avatar = stream_builder.add(CyberpunkAvatar)
    overlay = stream_builder.add(
        NeonOverlaySource, config={"width": FRAME_WIDTH, "height": FRAME_HEIGHT}
    )
    compositor = stream_builder.add(BreakingNewsCompositor)
    encoder = stream_builder.add(H264Encoder)

    stream_builder.connect(
        camera.output("video"), camera_texture.input("video_from_camera")
    )
    stream_builder.connect(
        camera_texture.output("video_to_downstream"),
        glitch.input("video_from_upstream"),
    )
    stream_builder.connect(
        glitch.output("video_to_downstream"), crt.input("video_from_upstream")
    )
    stream_builder.connect(
        crt.output("video_to_downstream"), compositor.input("video_from_upstream")
    )

    # Detection reads the camera directly rather than the effect chain: the
    # android should follow the person, not the grade applied over them.
    stream_builder.connect(camera.output("video"), avatar.input("video_from_camera"))
    stream_builder.connect(
        avatar.output("scene_to_downstream"),
        compositor.input("avatar_from_pose_scene"),
    )
    stream_builder.connect(
        overlay.output("overlay_to_downstream"),
        compositor.input("overlay_from_neon_source"),
    )

    stream_builder.connect(
        compositor.output("video_to_downstream"), encoder.input("video")
    )
    stream_builder.expose(encoder.output("encoded_video"), Exposure.PRIVATE)

    # The same graph doubles as a LeRobot data-collection rig: raw view,
    # stylized view and the solved pose, timestamp-joined into episodes,
    # written by LeRobot's own writer in one more child process.
    if LEROBOT_DATASET_ROOT is not None:
        recorder = stream_builder.add(
            LeRobotRecorder, config={"dataset_root": LEROBOT_DATASET_ROOT}
        )
        stream_builder.connect(
            camera.output("video"), recorder.input("video_from_camera")
        )
        stream_builder.connect(
            avatar.output("pose_to_downstream"), recorder.input("pose_from_avatar")
        )
        stream_builder.connect(
            compositor.output("video_to_downstream"),
            recorder.input("stylized_from_compositor"),
        )

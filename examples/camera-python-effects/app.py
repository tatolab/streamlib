# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A StreamLib app: camera → six Python processors → window.

`streamlib run` finds `setup(rt)` below by convention — there is no manifest and
no `main()`. `CameraSource` and `DisplayWindow` are native built-ins that ship
inside the wheel; everything between them is this app's own Python, and each of
those six runs in its own child process with its own interpreter.

    CameraSource ─┬─ CameraFrameToTexture ─ CyberpunkGlitch ─ CrtFilmGrain ─┐
                  │                                                         ▼
                  └─ PoseSkeletonOverlay ──────────────────▶ BreakingNewsCompositor ─ DisplayWindow
                                                                            ▲
                                          NeonOverlaySource ────────────────┘
"""

import os

from camera_python_effects.processors.breaking_news_compositor import (
    BreakingNewsCompositor,
)
from camera_python_effects.processors.camera_frame_to_texture import (
    CameraFrameToTexture,
)
from camera_python_effects.processors.crt_film_grain import CrtFilmGrain
from camera_python_effects.processors.cyberpunk_glitch import CyberpunkGlitch
from camera_python_effects.processors.neon_overlay_source import NeonOverlaySource
from camera_python_effects.processors.pose_skeleton_overlay import PoseSkeletonOverlay

from streamlib import CameraSource, DisplayWindow, Runtime

FRAME_WIDTH = 1920
FRAME_HEIGHT = 1080


def setup(rt: Runtime) -> None:
    camera_configuration: dict[str, object] = {
        "max_width": FRAME_WIDTH,
        "max_height": FRAME_HEIGHT,
    }
    # Unset means "the first capture device the engine finds"; set it to point
    # this app at a particular node, a vivid virtual camera included.
    requested_camera_device = os.environ.get("STREAMLIB_CAMERA_DEVICE")
    if requested_camera_device:
        camera_configuration["device_id"] = requested_camera_device

    camera = rt.add(CameraSource, config=camera_configuration)
    camera_texture = rt.add(CameraFrameToTexture)
    glitch = rt.add(CyberpunkGlitch)
    crt = rt.add(CrtFilmGrain)
    pose = rt.add(PoseSkeletonOverlay)
    overlay = rt.add(
        NeonOverlaySource, config={"width": FRAME_WIDTH, "height": FRAME_HEIGHT}
    )
    compositor = rt.add(BreakingNewsCompositor)
    window = rt.add(
        DisplayWindow,
        config={
            "title": "StreamLib Camera Python Effects",
            "width": FRAME_WIDTH,
            "height": FRAME_HEIGHT,
            "scaling": "fit",
        },
    )

    rt.connect(camera.output("video"), camera_texture.input("video_from_camera"))
    rt.connect(
        camera_texture.output("video_to_downstream"),
        glitch.input("video_from_upstream"),
    )
    rt.connect(
        glitch.output("video_to_downstream"), crt.input("video_from_upstream")
    )
    rt.connect(
        crt.output("video_to_downstream"),
        compositor.input("video_from_upstream"),
    )

    # Detection reads the camera directly rather than the effect chain: the
    # skeleton should follow the person, not the grade applied over them.
    rt.connect(camera.output("video"), pose.input("video_from_camera"))
    rt.connect(
        pose.output("skeleton_to_downstream"),
        compositor.input("pose_from_skeleton_overlay"),
    )
    rt.connect(
        overlay.output("overlay_to_downstream"),
        compositor.input("overlay_from_neon_source"),
    )

    rt.connect(compositor.output("video_to_downstream"), window.input("video"))

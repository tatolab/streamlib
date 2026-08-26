# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What every processor here agrees on about surfaces, shaders, and bags."""

from __future__ import annotations

from importlib import resources
from typing import Any

__all__ = [
    "COLOR_TARGET_TEXTURE_USAGE",
    "SAMPLED_ONLY_TEXTURE_USAGE",
    "TEXTURE_FORMAT",
    "read_shader_source",
    "video_frame_bag_naming",
]

# One format across the whole chain, so no pass has to convert.
TEXTURE_FORMAT = "rgba8_unorm"

# A texture a graphics pass renders into and the next pass samples. `copy_src`
# and `copy_dst` ride every acquire, so the CPU doors need no spelling here.
COLOR_TARGET_TEXTURE_USAGE = ["render_attachment", "texture_binding"]

# A texture written through a CPU or device-tensor door rather than a pass.
SAMPLED_ONLY_TEXTURE_USAGE = ["texture_binding"]


def read_shader_source(shader_file_name: str) -> str:
    """The GLSL shipped beside this package, as the engine's compiler takes it."""
    return (
        resources.files(__package__)
        .joinpath("shaders", shader_file_name)
        .read_text(encoding="utf-8")
    )


def video_frame_bag_naming(
    surface_id: str,
    width: int,
    height: int,
    timestamp_ns: int,
) -> "dict[str, Any]":
    """The bag a downstream `read(port, into=VideoFrame)` casts.

    The timestamp is carried from the frame this one was derived from rather
    than re-read off the clock: it is the ordering primitive for everything
    downstream, and a pass restamping it would date the picture to when the
    effect ran instead of when the camera captured it.
    """
    return {
        "surface_id": surface_id,
        "width": width,
        "height": height,
        "timestamp_ns": timestamp_ns,
    }

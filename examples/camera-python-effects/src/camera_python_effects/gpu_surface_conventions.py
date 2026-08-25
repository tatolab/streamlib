# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What every processor here agrees on about surfaces, shaders, and bags."""

from __future__ import annotations

from collections import deque
from importlib import resources
from typing import Any

from streamlib import GpuSurfaceHandle

__all__ = [
    "COLOR_TARGET_TEXTURE_USAGE",
    "SAMPLED_ONLY_TEXTURE_USAGE",
    "TEXTURE_FORMAT",
    "RecentlyPublishedSurfaceRing",
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

# How many published frames a producer keeps resolvable behind the newest one.
# Deep rings do not scale: a bucket is keyed by extent, format and usage, the
# engine caps one at 16 textures, and three passes publish full-extent colour
# targets here — so three retained each plus one in flight each is 12, with
# headroom. Three frames is ~100 ms at camera cadence, comfortably longer than
# a consumer reading `latest` takes to wake. Raising it means raising the cap.
PUBLISHED_SURFACE_RING_DEPTH = 3


class RecentlyPublishedSurfaceRing:
    """Holds the last few published surfaces so consumers can still resolve them.

    An acquired texture's registration *is* its handle: dropping the handle
    releases the pool slot and unregisters the surface id, so a consumer one
    process away that was handed the id a millisecond ago resolves nothing and
    its draw is refused by name. Publishing therefore means handing the id
    downstream *and* keeping the handle for a while.

    Depth bounds how far behind a consumer may fall, not how fast anything
    runs: the producer never waits, and a consumer that falls further behind
    than the ring misses frames rather than stalling it.
    """

    def __init__(self, depth: int = PUBLISHED_SURFACE_RING_DEPTH) -> None:
        # Eviction is the release: the oldest handle leaves the deque, its last
        # reference goes with it, and the wheel pays the parent its release.
        self._recently_published: "deque[GpuSurfaceHandle]" = deque(maxlen=depth)

    def retain_published_surface(self, surface: GpuSurfaceHandle) -> None:
        self._recently_published.append(surface)


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

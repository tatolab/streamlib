# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What every node here agrees on about surfaces and shaders."""

from __future__ import annotations

from importlib import resources

import numpy
from tatolab.stream import GpuSurfaceHandle

__all__ = [
    "COLOR_TARGET_TEXTURE_USAGE",
    "SAMPLED_ONLY_TEXTURE_USAGE",
    "TEXTURE_FORMAT",
    "copy_host_pixels_into_ring_texture",
    "read_shader_source",
]

# One format across the whole chain, so no pass has to convert.
TEXTURE_FORMAT = "rgba8_unorm"

# A texture a graphics pass renders into and the next pass samples. `copy_src`
# and `copy_dst` ride every acquire, so the CPU doors need no spelling here.
COLOR_TARGET_TEXTURE_USAGE = ["render_attachment", "texture_binding"]

# A texture written through a CPU door or the engine's copy rather than a pass.
SAMPLED_ONLY_TEXTURE_USAGE = ["texture_binding"]


def read_shader_source(shader_file_name: str) -> str:
    """The GLSL shipped beside this package, as the engine's compiler takes it."""
    return (
        resources.files(__package__)
        .joinpath("shaders", shader_file_name)
        .read_text(encoding="utf-8")
    )


def copy_host_pixels_into_ring_texture(
    ring_texture: GpuSurfaceHandle, host_pixels: numpy.ndarray
) -> None:
    """Write `host_pixels` through `ring_texture`'s CPU door, whole frame at once."""
    # `unlock` in a `finally` rather than a `with`: the handle's context manager
    # closes the surface on the way out, and the ring owns this slot for the
    # node's life.
    ring_texture.lock(read_only=False)
    try:
        ring_texture.as_numpy()[...] = host_pixels
    finally:
        ring_texture.unlock()

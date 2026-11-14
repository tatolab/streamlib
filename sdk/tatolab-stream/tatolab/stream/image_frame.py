# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The frame an Image port's read returns: a borrowed GPU surface with its size, format and colour.

Built from the message's Image descriptor. It holds its
surface against producer reuse while it lives and speaks `__dlpack__`, with
`writable()` the GPU write door and `cpu()` the CPU one, all from
`ClaimedSurfacePixelAccess`.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from .claimed_surface_pixel_access import ClaimedSurfacePixelAccess
from .color_info import ColorInfo

if TYPE_CHECKING:
    from ._gpu_protocols import GpuSurfaceHandle

__all__ = ["ImageFrame", "ImageFrameToWrite"]


@dataclass(frozen=True, init=False)
class ImageFrame(ClaimedSurfacePixelAccess):
    """An Image frame, as `ctx.inputs.read(self.video)` returns it.

    `surface_id` names the pooled GPU surface, `format` is its texture format's
    wire name such as `rgba16_float`, and `colour` its H.273 description.
    `ctx.outputs.write(self.video_out, frame)` publishes it again.
    """

    surface_id: str
    width: int
    height: int
    format: str
    colour: ColorInfo
    timestamp_ns: int

    def __init__(
        self,
        surface_id: str,
        width: int,
        height: int,
        format: str,
        colour: ColorInfo,
        timestamp_ns: int,
    ) -> None:
        """Hold the fields a read's descriptor states, then claim the surface."""
        super().__init__(
            surface_id=surface_id,
            width=width,
            height=height,
            format=format,
            colour=colour,
            timestamp_ns=timestamp_ns,
        )


@dataclass(frozen=True)
class ImageFrameToWrite:
    """An Image frame a node publishes from a texture it acquired from a pool.

    `ctx.outputs.write(self.video_out, frame_to_write)` publishes the texture's
    slot with this extent, format and colour; the write's own `timestamp_ns`
    stamps it.
    """

    surface_id: str
    width: int
    height: int
    format: str
    colour: ColorInfo

    @classmethod
    def of_texture(cls, texture: "GpuSurfaceHandle", colour: ColorInfo) -> "ImageFrameToWrite":
        """The frame `texture` holds, described by `colour`."""
        return cls(
            surface_id=texture.surface_id,
            width=texture.width,
            height=texture.height,
            format=texture.format,
            colour=colour,
        )

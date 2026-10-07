# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Where a wheel kernel lands the frame it samples.

A pixel buffer cannot bind as a sampled texture, so each frame is copied by
the engine into a pooled `rgba8_unorm` texture the kernel binds as
`streamlib_source`.
"""

from __future__ import annotations

from typing import Union

from ._gpu_protocols import (
    GpuContextFullAccess,
    GpuContextLimitedAccess,
    GpuSurfaceHandle,
)
from .node_output_texture_ring import NodeOutputTextureRing

GpuContextWithSurfaceCopy = Union[GpuContextLimitedAccess, GpuContextFullAccess]

SAMPLED_SOURCE_BINDING_NAME = "streamlib_source"
SAMPLED_SOURCE_TEXTURE_FORMAT = "rgba8_unorm"


class SampledSourceLandingTextureRing:
    """The texture each frame lands in before a kernel samples it, one pooled slot per frame."""

    def __init__(self) -> None:
        self._landing_ring = NodeOutputTextureRing(
            SAMPLED_SOURCE_TEXTURE_FORMAT, ["texture_binding"], depth=1
        )

    def land_source_for_this_frame(
        self,
        gpu_limited_access: GpuContextWithSurfaceCopy,
        source_surface_id: str,
        width: int,
        height: int,
        refusal_subject: str,
    ) -> GpuSurfaceHandle:
        """Copy the source into this frame's landing texture and return it.

        Raises `ValueError` opening with `refusal_subject` when the engine
        copy refuses the source.
        """
        landing_texture = self._landing_ring.next_texture_for_this_frame(
            gpu_limited_access, width, height
        )
        try:
            gpu_limited_access.copy_surface_to_surface(source_surface_id, landing_texture)
        except RuntimeError as copy_refusal:
            raise ValueError(
                f"{refusal_subject} ({width}x{height}) could not land in the kernel's "
                f"{SAMPLED_SOURCE_TEXTURE_FORMAT} source; the engine copy refused it: "
                f"{copy_refusal}"
            ) from copy_refusal
        return landing_texture

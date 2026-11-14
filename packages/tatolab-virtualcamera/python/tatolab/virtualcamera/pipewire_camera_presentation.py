# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The PipeWire door's per-frame path: each frame converted to YUYV on the GPU, then copied once into the buffer PipeWire hands the camera."""

from __future__ import annotations

from typing import Literal

from tatolab.stream import GpuSurfaceHandle, RuntimeContextLimitedAccess, ImageFrame

from ._native import PipeWireCameraDoor
from .v4l2_color_fields import yuyv_picture_color_of

PipeWireCameraFramePresentation = Literal[
    "published", "no_consumer_is_watching", "every_buffer_is_held_by_the_consumer"
]


class PipeWireCameraPresentation:
    """One PipeWire camera's offered extent and the YUYV pixel buffer each frame is converted into."""

    def __init__(self, pipewire_camera_door: PipeWireCameraDoor) -> None:
        self._pipewire_camera_door = pipewire_camera_door
        self._offered_extent: "tuple[int, int] | None" = None
        self._yuyv_pixel_buffer: "GpuSurfaceHandle | None" = None

    @property
    def offered_extent(self) -> "tuple[int, int] | None":
        """The extent the camera offers, once a frame has set one."""
        return self._offered_extent

    def failure_that_ended_the_camera(self) -> "str | None":
        """Why PipeWire dropped the camera node, or `None` while it lives."""
        return self._pipewire_camera_door.failure_that_ended_the_camera

    def offer_the_extent_of(
        self, ctx: RuntimeContextLimitedAccess, frame: ImageFrame
    ) -> bool:
        """Offer the frame's extent where it differs from the one offered; `True` when it did."""
        extent = (frame.width, frame.height)
        if self._offered_extent == extent:
            return False
        self._close_the_yuyv_pixel_buffer()
        self._offered_extent = None
        self._pipewire_camera_door.offer_extent(frame.width, frame.height)
        self._yuyv_pixel_buffer = ctx.gpu_limited_access.acquire_pixel_buffer(
            frame.width, frame.height, "yuyv"
        )
        self._offered_extent = extent
        return True

    def present(
        self, ctx: RuntimeContextLimitedAccess, frame: ImageFrame
    ) -> PipeWireCameraFramePresentation:
        """Convert the frame into the held YUYV pixel buffer and hand its rows to the camera."""
        yuyv_pixel_buffer = self._yuyv_pixel_buffer
        if yuyv_pixel_buffer is None:
            raise RuntimeError("no YUYV pixel buffer is held for the offered extent")
        ctx.gpu_limited_access.convert_surface_into(
            frame.surface_id, yuyv_pixel_buffer, yuyv_picture_color_of(frame.colour)
        )
        yuyv_pixel_buffer.lock(read_only=True)
        try:
            pixel_buffer_address = yuyv_pixel_buffer.base_address
            if pixel_buffer_address is None:
                raise RuntimeError("the YUYV pixel buffer opened no host mapping")
            bytes_per_row = yuyv_pixel_buffer.bytes_per_row
            return self._pipewire_camera_door.write_yuyv_frame(
                pixel_buffer_address,
                bytes_per_row * frame.height,
                bytes_per_row,
                frame.timestamp_ns,
            )
        finally:
            yuyv_pixel_buffer.unlock()

    def close(self) -> None:
        """Release the pixel buffer and remove the camera node."""
        self._close_the_yuyv_pixel_buffer()
        self._pipewire_camera_door.close()

    def _close_the_yuyv_pixel_buffer(self) -> None:
        if self._yuyv_pixel_buffer is not None:
            self._yuyv_pixel_buffer.close()
            self._yuyv_pixel_buffer = None

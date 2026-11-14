# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The native half of `tatolab-virtualcamera`, as the wheel's Python sees it.

Hand-written and gated against the compiled module by stubtest.
"""

from typing import Literal, final

__all__ = ["PipeWireCameraDoor"]

@final
class PipeWireCameraDoor:
    """One node's PipeWire camera: a `Video/Source` node named for the camera,
    alive until it is closed."""

    def __new__(
        cls, module_host_table: object, camera_name: str
    ) -> PipeWireCameraDoor: ...
    @property
    def camera_name(self) -> str: ...
    @property
    def failure_that_ended_the_camera(self) -> str | None: ...
    def offer_extent(
        self, width: int, height: int, frames_per_second: int | None = None
    ) -> None: ...
    def write_yuyv_frame(
        self,
        rows_address: int,
        rows_byte_count: int,
        stride_bytes: int,
        timestamp_ns: int,
    ) -> Literal[
        "published", "no_consumer_is_watching", "every_buffer_is_held_by_the_consumer"
    ]: ...
    def close(self) -> None: ...

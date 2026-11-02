# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native half of `tatolab-camera`, as the wheel's Python sees it."""

from typing import final

__all__ = [
    "V4l2ZeroCopyCameraCapture",
    "list_v4l2_capture_devices",
    "monotonic_now_ns",
]

@final
class V4l2ZeroCopyCameraCapture:
    """A camera publishing `VideoFrame`s on a lent output port from a thread this wheel owns."""

    def __new__(
        cls,
        module_host_table: object,
        video_output_port: str,
        device_path: str | None,
        max_width: int,
        max_height: int,
        output_pool_rotation_depth: int,
    ) -> V4l2ZeroCopyCameraCapture:
        """Open the camera, adopt its buffers and start publishing on `video_output_port`.

        `module_host_table` is what `ctx.open_module_host_table(output_ports=[video_output_port])`
        returns.
        """
    @property
    def device_path(self) -> str: ...
    @property
    def card_name(self) -> str: ...
    @property
    def fourcc(self) -> str:
        """`NV12` or `YUYV`."""
    @property
    def width(self) -> int: ...
    @property
    def height(self) -> int: ...
    @property
    def bytes_per_line(self) -> int: ...
    @property
    def nominal_frames_per_second(self) -> int | None: ...
    @property
    def colour_primaries(self) -> str | None: ...
    @property
    def colour_transfer(self) -> str | None: ...
    @property
    def colour_matrix(self) -> str | None: ...
    @property
    def colour_range(self) -> str | None: ...
    def stop(self) -> None:
        """Stop streaming and release the device. Idempotent."""

def list_v4l2_capture_devices() -> list[tuple[str, str]]:
    """Every `/dev/video*` node that reports video capture, as `(path, card name)`."""

def monotonic_now_ns() -> int:
    """Nanoseconds on the clock capture timestamps are on."""

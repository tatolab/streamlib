# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native half of `tatolab-camera-proof`, as the wheel's Python sees it."""

from typing import final

__all__ = [
    "CapturedCameraFrame",
    "V4l2CameraCapture",
    "list_v4l2_capture_devices",
    "monotonic_now_ns",
]

@final
class CapturedCameraFrame:
    """One captured frame: its rows, and when it was taken."""

    @property
    def frame_bytes(self) -> bytes: ...
    @property
    def timestamp_ns(self) -> int: ...
    @property
    def timestamp_source(self) -> str:
        """`device`, `clamped_to_dequeue`, or `dequeue`."""
    @property
    def driver_sequence(self) -> int: ...
    @property
    def frames_captured(self) -> int: ...
    @property
    def frames_replaced_before_they_were_read(self) -> int: ...

@final
class V4l2CameraCapture:
    """A camera streaming on a thread this wheel owns; reading takes the newest frame."""

    def __new__(
        cls, device_path: str | None, max_width: int, max_height: int
    ) -> V4l2CameraCapture: ...
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
    def rows_in_one_frame(self) -> int: ...
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
    def luma_weights(self) -> tuple[float, float]:
        """`(Kr, Kb)` for the matrix the frames were encoded with."""
    def wait_for_newest_frame(self, timeout_ms: int) -> CapturedCameraFrame | None:
        """The newest frame not yet read, or `None` after `timeout_ms` with none."""
    def stop(self) -> None:
        """Stop streaming and release the device. Idempotent."""

def list_v4l2_capture_devices() -> list[tuple[str, str]]:
    """Every `/dev/video*` node that reports video capture, as `(path, card name)`."""

def monotonic_now_ns() -> int:
    """Nanoseconds on the clock capture timestamps are on."""

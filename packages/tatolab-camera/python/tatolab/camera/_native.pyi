# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The native half of `tatolab-camera`, as the wheel's Python sees it."""

import sys
from typing import final

if sys.platform == "linux":
    __all__ = [
        "V4l2ZeroCopyCameraCapture",
        "list_v4l2_capture_devices",
        "monotonic_now_ns",
    ]

    @final
    class V4l2ZeroCopyCameraCapture:
        """A camera publishing Image frames on a lent output port from a thread this wheel owns."""

        def __new__(
            cls,
            module_host_table: object,
            video_output_port: str,
            device_path: str | None,
            max_width: int,
            max_height: int,
            output_pool_rotation_depth: int,
        ) -> V4l2ZeroCopyCameraCapture:
            """Open the camera and adopt its buffers; publishing on `video_output_port` waits for `start`.

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
        def start(self) -> None:
            """Stream on and publish every frame from the capture thread."""
        def stop(self) -> None:
            """Stop streaming and release the device. Idempotent."""

    def list_v4l2_capture_devices() -> list[tuple[str, str]]:
        """Every `/dev/video*` node that reports video capture, as `(path, card name)`."""

if sys.platform == "darwin":
    __all__ = [
        "AvFoundationZeroCopyCameraCapture",
        "list_avfoundation_capture_devices",
        "monotonic_now_ns",
    ]

    @final
    class AvFoundationZeroCopyCameraCapture:
        """A camera publishing Image frames on a lent output port from AVFoundation's own queues."""

        def __new__(
            cls,
            module_host_table: object,
            video_output_port: str,
            device_id: str | None,
            max_width: int,
            max_height: int,
            output_pool_rotation_depth: int,
        ) -> AvFoundationZeroCopyCameraCapture:
            """Open the camera and settle camera access; publishing waits for `start`.

            Asks the user without waiting when macOS has never asked. A camera
            macOS refuses, or one nobody was asked for when the runtime
            re-loaded the stream on its own, raises `OSError` naming what to
            change.
            """
        @property
        def device_id(self) -> str:
            """The camera's AVFoundation `uniqueID`."""
        @property
        def device_name(self) -> str: ...
        @property
        def fourcc(self) -> str:
            """`420v` or `420f`."""
        @property
        def width(self) -> int: ...
        @property
        def height(self) -> int: ...
        @property
        def nominal_frames_per_second(self) -> int | None: ...
        def start(self) -> None:
            """Start the session, or start it once the user grants camera access."""
        def stop(self) -> None:
            """Stop the session and give its surfaces back. Idempotent."""

    def list_avfoundation_capture_devices() -> list[tuple[str, str]]:
        """Every camera AVFoundation lists, as `(unique id, name)`: built-in cameras first."""

def monotonic_now_ns() -> int:
    """Nanoseconds on the clock capture timestamps are on."""

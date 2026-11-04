# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`VirtualCameraSink`: video frames in, a camera any Linux application can select out.

Each instance is one v4l2loopback camera that exists only while its node runs,
created at `setup()` and removed at `teardown()`: a USB camera plugged in and
pulled out from every other application's point of view. The engine converts
each RGBA frame into a YUYV pixel buffer on the GPU, and the sink copies it into
the device's next free buffer.

The sink never loads the module, writes a udev rule or asks for elevation:
without write access to the module's control node it refuses at `setup()`,
naming `tatolab enable-virtual-camera`, the one-time command that grants it.
"""

from __future__ import annotations

import ctypes
import dataclasses
import sys
from typing import Annotated, Literal, get_args

from tatolab.stream import (
    GpuSurfaceHandle,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    log,
    node,
)

from .v4l2_color_fields import yuyv_picture_color_of
from .v4l2_loopback_output_device import (
    UDEV_GRANT_DEADLINE_SECONDS,
    YUYV_BYTES_PER_PIXEL,
    StreamingOutputFormat,
    V4l2LoopbackOutputDevice,
    yuyv_picture_byte_count_or_none,
)
from .v4l2loopback_control_node import (
    ENABLE_VIRTUAL_CAMERA_VERB,
    V4L2LOOPBACK_CONTROL_NODE_PATH,
    ControlNodeAccess,
    V4l2LoopbackControlNode,
    V4l2LoopbackControlNodeOnDisk,
    create_or_reclaim_device_then,
    removal_was_refused_because_a_reader_holds_the_device,
)
from .virtual_camera_naming import TheNodeAVirtualCameraIsNamedFor, camera_name_for

__all__ = ["VirtualCameraSink", "VirtualCameraSinkConfig"]

VIRTUAL_CAMERA_SINK_NODE_NAME = "VirtualCameraSink"

VIDEO_INPUT_PORT_NAME = "video"

FRAMES_BETWEEN_PROGRESS_REPORTS = 300
"""Written frames are logged at the first and then at every this many."""

DROPPED_FRAMES_BETWEEN_REPORTS = 300
"""A count of dropped or failed frames is logged at its first and then at every this many."""

VirtualCameraDoorName = Literal["auto", "v4l2loopback", "pipewire"]


@dataclasses.dataclass
class VirtualCameraSinkConfig:
    """What a `VirtualCameraSink` is configured with: the camera's name and the door it takes."""

    name: Annotated[
        "str | None",
        "The camera's name in every picker. Absent: `StreamLib Camera` plus a short id that is "
        "unique per node and stream and stable across runs of the stream.",
    ] = None
    door: Annotated[
        VirtualCameraDoorName,
        "Which door to take: `auto` and `v4l2loopback` take the loopback door; `pipewire` is "
        "refused, as this sink offers no PipeWire camera.",
    ] = "auto"

    def __post_init__(self) -> None:
        if self.name is not None and not isinstance(self.name, str):
            raise ValueError(
                f"VirtualCameraSinkConfig: name is a string, got {self.name!r}"
            )
        if self.door not in get_args(VirtualCameraDoorName):
            raise ValueError(
                "VirtualCameraSinkConfig: door is one of "
                f"{', '.join(get_args(VirtualCameraDoorName))}, got {self.door!r}"
            )


def loopback_door_refusal_or_none(
    door: VirtualCameraDoorName, camera_name: str, access: ControlNodeAccess
) -> "str | None":
    """Why one instance takes no door, or `None` where it takes the loopback door."""
    if door == "pipewire":
        return (
            f'{VIRTUAL_CAMERA_SINK_NODE_NAME} "{camera_name}": this sink offers no PipeWire '
            'camera door. Set door="auto" or door="v4l2loopback" for the v4l2loopback camera '
            "every application sees."
        )
    if access is ControlNodeAccess.WRITABLE:
        return None
    return (
        f'{VIRTUAL_CAMERA_SINK_NODE_NAME} "{camera_name}": no permission to create a '
        f"v4l2loopback camera: {V4L2LOOPBACK_CONTROL_NODE_PATH} is {access.value}. Run "
        f"`{ENABLE_VIRTUAL_CAMERA_VERB}` once (it asks for your password), then re-run."
    )


class _LatchedRefusal(Exception):
    """A frame the camera can never carry; every later frame is dropped."""


@node(
    scheduling="high",
    description=(
        "Presents video frames as a virtual camera any Linux application can select: one "
        "v4l2loopback camera per instance, created at setup and removed at teardown"
    ),
)
class VirtualCameraSink:
    """Each frame's picture, converted to YUYV, as a v4l2loopback camera's next picture.

    The device's format follows the first frame's extent and colour, and an
    extent change re-negotiates it, which readers see as the camera re-plugged.
    A frame of odd width, or one the device refuses to be configured for,
    drops every later frame. A device a reader still holds at teardown is left
    in place and reclaimed by its name at the next setup.
    """

    def __init__(self, config: VirtualCameraSinkConfig) -> None:
        self._config = config
        self._control_node: V4l2LoopbackControlNode = V4l2LoopbackControlNodeOnDisk()
        self._camera_name = ""
        self._device: "V4l2LoopbackOutputDevice | None" = None
        self._streaming: "StreamingOutputFormat | None" = None
        self._yuyv_pixel_buffer: "GpuSurfaceHandle | None" = None
        self._latched_refusal: "str | None" = None
        self._frames_written = 0
        self._frames_dropped_every_buffer_queued = 0
        self._frames_dropped_under_refusal = 0
        self._frames_that_failed_to_write = 0

    @node.input(
        delivery_profile="newest",
        description="Video frames to present as the camera's picture",
    )
    def video(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self._camera_name = camera_name_for(
            self._config.name,
            TheNodeAVirtualCameraIsNamedFor(
                stream_project_directory=ctx.stream_project_directory,
                stream_id=ctx.stream_id,
                node_name=ctx.node_name or VIRTUAL_CAMERA_SINK_NODE_NAME,
            ),
        )
        if not sys.platform.startswith("linux"):
            raise RuntimeError(
                f'{VIRTUAL_CAMERA_SINK_NODE_NAME} "{self._camera_name}": a v4l2loopback camera '
                f"is Linux's; this machine runs {sys.platform}"
            )
        access = self._control_node.access()
        refusal = loopback_door_refusal_or_none(
            self._config.door, self._camera_name, access
        )
        if refusal is not None:
            log.error(
                refusal,
                camera=self._camera_name,
                door=self._config.door,
                control_node=access.value,
            )
            raise RuntimeError(refusal)
        self._device = create_or_reclaim_device_then(
            self._control_node, self._camera_name, self._open_the_created_device
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(VIDEO_INPUT_PORT_NAME, into=VideoFrame)
        if frame is None:
            return
        if self._latched_refusal is not None:
            self._frames_dropped_under_refusal += 1
            return
        try:
            self._present_one_frame(ctx, frame)
        except _LatchedRefusal as refusal:
            self._latched_refusal = str(refusal)
            log.error(
                f"{VIRTUAL_CAMERA_SINK_NODE_NAME}: every later frame is dropped: {refusal}",
                camera=self._camera_name,
            )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        device = self._device
        self._device = None
        if device is None:
            return
        if self._streaming is not None:
            try:
                device.stop_streaming_and_release_buffers(self._streaming)
            except RuntimeError as refusal:
                log.warn(
                    f"{VIRTUAL_CAMERA_SINK_NODE_NAME}: teardown — {refusal}",
                    camera=self._camera_name,
                )
            self._streaming = None
        self._close_the_yuyv_pixel_buffer()
        device.close()
        try:
            self._control_node.remove_device(device.device_number)
        except OSError as refusal:
            if removal_was_refused_because_a_reader_holds_the_device(refusal):
                log.warn(
                    f"{VIRTUAL_CAMERA_SINK_NODE_NAME}: teardown — a reader still holds the "
                    "camera, so the device is left in place and reclaimed by name at the next setup",
                    camera=self._camera_name,
                    device=device.path,
                )
            else:
                log.warn(
                    f"{VIRTUAL_CAMERA_SINK_NODE_NAME}: teardown — the device could not be "
                    "removed and is left for the next setup to reclaim",
                    camera=self._camera_name,
                    device=device.path,
                    error=str(refusal),
                )
            return
        log.info(
            f"{VIRTUAL_CAMERA_SINK_NODE_NAME}: teardown — camera removed",
            camera=self._camera_name,
            device=device.path,
            frames_written=self._frames_written,
            frames_dropped_every_buffer_queued=self._frames_dropped_every_buffer_queued,
            frames_dropped_under_refusal=self._frames_dropped_under_refusal,
            frames_that_failed_to_write=self._frames_that_failed_to_write,
        )

    def _open_the_created_device(
        self, device_number: int, reclaimed: bool
    ) -> V4l2LoopbackOutputDevice:
        try:
            device = V4l2LoopbackOutputDevice.open_awaiting_udev_grant(device_number)
        except OSError as refusal:
            raise RuntimeError(
                f'{VIRTUAL_CAMERA_SINK_NODE_NAME} "{self._camera_name}": /dev/video{device_number} '
                f"was created but could not be opened within {UDEV_GRANT_DEADLINE_SECONDS:g} s: "
                f"{refusal}"
            ) from refusal
        log.info(
            f"{VIRTUAL_CAMERA_SINK_NODE_NAME}: camera created",
            camera=self._camera_name,
            door="v4l2loopback",
            reason=(
                "a device carrying this camera's name was left behind and is reclaimed"
                if reclaimed
                else "the control node is writable, so the sink created its own device"
            ),
            device=device.path,
            driver=device.driver_name() or "?",
            module_version=self._control_node.module_version() or "?",
        )
        return device

    def _present_one_frame(
        self, ctx: RuntimeContextLimitedAccess, frame: VideoFrame
    ) -> None:
        if yuyv_picture_byte_count_or_none(frame.width, frame.height) is None:
            raise _LatchedRefusal(
                f"a {frame.width}x{frame.height} frame cannot be a YUYV camera picture — the "
                "width must be even and both dimensions non-zero, within a frame the device "
                "can size"
            )
        device = self._device
        if device is None:
            return
        streaming = self._streaming_format_for(ctx, device, frame)
        device.reclaim_dequeued_buffers(streaming)
        buffer = streaming.next_free_buffer()
        if buffer is None:
            self._frames_dropped_every_buffer_queued += 1
            if (
                self._frames_dropped_every_buffer_queued - 1
            ) % DROPPED_FRAMES_BETWEEN_REPORTS == 0:
                log.warn(
                    f"{VIRTUAL_CAMERA_SINK_NODE_NAME}: a frame arrived with every buffer queued",
                    camera=self._camera_name,
                    frames_dropped_every_buffer_queued=self._frames_dropped_every_buffer_queued,
                )
            return
        try:
            self._write_the_frame_into(ctx, streaming, buffer.address, frame)
            device.queue_buffer(streaming, buffer, frame.timestamp_ns)
        except RuntimeError as failure:
            self._frames_that_failed_to_write += 1
            if (
                self._frames_that_failed_to_write - 1
            ) % DROPPED_FRAMES_BETWEEN_REPORTS == 0:
                log.warn(
                    f"{VIRTUAL_CAMERA_SINK_NODE_NAME}: a frame could not be written to the camera",
                    camera=self._camera_name,
                    frames_that_failed_to_write=self._frames_that_failed_to_write,
                    error=str(failure),
                )
            return
        self._count_a_written_frame(frame)

    def _streaming_format_for(
        self,
        ctx: RuntimeContextLimitedAccess,
        device: V4l2LoopbackOutputDevice,
        frame: VideoFrame,
    ) -> StreamingOutputFormat:
        streaming = self._streaming
        if streaming is not None and streaming.matches(frame.width, frame.height):
            return streaming
        if streaming is not None:
            log.info(
                f"{VIRTUAL_CAMERA_SINK_NODE_NAME}: upstream extent changed — re-negotiating the "
                "device format; readers reopen as for a camera re-plugged",
                camera=self._camera_name,
                from_extent=f"{streaming.width}x{streaming.height}",
                to_extent=f"{frame.width}x{frame.height}",
            )
            self._streaming = None
            self._close_the_yuyv_pixel_buffer()
            try:
                device.stop_streaming_and_release_buffers(streaming)
            except RuntimeError as refusal:
                raise _LatchedRefusal(
                    f"the device could not be reconfigured: {refusal}"
                ) from refusal
        try:
            streaming = device.negotiate_format_and_start_streaming(
                frame.width,
                frame.height,
                frame.fps,
                yuyv_picture_color_of(frame.color_info),
                self._camera_name,
            )
            self._yuyv_pixel_buffer = ctx.gpu_limited_access.acquire_pixel_buffer(
                frame.width, frame.height, "yuyv"
            )
        except RuntimeError as refusal:
            raise _LatchedRefusal(
                f"the device could not be configured for the frame: {refusal}"
            ) from refusal
        self._streaming = streaming
        return streaming

    def _write_the_frame_into(
        self,
        ctx: RuntimeContextLimitedAccess,
        streaming: StreamingOutputFormat,
        device_buffer_address: int,
        frame: VideoFrame,
    ) -> None:
        yuyv_pixel_buffer = self._yuyv_pixel_buffer
        if yuyv_pixel_buffer is None:
            raise RuntimeError("no YUYV pixel buffer is held for the device's format")
        ctx.gpu_limited_access.convert_surface_into(
            frame.surface_id, yuyv_pixel_buffer, streaming.yuyv_picture_color
        )
        yuyv_pixel_buffer.lock(read_only=True)
        try:
            pixel_buffer_address = yuyv_pixel_buffer.base_address
            if pixel_buffer_address is None:
                raise RuntimeError("the YUYV pixel buffer opened no host mapping")
            copy_rows_between(
                source_address=pixel_buffer_address,
                source_bytes_per_row=yuyv_pixel_buffer.bytes_per_row,
                destination_address=device_buffer_address,
                destination_bytes_per_row=streaming.bytesperline,
                row_byte_count=streaming.width * YUYV_BYTES_PER_PIXEL,
                row_count=streaming.height,
            )
        finally:
            yuyv_pixel_buffer.unlock()

    def _count_a_written_frame(self, frame: VideoFrame) -> None:
        self._frames_written += 1
        if self._frames_written == 1:
            log.info(
                f"{VIRTUAL_CAMERA_SINK_NODE_NAME}: first frame presented",
                camera=self._camera_name,
                width=frame.width,
                height=frame.height,
            )
        elif self._frames_written % FRAMES_BETWEEN_PROGRESS_REPORTS == 0:
            log.info(
                f"{VIRTUAL_CAMERA_SINK_NODE_NAME}: progress",
                camera=self._camera_name,
                frames_written=self._frames_written,
                frames_dropped_every_buffer_queued=self._frames_dropped_every_buffer_queued,
            )

    def _close_the_yuyv_pixel_buffer(self) -> None:
        if self._yuyv_pixel_buffer is not None:
            self._yuyv_pixel_buffer.close()
            self._yuyv_pixel_buffer = None


def copy_rows_between(
    *,
    source_address: int,
    source_bytes_per_row: int,
    destination_address: int,
    destination_bytes_per_row: int,
    row_byte_count: int,
    row_count: int,
) -> None:
    """Copy `row_count` rows of `row_byte_count` bytes, in one move where both sides are packed alike."""
    if source_bytes_per_row == destination_bytes_per_row == row_byte_count:
        ctypes.memmove(destination_address, source_address, row_byte_count * row_count)
        return
    for row_index in range(row_count):
        ctypes.memmove(
            destination_address + row_index * destination_bytes_per_row,
            source_address + row_index * source_bytes_per_row,
            row_byte_count,
        )

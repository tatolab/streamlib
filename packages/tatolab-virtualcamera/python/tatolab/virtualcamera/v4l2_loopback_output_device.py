# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One loopback camera's `/dev/video<N>`, open for output: its format, its memory-mapped buffers and their queue."""

from __future__ import annotations

import ctypes
import dataclasses
import fcntl
import mmap
import os
import time

from tatolab.stream import ColorInfo, log

from .v4l2_color_fields import v4l2_color_fields_of
from .v4l2_output_device_abi import (
    V4L2_BUF_TYPE_VIDEO_OUTPUT,
    V4L2_FIELD_NONE,
    V4L2_MEMORY_MMAP,
    V4L2_PIX_FMT_PRIV_MAGIC,
    V4L2_PIX_FMT_YUYV,
    VIDIOC_DQBUF,
    VIDIOC_QBUF,
    VIDIOC_QUERYBUF,
    VIDIOC_QUERYCAP,
    VIDIOC_REQBUFS,
    VIDIOC_S_FMT,
    VIDIOC_S_PARM,
    VIDIOC_STREAMOFF,
    VIDIOC_STREAMON,
    V4l2Buffer,
    V4l2Capability,
    V4l2Format,
    V4l2RequestBuffers,
    V4l2StreamParm,
)
from .v4l2loopback_control_node import LOOPBACK_DEVICE_BUFFER_COUNT

UDEV_GRANT_DEADLINE_SECONDS = 3.0
"""How long a fresh device's open waits for udev to apply its `uaccess` ACL."""

_UDEV_GRANT_POLL_INTERVAL_SECONDS = 0.01

YUYV_BYTES_PER_PIXEL = 2

_LARGEST_V4L2_SIZE_FIELD = 2**32 - 1


def yuyv_picture_byte_count_or_none(width: int, height: int) -> "int | None":
    """The tightly packed YUYV size of a `width × height` picture, or `None` where no camera can carry it.

    YUYV packs two pixels a macropixel, so the width must be even, and the
    size must fit the device's 32-bit `sizeimage`.
    """
    byte_count = width * YUYV_BYTES_PER_PIXEL * height
    if width % 2 != 0 or byte_count <= 0 or byte_count > _LARGEST_V4L2_SIZE_FIELD:
        return None
    return byte_count


def _output_mmap_buffer_description(index: int) -> V4l2Buffer:
    description = V4l2Buffer()
    description.type = V4L2_BUF_TYPE_VIDEO_OUTPUT
    description.memory = V4L2_MEMORY_MMAP
    description.index = index
    return description


def _output_mmap_buffer_request(count: int) -> V4l2RequestBuffers:
    request = V4l2RequestBuffers()
    request.count = count
    request.type = V4L2_BUF_TYPE_VIDEO_OUTPUT
    request.memory = V4L2_MEMORY_MMAP
    return request


class _MappedOutputBuffer:
    """One of the device's output buffers, mapped into this process."""

    def __init__(self, index: int, page_mapping: mmap.mmap) -> None:
        self.index = index
        self._page_mapping = page_mapping
        self._first_byte = ctypes.c_char.from_buffer(page_mapping)
        self.address = ctypes.addressof(self._first_byte)
        self.length = len(page_mapping)
        self.queued = False

    def unmap(self) -> None:
        del self._first_byte
        self._page_mapping.close()


@dataclasses.dataclass
class StreamingOutputFormat:
    """The format the device streams, set from a frame's extent and colour."""

    width: int
    height: int
    bytesperline: int
    sizeimage: int
    yuyv_picture_color: ColorInfo
    buffers: "list[_MappedOutputBuffer]"
    next_buffer_position: int = 0

    def matches(self, width: int, height: int) -> bool:
        return self.width == width and self.height == height

    def next_free_buffer(self) -> "_MappedOutputBuffer | None":
        """The next buffer the driver is not holding, round-robin from the last one."""
        buffer_count = len(self.buffers)
        for step in range(buffer_count):
            position = (self.next_buffer_position + step) % buffer_count
            if not self.buffers[position].queued:
                self.next_buffer_position = (position + 1) % buffer_count
                return self.buffers[position]
        return None


class V4l2LoopbackOutputDevice:
    """`/dev/video<N>` open read-write and non-blocking, as the camera's one writer."""

    def __init__(self, device_number: int, device_fd: int) -> None:
        self.device_number = device_number
        self.path = f"/dev/video{device_number}"
        self._device_fd = device_fd

    @classmethod
    def open_awaiting_udev_grant(cls, device_number: int) -> "V4l2LoopbackOutputDevice":
        """Open the device, retrying a permission refusal until udev grants it or the deadline passes."""
        path = f"/dev/video{device_number}"
        deadline = time.monotonic() + UDEV_GRANT_DEADLINE_SECONDS
        while True:
            try:
                return cls(device_number, os.open(path, os.O_RDWR | os.O_NONBLOCK))
            except PermissionError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(_UDEV_GRANT_POLL_INTERVAL_SECONDS)

    def driver_name(self) -> "str | None":
        capability = V4l2Capability()
        try:
            fcntl.ioctl(self._device_fd, VIDIOC_QUERYCAP, capability)
        except OSError:
            return None
        return capability.driver.decode("utf-8", errors="replace")

    def negotiate_format_and_start_streaming(
        self,
        width: int,
        height: int,
        fps: "int | None",
        yuyv_picture_color: ColorInfo,
        camera_name: str,
    ) -> StreamingOutputFormat:
        """`S_FMT`, `S_PARM`, `REQBUFS`, `QUERYBUF` and `mmap`, `STREAMON`, in that order.

        The loopback's poll returns nothing between `REQBUFS` and `STREAMON`,
        so a buffer queued before the start would never come back.
        """
        color_fields = v4l2_color_fields_of(yuyv_picture_color)
        output_format = V4l2Format()
        output_format.type = V4L2_BUF_TYPE_VIDEO_OUTPUT
        pix = output_format.fmt.pix
        pix.width = width
        pix.height = height
        pix.pixelformat = V4L2_PIX_FMT_YUYV
        pix.field = V4L2_FIELD_NONE
        pix.priv = V4L2_PIX_FMT_PRIV_MAGIC
        pix.bytesperline = width * YUYV_BYTES_PER_PIXEL
        pix.sizeimage = width * YUYV_BYTES_PER_PIXEL * height
        pix.colorspace = color_fields.colorspace
        pix.xfer_func = color_fields.xfer_func
        pix.ycbcr_enc = color_fields.ycbcr_enc
        pix.quantization = color_fields.quantization
        self._ioctl(VIDIOC_S_FMT, output_format, "S_FMT")
        bytesperline, sizeimage = pix.bytesperline, pix.sizeimage
        if (
            pix.pixelformat != V4L2_PIX_FMT_YUYV
            or bytesperline < width * YUYV_BYTES_PER_PIXEL
            or bytesperline % 4 != 0
            or sizeimage < bytesperline * height
        ):
            raise RuntimeError(
                f'VirtualCameraSink "{camera_name}": the device set {width}x{height} '
                f"bytesperline={bytesperline} sizeimage={sizeimage} rather than YUYV at 2×width"
            )

        if fps is not None and fps > 0:
            stream_parameters = V4l2StreamParm()
            stream_parameters.type = V4L2_BUF_TYPE_VIDEO_OUTPUT
            stream_parameters.parm.output.timeperframe.numerator = 1
            stream_parameters.parm.output.timeperframe.denominator = fps
            try:
                fcntl.ioctl(self._device_fd, VIDIOC_S_PARM, stream_parameters)
            except OSError as refusal:
                log.debug(
                    "VirtualCameraSink: S_PARM declined; readers keep the device's interval",
                    camera=camera_name,
                    error=str(refusal),
                )

        request = _output_mmap_buffer_request(LOOPBACK_DEVICE_BUFFER_COUNT)
        self._ioctl(VIDIOC_REQBUFS, request, "REQBUFS")
        if request.count == 0:
            raise RuntimeError(
                f'VirtualCameraSink "{camera_name}": the device granted no output buffers'
            )
        buffers: "list[_MappedOutputBuffer]" = []
        try:
            for index in range(request.count):
                description = _output_mmap_buffer_description(index)
                self._ioctl(VIDIOC_QUERYBUF, description, "QUERYBUF")
                page_mapping = mmap.mmap(
                    self._device_fd,
                    description.length,
                    flags=mmap.MAP_SHARED,
                    prot=mmap.PROT_READ | mmap.PROT_WRITE,
                    offset=description.m.offset,
                )
                buffers.append(_MappedOutputBuffer(index, page_mapping))
            self._ioctl(
                VIDIOC_STREAMON, ctypes.c_int(V4L2_BUF_TYPE_VIDEO_OUTPUT), "STREAMON"
            )
        except BaseException:
            for buffer in buffers:
                buffer.unmap()
            self._release_buffers_quietly()
            raise
        log.info(
            "VirtualCameraSink: device format set from the frame — YUYV, streaming",
            camera=camera_name,
            width=width,
            height=height,
            bytesperline=bytesperline,
            sizeimage=sizeimage,
            buffers=len(buffers),
        )
        return StreamingOutputFormat(
            width=width,
            height=height,
            bytesperline=bytesperline,
            sizeimage=sizeimage,
            yuyv_picture_color=yuyv_picture_color,
            buffers=buffers,
        )

    def reclaim_dequeued_buffers(self, streaming: StreamingOutputFormat) -> None:
        """`DQBUF` until the driver hands nothing more back; each returned buffer is free again."""
        buffers_by_index = {buffer.index: buffer for buffer in streaming.buffers}
        while True:
            description = _output_mmap_buffer_description(0)
            try:
                fcntl.ioctl(self._device_fd, VIDIOC_DQBUF, description)
            except OSError:
                return
            dequeued = buffers_by_index.get(description.index)
            if dequeued is not None:
                dequeued.queued = False

    def queue_buffer(
        self,
        streaming: StreamingOutputFormat,
        buffer: _MappedOutputBuffer,
        timestamp_ns: int,
    ) -> None:
        """`QBUF` with the frame's monotonic stamp, which the driver copies through to readers."""
        description = _output_mmap_buffer_description(buffer.index)
        description.field = V4L2_FIELD_NONE
        description.bytesused = streaming.sizeimage
        timestamp_seconds, timestamp_remainder_ns = divmod(timestamp_ns, 1_000_000_000)
        description.timestamp.tv_sec = timestamp_seconds
        description.timestamp.tv_usec = timestamp_remainder_ns // 1_000
        self._ioctl(VIDIOC_QBUF, description, "QBUF")
        buffer.queued = True

    def stop_streaming_and_release_buffers(
        self, streaming: StreamingOutputFormat
    ) -> None:
        """`STREAMOFF`, unmap every buffer, then `REQBUFS(0)`."""
        stream_off_refusal: "OSError | None" = None
        try:
            fcntl.ioctl(
                self._device_fd,
                VIDIOC_STREAMOFF,
                ctypes.c_int(V4L2_BUF_TYPE_VIDEO_OUTPUT),
            )
        except OSError as refusal:
            stream_off_refusal = refusal
        for buffer in streaming.buffers:
            buffer.unmap()
        streaming.buffers.clear()
        self._ioctl(VIDIOC_REQBUFS, _output_mmap_buffer_request(0), "REQBUFS(0)")
        if stream_off_refusal is not None:
            raise RuntimeError(f"STREAMOFF on {self.path}: {stream_off_refusal}")

    def close(self) -> None:
        os.close(self._device_fd)

    def _release_buffers_quietly(self) -> None:
        try:
            fcntl.ioctl(self._device_fd, VIDIOC_REQBUFS, _output_mmap_buffer_request(0))
        except OSError:
            pass

    def _ioctl(
        self, request: int, argument: "ctypes.Structure | ctypes.c_int", what: str
    ) -> None:
        try:
            fcntl.ioctl(self._device_fd, request, argument)
        except OSError as refusal:
            raise RuntimeError(f"{what} on {self.path}: {refusal}") from refusal

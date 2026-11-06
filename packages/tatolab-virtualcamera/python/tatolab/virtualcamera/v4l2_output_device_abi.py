# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The kernel ABI the sink writes a camera through: `<linux/videodev2.h>` and `v4l2loopback.h` 0.15.

Every struct is laid out by `ctypes` with the C compiler's own alignment, and
every request number is `_IOC(direction, type, number, size)` of the struct it
carries, as the headers define them.
"""

from __future__ import annotations

import ctypes

_IOC_WRITE = 1
_IOC_READ = 2


def linux_ioc(direction: int, io_type: str, number: int, size: int) -> int:
    """Linux `_IOC(dir, type, nr, size)`."""
    return (direction << 30) | (size << 16) | (ord(io_type) << 8) | number


class V4l2Capability(ctypes.Structure):
    """`struct v4l2_capability`, what `VIDIOC_QUERYCAP` fills."""

    _fields_ = [
        ("driver", ctypes.c_char * 16),
        ("card", ctypes.c_char * 32),
        ("bus_info", ctypes.c_char * 32),
        ("version", ctypes.c_uint32),
        ("capabilities", ctypes.c_uint32),
        ("device_caps", ctypes.c_uint32),
        ("reserved", ctypes.c_uint32 * 3),
    ]


class V4l2PixFormat(ctypes.Structure):
    """`struct v4l2_pix_format`, a single-planar picture's format."""

    _fields_ = [
        ("width", ctypes.c_uint32),
        ("height", ctypes.c_uint32),
        ("pixelformat", ctypes.c_uint32),
        ("field", ctypes.c_uint32),
        ("bytesperline", ctypes.c_uint32),
        ("sizeimage", ctypes.c_uint32),
        ("colorspace", ctypes.c_uint32),
        ("priv", ctypes.c_uint32),
        ("flags", ctypes.c_uint32),
        ("ycbcr_enc", ctypes.c_uint32),
        ("quantization", ctypes.c_uint32),
        ("xfer_func", ctypes.c_uint32),
    ]


class _V4l2FormatUnion(ctypes.Union):
    # `v4l2_window` holds pointers, which give the union its pointer alignment.
    _fields_ = [
        ("pix", V4l2PixFormat),
        ("raw_data", ctypes.c_uint8 * 200),
        ("pointer_alignment", ctypes.c_void_p),
    ]


class V4l2Format(ctypes.Structure):
    """`struct v4l2_format`, what `VIDIOC_S_FMT` takes and fills."""

    _fields_ = [("type", ctypes.c_uint32), ("fmt", _V4l2FormatUnion)]


class V4l2RequestBuffers(ctypes.Structure):
    """`struct v4l2_requestbuffers`, what `VIDIOC_REQBUFS` takes and fills."""

    _fields_ = [
        ("count", ctypes.c_uint32),
        ("type", ctypes.c_uint32),
        ("memory", ctypes.c_uint32),
        ("capabilities", ctypes.c_uint32),
        ("flags", ctypes.c_uint8),
        ("reserved", ctypes.c_uint8 * 3),
    ]


class Timeval(ctypes.Structure):
    """`struct timeval` on a 64-bit kernel ABI."""

    _fields_ = [("tv_sec", ctypes.c_long), ("tv_usec", ctypes.c_long)]


class V4l2Timecode(ctypes.Structure):
    """`struct v4l2_timecode`."""

    _fields_ = [
        ("type", ctypes.c_uint32),
        ("flags", ctypes.c_uint32),
        ("frames", ctypes.c_uint8),
        ("seconds", ctypes.c_uint8),
        ("minutes", ctypes.c_uint8),
        ("hours", ctypes.c_uint8),
        ("userbits", ctypes.c_uint8 * 4),
    ]


class _V4l2BufferMemoryUnion(ctypes.Union):
    _fields_ = [
        ("offset", ctypes.c_uint32),
        ("userptr", ctypes.c_ulong),
        ("planes", ctypes.c_void_p),
        ("fd", ctypes.c_int32),
    ]


class _V4l2BufferRequestUnion(ctypes.Union):
    _fields_ = [("request_fd", ctypes.c_int32), ("reserved", ctypes.c_uint32)]


class V4l2Buffer(ctypes.Structure):
    """`struct v4l2_buffer`, what `QUERYBUF`, `QBUF` and `DQBUF` take and fill."""

    _fields_ = [
        ("index", ctypes.c_uint32),
        ("type", ctypes.c_uint32),
        ("bytesused", ctypes.c_uint32),
        ("flags", ctypes.c_uint32),
        ("field", ctypes.c_uint32),
        ("timestamp", Timeval),
        ("timecode", V4l2Timecode),
        ("sequence", ctypes.c_uint32),
        ("memory", ctypes.c_uint32),
        ("m", _V4l2BufferMemoryUnion),
        ("length", ctypes.c_uint32),
        ("reserved2", ctypes.c_uint32),
        ("request", _V4l2BufferRequestUnion),
    ]


V4L2LOOPBACK_CARD_LABEL_BYTES = 32
"""The size of the module's `card_label` field, its NUL included."""


class V4l2LoopbackConfig(ctypes.Structure):
    """`struct v4l2_loopback_config` as the 0.15 module declares it: what `CTL_ADD` takes and `CTL_QUERY` fills."""

    _fields_ = [
        ("output_nr", ctypes.c_int32),
        ("unused", ctypes.c_int32),
        ("card_label", ctypes.c_char * V4L2LOOPBACK_CARD_LABEL_BYTES),
        ("min_width", ctypes.c_uint32),
        ("max_width", ctypes.c_uint32),
        ("min_height", ctypes.c_uint32),
        ("max_height", ctypes.c_uint32),
        ("max_buffers", ctypes.c_int32),
        ("max_openers", ctypes.c_int32),
        ("debug", ctypes.c_int32),
        ("announce_all_caps", ctypes.c_int32),
    ]

    def label(self) -> str:
        """The label as text, up to its NUL."""
        return (
            bytes(self.card_label).split(b"\0", 1)[0].decode("utf-8", errors="replace")
        )


VIDIOC_QUERYCAP = linux_ioc(_IOC_READ, "V", 0, ctypes.sizeof(V4l2Capability))
VIDIOC_S_FMT = linux_ioc(_IOC_READ | _IOC_WRITE, "V", 5, ctypes.sizeof(V4l2Format))
VIDIOC_REQBUFS = linux_ioc(
    _IOC_READ | _IOC_WRITE, "V", 8, ctypes.sizeof(V4l2RequestBuffers)
)
VIDIOC_QUERYBUF = linux_ioc(_IOC_READ | _IOC_WRITE, "V", 9, ctypes.sizeof(V4l2Buffer))
VIDIOC_QBUF = linux_ioc(_IOC_READ | _IOC_WRITE, "V", 15, ctypes.sizeof(V4l2Buffer))
VIDIOC_DQBUF = linux_ioc(_IOC_READ | _IOC_WRITE, "V", 17, ctypes.sizeof(V4l2Buffer))
VIDIOC_STREAMON = linux_ioc(_IOC_WRITE, "V", 18, ctypes.sizeof(ctypes.c_int))
VIDIOC_STREAMOFF = linux_ioc(_IOC_WRITE, "V", 19, ctypes.sizeof(ctypes.c_int))

V4L2LOOPBACK_CTL_VERSION = linux_ioc(_IOC_READ, "~", 0, ctypes.sizeof(ctypes.c_uint32))
V4L2LOOPBACK_CTL_ADD = linux_ioc(_IOC_WRITE, "~", 1, ctypes.sizeof(V4l2LoopbackConfig))
V4L2LOOPBACK_CTL_REMOVE = linux_ioc(_IOC_WRITE, "~", 2, ctypes.sizeof(ctypes.c_uint32))
V4L2LOOPBACK_CTL_QUERY = linux_ioc(
    _IOC_READ | _IOC_WRITE, "~", 3, ctypes.sizeof(V4l2LoopbackConfig)
)

V4L2_BUF_TYPE_VIDEO_OUTPUT = 2
V4L2_MEMORY_MMAP = 1
V4L2_FIELD_NONE = 1
V4L2_PIX_FMT_YUYV = int.from_bytes(b"YUYV", "little")

V4L2_PIX_FMT_PRIV_MAGIC = 0xFEEDCAFE
"""Without this in `priv` the V4L2 core zeroes `flags`, `ycbcr_enc`, `quantization` and `xfer_func` on `S_FMT`."""

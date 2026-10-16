# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The cast for a video-frame message: a GPU surface reference plus per-frame metadata.

The payload is the msgpack map the camera and test pattern write; the pixels
never ride it — the frame names a GPU surface by `surface_id`. Read as a
`VideoFrame`, the frame holds its surface against producer reuse and speaks
`__dlpack__`, so `torch.from_dlpack(frame)` works straight off the read;
`frame.writable()` is the GPU write door and `frame.cpu()` the CPU one. All of
it comes from `ClaimedSurfacePixelAccess`, which any cast type may compose.
"""

from __future__ import annotations

import typing
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Literal, cast

from .claimed_surface_pixel_access import ClaimedSurfacePixelAccess
from .msgpack import MsgpackCast

__all__ = [
    "ColorInfo",
    "ContentLight",
    "MasteringDisplay",
    "VideoFrame",
]

Primaries = Literal[
    "bt2020", "bt470_bg", "bt470_m", "bt709", "ebu3213", "film",
    "smpte170m", "smpte240m", "smpte428", "smpte431", "smpte432",
]
Transfer = Literal[
    "arib_std_b67", "bt1361", "bt2020_ten_bit", "bt2020_twelve_bit", "bt709",
    "gamma22", "gamma28", "linear", "log100", "log100_sqrt10", "smpte170m",
    "smpte2084", "smpte240m", "smpte428", "srgb", "xvycc",
]
Matrix = Literal[
    "bt2020_cl", "bt2020_ncl", "bt470_bg", "bt709", "chroma_cl", "chroma_ncl",
    "fcc", "ictcp", "identity", "smpte170m", "smpte2085", "smpte240m", "ycgco",
]
Range = Literal["full", "limited"]

_NestedCast = typing.TypeVar("_NestedCast", "ContentLight", "MasteringDisplay")

_VIDEO_FRAME_REFUSAL_SUBJECT = "message is not a video frame"


def _is_plain_int(value: Any) -> bool:
    """`bool` is an `int` subclass; a frame dimension of `True` is a bug."""
    return isinstance(value, int) and not isinstance(value, bool)


def _require_int_or_none(key: str, value: Any) -> "int | None":
    if value is not None and not _is_plain_int(value):
        raise ValueError(
            f"{_VIDEO_FRAME_REFUSAL_SUBJECT}: {key!r} must be int or absent"
        )
    return value


def _require_mapping(
    refusal_subject: str, key: str, value: Any
) -> "Mapping[str, Any]":
    if not isinstance(value, Mapping):
        raise ValueError(f"{refusal_subject}: {key!r} must be a mapping or absent")
    return value


def _cast_nested(
    key: str, nested_type: "type[_NestedCast]", nested_entries: Mapping[str, Any]
) -> "_NestedCast":
    """Construct a nested cast, refusing a malformed map with `ValueError`."""
    try:
        return nested_type(**nested_entries)
    except TypeError as construction_error:
        raise ValueError(
            f"{_VIDEO_FRAME_REFUSAL_SUBJECT}: {key!r} is malformed "
            f"({construction_error})"
        ) from None


def _nested_or_none(
    key: str, nested_type: "type[_NestedCast]", value: Any
) -> "_NestedCast | None":
    """Nested metadata as its own type, whether it arrived already cast or as
    the message's nested map."""
    if value is None or isinstance(value, nested_type):
        return value
    return _cast_nested(
        key, nested_type, _require_mapping(_VIDEO_FRAME_REFUSAL_SUBJECT, key, value)
    )


# Every name a cast can place on each H.273 axis. The engine's own producers
# narrow an unknown H.273 byte to absent, so an unrecognised name is refused.
_H273_AXIS_NAMES: "dict[str, frozenset[str]]" = {
    "primaries": frozenset(typing.get_args(Primaries)),
    "transfer": frozenset(typing.get_args(Transfer)),
    "matrix": frozenset(typing.get_args(Matrix)),
    "range": frozenset(typing.get_args(Range)),
}


def _h273_axis_or_none(
    refusal_subject: str, key: str, axis_name: str, value: Any
) -> "str | None":
    if value is None:
        return None
    if not isinstance(value, str):
        raise ValueError(
            f"{refusal_subject}: {key}.{axis_name} must be a string or absent; "
            f"got {value!r}"
        )
    if value not in _H273_AXIS_NAMES[axis_name]:
        raise ValueError(
            f"{refusal_subject}: {key}.{axis_name} is {value!r}, which is not an "
            f"H.273 {axis_name} name"
        )
    return value


def _color_info_or_none(
    refusal_subject: str, key: str, value: Any
) -> "ColorInfo | None":
    """`ColorInfo` read field by field, ignoring keys it does not know.

    The same four-tuple rides a video frame as `color_info` and an encoded
    frame as `color`, so the caller names the key and the refusal subject.
    """
    if value is None or isinstance(value, ColorInfo):
        return value
    color_info_entries = _require_mapping(refusal_subject, key, value)

    def axis(axis_name: str) -> "str | None":
        return _h273_axis_or_none(
            refusal_subject, key, axis_name, color_info_entries.get(axis_name)
        )

    return ColorInfo(
        primaries=cast("Primaries | None", axis("primaries")),
        transfer=cast("Transfer | None", axis("transfer")),
        matrix=cast("Matrix | None", axis("matrix")),
        range=cast("Range | None", axis("range")),
    )


@dataclass(frozen=True)
class ColorInfo:
    """H.273 / ITU-T VUI four-tuple. ``None`` means unspecified."""

    primaries: Primaries | None = None
    transfer: Transfer | None = None
    matrix: Matrix | None = None
    range: Range | None = None


@dataclass(frozen=True)
class ContentLight:
    """HDR10 content light level (MaxCLL / MaxFALL), in cd/m²."""

    max_cll: int
    max_fall: int


@dataclass(frozen=True)
class MasteringDisplay:
    """SMPTE ST.2086 mastering display color volume (HDR10 static metadata).

    Chromaticities are in 1/50000 increments; luminances in 0.0001 cd/m²
    increments.
    """

    display_primaries_r_x: int
    display_primaries_r_y: int
    display_primaries_g_x: int
    display_primaries_g_y: int
    display_primaries_b_x: int
    display_primaries_b_y: int
    white_point_x: int
    white_point_y: int
    max_luminance: int
    min_luminance: int


@dataclass(frozen=True, init=False)
class VideoFrame(ClaimedSurfacePixelAccess, MsgpackCast):
    """A video-frame message, cast: GPU surface reference plus per-frame metadata.

    ``surface_id`` names the pixels; ``timestamp_ns`` (the machine's monotonic
    clock in nanoseconds) orders frames.

    Read through ``ctx.inputs.read(port, into=VideoFrame)``, the frame holds
    its surface still: the producer cannot recycle those pixels while this
    object lives, and letting it go releases them. Holding a frame longer
    costs the producer memory and then its own frames — never another node's
    cadence. A frame built anywhere else — `VideoFrame.from_bytes` on a payload
    in hand, or by hand — takes no claim and rides pool depth.

    ``to_bytes()`` writes the frame back as the message it was read from.
    """

    surface_id: str
    width: int
    height: int
    timestamp_ns: int
    fps: int | None = None
    color_info: ColorInfo | None = None
    content_light: ContentLight | None = None
    mastering_display: MasteringDisplay | None = None
    texture_layout: int | None = None

    def __init__(
        self,
        surface_id: str,
        width: int,
        height: int,
        timestamp_ns: int,
        fps: "int | None" = None,
        color_info: "ColorInfo | Mapping[str, Any] | None" = None,
        content_light: "ContentLight | Mapping[str, Any] | None" = None,
        mastering_display: "MasteringDisplay | Mapping[str, Any] | None" = None,
        texture_layout: "int | None" = None,
        **keys_this_cast_does_not_read: Any,
    ) -> None:
        """Validate and cast the nested metadata, then claim the surface.

        A message is an open map, so keys this cast does not read are ignored.
        """
        del keys_this_cast_does_not_read
        if (
            not isinstance(surface_id, str)
            or not _is_plain_int(width)
            or not _is_plain_int(height)
            or not _is_plain_int(timestamp_ns)
        ):
            raise ValueError(
                f"{_VIDEO_FRAME_REFUSAL_SUBJECT}: surface_id must be str and "
                "width/height/timestamp_ns must be int"
            )
        super().__init__(
            surface_id=surface_id,
            width=width,
            height=height,
            timestamp_ns=timestamp_ns,
            fps=_require_int_or_none("fps", fps),
            texture_layout=_require_int_or_none("texture_layout", texture_layout),
            color_info=_color_info_or_none(
                _VIDEO_FRAME_REFUSAL_SUBJECT, "color_info", color_info
            ),
            content_light=_nested_or_none("content_light", ContentLight, content_light),
            mastering_display=_nested_or_none(
                "mastering_display", MasteringDisplay, mastering_display
            ),
        )

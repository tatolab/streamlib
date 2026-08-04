# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The optional typed cast for a video-frame bag.

A link carries a self-describing bag (a dict); its keys are the contract and
reading them directly is always enough. `VideoFrame.from_bag` is the opt-in
strictness dial for consumers that want construction-time validation and
attribute access instead of key lookups. Pixels never ride the bag — the
frame references a GPU surface by `surface_id`, resolved out-of-band.
"""

from __future__ import annotations

import typing
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Literal, cast

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


def _is_plain_int(value: Any) -> bool:
    """`bool` is an `int` subclass; a frame dimension of `True` is a bug."""
    return isinstance(value, int) and not isinstance(value, bool)


def _require_int_or_none(key: str, value: Any) -> "int | None":
    if value is not None and not _is_plain_int(value):
        raise ValueError(f"bag is not a video frame: {key!r} must be int or absent")
    return value


def _require_mapping_or_none(key: str, value: Any) -> "Mapping[str, Any] | None":
    if value is not None and not isinstance(value, Mapping):
        raise ValueError(f"bag is not a video frame: {key!r} must be a mapping or absent")
    return value


def _cast_nested(
    key: str, nested_type: "type[_NestedCast]", nested_bag: Mapping[str, Any]
) -> "_NestedCast":
    """Construct a nested cast, keeping the ValueError contract of `from_bag`."""
    try:
        return nested_type(**nested_bag)
    except TypeError as construction_error:
        raise ValueError(
            f"bag is not a video frame: {key!r} is malformed ({construction_error})"
        ) from None


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


@dataclass(frozen=True)
class VideoFrame:
    """A video-frame bag, cast: GPU surface reference plus per-frame metadata.

    ``surface_id`` is the handoff contract; ``timestamp_ns`` (monotonic
    nanoseconds — process-relative until the engine's clock-epoch unification
    lands) is the ordering primitive.
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

    @classmethod
    def from_bag(cls, bag: Mapping[str, Any]) -> "VideoFrame":
        """Construct from a bag dict, raising ValueError on missing or
        mistyped keys — required and optional alike."""
        try:
            surface_id = bag["surface_id"]
            width = bag["width"]
            height = bag["height"]
            timestamp_ns = bag["timestamp_ns"]
        except KeyError as missing:
            raise ValueError(
                f"bag is not a video frame: missing key {missing.args[0]!r}"
            ) from None
        if (
            not isinstance(surface_id, str)
            or not _is_plain_int(width)
            or not _is_plain_int(height)
            or not _is_plain_int(timestamp_ns)
        ):
            raise ValueError(
                "bag is not a video frame: surface_id must be str and "
                "width/height/timestamp_ns must be int"
            )

        color_info_bag = _require_mapping_or_none("color_info", bag.get("color_info"))
        content_light_bag = _require_mapping_or_none("content_light", bag.get("content_light"))
        mastering_display_bag = _require_mapping_or_none(
            "mastering_display", bag.get("mastering_display")
        )
        return cls(
            surface_id=surface_id,
            width=width,
            height=height,
            timestamp_ns=timestamp_ns,
            fps=_require_int_or_none("fps", bag.get("fps")),
            color_info=(
                ColorInfo(
                    primaries=cast("Primaries | None", color_info_bag.get("primaries")),
                    transfer=cast("Transfer | None", color_info_bag.get("transfer")),
                    matrix=cast("Matrix | None", color_info_bag.get("matrix")),
                    range=cast("Range | None", color_info_bag.get("range")),
                )
                if color_info_bag is not None
                else None
            ),
            content_light=(
                _cast_nested("content_light", ContentLight, content_light_bag)
                if content_light_bag is not None
                else None
            ),
            mastering_display=(
                _cast_nested("mastering_display", MasteringDisplay, mastering_display_bag)
                if mastering_display_bag is not None
                else None
            ),
            texture_layout=_require_int_or_none("texture_layout", bag.get("texture_layout")),
        )

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The optional typed cast for a video-frame bag.

A link carries a self-describing bag (a dict); its keys are the contract and
reading them directly is always enough. Casting is the opt-in dial for
consumers that want construction-time validation and attribute access instead
of key lookups. Pixels never ride the bag — the frame references a GPU surface
by `surface_id`, resolved out-of-band.

Casting also buys the frame's own lifetime: read as a `VideoFrame`, the frame
babysits its buffer, and the producer cannot recycle the pixels under a frame
you are still holding. Nothing here is privileged — the class asks the read in
progress for the capability and keeps what it gets in a field, which any class
can do, and which is why reading the bag as a dict stays first-class and
unpenalized.
"""

from __future__ import annotations

import typing
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Literal, cast

from ._engine import (
    GpuSurfaceCheckOutLease,
    gpu_limited_access_of_the_typed_read_in_progress,
)

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


def _require_mapping(key: str, value: Any) -> "Mapping[str, Any]":
    if not isinstance(value, Mapping):
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


def _nested_or_none(
    key: str, nested_type: "type[_NestedCast]", value: Any
) -> "_NestedCast | None":
    """Nested metadata as its own type, whether it arrived already cast or as
    the bag's nested map."""
    if value is None or isinstance(value, nested_type):
        return value
    return _cast_nested(key, nested_type, _require_mapping(key, value))


def _color_info_or_none(value: Any) -> "ColorInfo | None":
    """`ColorInfo` reads field by field rather than by construction: every
    field is optional and a bag may carry a key this version does not know,
    where the H.273 tuple's absent-means-unspecified rule still applies."""
    if value is None or isinstance(value, ColorInfo):
        return value
    color_info_bag = _require_mapping("color_info", value)
    return ColorInfo(
        primaries=cast("Primaries | None", color_info_bag.get("primaries")),
        transfer=cast("Transfer | None", color_info_bag.get("transfer")),
        matrix=cast("Matrix | None", color_info_bag.get("matrix")),
        range=cast("Range | None", color_info_bag.get("range")),
    )


def _claim_on_the_surface_a_frame_names(surface_id: str) -> "GpuSurfaceCheckOutLease | None":
    """The claim a frame holds on its own pixels, or `None` when nothing
    offered the means to take one.

    Only a `read(port, into=…)` offers it, so a frame built from a dict you are
    holding claims nothing — a hand-rolled bag may name no live surface at all.
    """
    gpu_limited_access = gpu_limited_access_of_the_typed_read_in_progress()
    if gpu_limited_access is None:
        return None
    try:
        return gpu_limited_access.claim_surface_against_producer_reuse(surface_id)
    except RuntimeError:
        # Every way this fails means the frame cannot be pinned — its surface
        # is already gone, or this helper has no route to the engine — and none
        # of them make the bag unreadable. An unclaimed frame falls back to the
        # protection pool depth gives it, which is what an untyped read gets;
        # raising here would turn a delivered frame into an exception.
        return None


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

    ``surface_id`` is the handoff contract; ``timestamp_ns`` (the machine's
    monotonic clock in nanoseconds, comparable across every process on the
    host) is the ordering primitive.

    Read through ``ctx.inputs.read(port, into=VideoFrame)``, the frame also
    holds its surface still: the producer cannot recycle those pixels while
    this object lives, and letting it go releases them. There is nothing to
    call, and holding a frame for longer costs the producer memory and then its
    own frames — never another processor's cadence.
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

    def __post_init__(self) -> None:
        """Validate, cast the nested metadata, and claim the surface.

        Construction is the validation, so `VideoFrame(**bag)` — what
        `read(port, into=VideoFrame)` does — and `from_bag` agree on what a
        frame is rather than one being the stricter spelling.
        """
        if (
            not isinstance(self.surface_id, str)
            or not _is_plain_int(self.width)
            or not _is_plain_int(self.height)
            or not _is_plain_int(self.timestamp_ns)
        ):
            raise ValueError(
                "bag is not a video frame: surface_id must be str and "
                "width/height/timestamp_ns must be int"
            )
        object.__setattr__(self, "fps", _require_int_or_none("fps", self.fps))
        object.__setattr__(
            self,
            "texture_layout",
            _require_int_or_none("texture_layout", self.texture_layout),
        )
        object.__setattr__(self, "color_info", _color_info_or_none(self.color_info))
        object.__setattr__(
            self,
            "content_light",
            _nested_or_none("content_light", ContentLight, self.content_light),
        )
        object.__setattr__(
            self,
            "mastering_display",
            _nested_or_none("mastering_display", MasteringDisplay, self.mastering_display),
        )
        # The frame's own field, and its whole lifetime protocol: this object
        # going away is what releases the claim.
        object.__setattr__(
            self,
            "_check_out_lease_on_this_frames_surface",
            _claim_on_the_surface_a_frame_names(self.surface_id),
        )

    @classmethod
    def from_bag(cls, bag: Mapping[str, Any]) -> "VideoFrame":
        """Construct from a bag dict, raising ValueError on missing or
        mistyped keys — required and optional alike.

        Keys a frame does not declare are ignored: the bag is an open map, and
        a producer may carry more than this cast reads.
        """
        try:
            surface_id = bag["surface_id"]
            width = bag["width"]
            height = bag["height"]
            timestamp_ns = bag["timestamp_ns"]
        except KeyError as missing:
            raise ValueError(
                f"bag is not a video frame: missing key {missing.args[0]!r}"
            ) from None
        return cls(
            surface_id=surface_id,
            width=width,
            height=height,
            timestamp_ns=timestamp_ns,
            fps=bag.get("fps"),
            color_info=bag.get("color_info"),
            content_light=bag.get("content_light"),
            mastering_display=bag.get("mastering_display"),
            texture_layout=bag.get("texture_layout"),
        )

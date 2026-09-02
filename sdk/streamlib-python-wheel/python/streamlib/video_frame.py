# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The optional typed cast for a video-frame bag.

A link carries a self-describing bag (a dict); its keys are the contract and
reading them directly is always enough. Casting is the opt-in dial for
consumers that want construction-time validation and attribute access instead
of key lookups. Pixels never ride the bag — the frame references a GPU surface
by `surface_id`, resolved out-of-band.

Casting also buys the frame's own lifetime and its pixels: read as a
`VideoFrame`, the frame babysits its buffer and speaks `__dlpack__` itself, so
`torch.from_dlpack(frame)` works straight off the read; `frame.writable()` is
the GPU write door and `frame.cpu()` the CPU one, whose name is the whole
warning. All of it comes from `ClaimedSurfacePixelAccess`, the piece the wheel
ships for any cast type to compose — this class is built from it like any
other, which is why reading the bag as a dict stays first-class and
unpenalized.
"""

from __future__ import annotations

import typing
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Literal, cast

from .claimed_surface_pixel_access import ClaimedSurfacePixelAccess

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

# The keys without which there is no frame to speak of. Everything else the
# bag carries is optional, this cast's or somebody else's.
_REQUIRED_BAG_KEYS = ("surface_id", "width", "height", "timestamp_ns")

# How every refusal from this cast names what the bag failed to be. Spelled
# once because `color_info_or_none` serves the encoded-frame cast too, which
# names itself differently.
VIDEO_FRAME_REFUSAL_SUBJECT = "bag is not a video frame"


def _is_plain_int(value: Any) -> bool:
    """`bool` is an `int` subclass; a frame dimension of `True` is a bug."""
    return isinstance(value, int) and not isinstance(value, bool)


def _require_int_or_none(key: str, value: Any) -> "int | None":
    if value is not None and not _is_plain_int(value):
        raise ValueError(
            f"{VIDEO_FRAME_REFUSAL_SUBJECT}: {key!r} must be int or absent"
        )
    return value


def _require_mapping(
    refusal_subject: str, key: str, value: Any
) -> "Mapping[str, Any]":
    if not isinstance(value, Mapping):
        raise ValueError(f"{refusal_subject}: {key!r} must be a mapping or absent")
    return value


def _cast_nested(
    key: str, nested_type: "type[_NestedCast]", nested_bag: Mapping[str, Any]
) -> "_NestedCast":
    """Construct a nested cast, keeping the ValueError contract of `from_bag`."""
    try:
        return nested_type(**nested_bag)
    except TypeError as construction_error:
        raise ValueError(
            f"{VIDEO_FRAME_REFUSAL_SUBJECT}: {key!r} is malformed "
            f"({construction_error})"
        ) from None


def _nested_or_none(
    key: str, nested_type: "type[_NestedCast]", value: Any
) -> "_NestedCast | None":
    """Nested metadata as its own type, whether it arrived already cast or as
    the bag's nested map."""
    if value is None or isinstance(value, nested_type):
        return value
    return _cast_nested(
        key, nested_type, _require_mapping(VIDEO_FRAME_REFUSAL_SUBJECT, key, value)
    )


def color_info_or_none(
    refusal_subject: str, key: str, value: Any
) -> "ColorInfo | None":
    """`ColorInfo` reads field by field rather than by construction: every
    field is optional and a bag may carry a key this version does not know,
    where the H.273 tuple's absent-means-unspecified rule still applies.

    `refusal_subject` and `key` are the caller's because the same four-tuple
    rides a video-frame bag as `color_info` and an encoded-frame bag as
    `color`.
    """
    if value is None or isinstance(value, ColorInfo):
        return value
    color_info_bag = _require_mapping(refusal_subject, key, value)
    return ColorInfo(
        primaries=cast("Primaries | None", color_info_bag.get("primaries")),
        transfer=cast("Transfer | None", color_info_bag.get("transfer")),
        matrix=cast("Matrix | None", color_info_bag.get("matrix")),
        range=cast("Range | None", color_info_bag.get("range")),
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
class VideoFrame(ClaimedSurfacePixelAccess):
    """A video-frame bag, cast: GPU surface reference plus per-frame metadata.

    ``surface_id`` is the handoff contract; ``timestamp_ns`` (the machine's
    monotonic clock in nanoseconds, comparable across every process on the
    host) is the ordering primitive.

    Read through ``ctx.inputs.read(port, into=VideoFrame)``, the frame also
    holds its surface still: the producer cannot recycle those pixels while
    this object lives, and letting it go releases them. There is nothing to
    call, and holding a frame for longer costs the producer memory and then its
    own frames — never another processor's cadence.

    Such a frame is a DLPack producer in its own right — ``torch.from_dlpack``
    consumes it directly, GPU-resident — and carries the two write doors,
    ``writable()`` and ``cpu()``, because it composes
    ``ClaimedSurfacePixelAccess`` like any user-authored cast type can.
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
        """Validate and cast the nested metadata, then hand the settled values
        to the composable, which assigns them and claims the surface.

        Written out rather than generated because the bag is an open map: a
        producer may carry keys this cast does not read, and the day one adds
        a key must not be the day every `read(port, into=VideoFrame)` starts
        raising — which would take the frame's lifetime protection with it.
        Construction is the validation, so that spelling and `from_bag` are
        one path rather than two that can drift.
        """
        del keys_this_cast_does_not_read
        if (
            not isinstance(surface_id, str)
            or not _is_plain_int(width)
            or not _is_plain_int(height)
            or not _is_plain_int(timestamp_ns)
        ):
            raise ValueError(
                f"{VIDEO_FRAME_REFUSAL_SUBJECT}: surface_id must be str and "
                f"width/height/timestamp_ns must be int"
            )
        super().__init__(
            surface_id=surface_id,
            width=width,
            height=height,
            timestamp_ns=timestamp_ns,
            fps=_require_int_or_none("fps", fps),
            texture_layout=_require_int_or_none("texture_layout", texture_layout),
            color_info=color_info_or_none(
                VIDEO_FRAME_REFUSAL_SUBJECT, "color_info", color_info
            ),
            content_light=_nested_or_none("content_light", ContentLight, content_light),
            mastering_display=_nested_or_none(
                "mastering_display", MasteringDisplay, mastering_display
            ),
        )

    @classmethod
    def from_bag(cls, bag: Mapping[str, Any]) -> "VideoFrame":
        """Construct from a bag dict, raising ValueError on missing or
        mistyped keys — required and optional alike.

        The same *validation* `read(port, into=VideoFrame)` performs, so the
        two spellings cannot disagree about what a valid frame is; this one
        only names the missing key, which keyword construction reports in
        Python's words.

        They do differ in one respect, and it is the frame's lifetime. The
        claim on the surface follows the *read*, not the spelling: it is
        offered for the duration of a `read(port, into=…)` construction and
        withdrawn the moment that returns. So calling this on a bag you are
        already holding takes no claim, and its pixels last only as long as
        pool depth gives them — the same protection an untyped read gets.
        Called from inside a type that a typed read is constructing, it takes
        one like any other frame built under that offer; the offer is open to
        every class the read reaches, with no registration and no privileged
        type.

        The unclaimed case is a choice, not a lapse — a hand-rolled bag may
        name no live surface at all. To hold a frame past the producer's ring,
        read it with `into=VideoFrame`, or compose `ClaimedSurfacePixelAccess`
        in your own type, which takes the claim on the same terms.
        """
        missing = [key for key in _REQUIRED_BAG_KEYS if key not in bag]
        if missing:
            raise ValueError(
                f"{VIDEO_FRAME_REFUSAL_SUBJECT}: missing key {missing[0]!r}"
            )
        return cls(**bag)

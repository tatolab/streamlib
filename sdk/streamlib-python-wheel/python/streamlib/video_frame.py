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
from .log import warn

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


_a_refused_claim_has_been_reported = False


def _report_the_first_refused_claim(surface_id: str, refusal: BaseException) -> None:
    """Say once, per process, that frames are arriving unprotected.

    Once and not per frame: this is the per-frame path, and a helper's records
    cross to the parent, so a refusal that persists would cost more in logging
    than the claim it is reporting on. Silence would be worse than either — the
    whole lifetime contract can be off with no other signal.
    """
    global _a_refused_claim_has_been_reported
    if _a_refused_claim_has_been_reported:
        return
    _a_refused_claim_has_been_reported = True
    warn(
        "a frame could not claim its surface, so the producer may recycle it while this "
        "processor is still holding the frame; frames are protected by pool depth alone "
        "until this clears. Not reported again in this process.",
        surface_id=surface_id,
        refusal=str(refusal),
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
    except Exception as refusal:  # noqa: BLE001 — see below
        # Deliberately every failure, not just the refusals this path is known
        # to raise today: the claim crosses a socket, and whatever comes back
        # from below it, none of it makes the delivered bag unreadable. An
        # unclaimed frame falls back to the protection pool depth gives it,
        # which is what an untyped read gets; raising here would turn a
        # delivered frame into an exception at the read. Nothing is hidden by
        # the breadth — the first one is reported.
        _report_the_first_refused_claim(surface_id, refusal)
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


@dataclass(frozen=True, init=False)
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
        """Validate, cast the nested metadata, and claim the surface when a
        read is offering the claim.

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
                "bag is not a video frame: surface_id must be str and "
                "width/height/timestamp_ns must be int"
            )
        # `object.__setattr__` throughout because the frame is frozen: the
        # generated `__setattr__` refuses, and freezing is what makes a
        # delivered frame safe to hand around.
        assign = object.__setattr__
        assign(self, "surface_id", surface_id)
        assign(self, "width", width)
        assign(self, "height", height)
        assign(self, "timestamp_ns", timestamp_ns)
        assign(self, "fps", _require_int_or_none("fps", fps))
        assign(self, "texture_layout", _require_int_or_none("texture_layout", texture_layout))
        assign(self, "color_info", _color_info_or_none(color_info))
        assign(
            self,
            "content_light",
            _nested_or_none("content_light", ContentLight, content_light),
        )
        assign(
            self,
            "mastering_display",
            _nested_or_none("mastering_display", MasteringDisplay, mastering_display),
        )
        # The frame's own field, and its whole lifetime protocol: this object
        # going away is what releases the claim.
        assign(
            self,
            "_check_out_lease_on_this_frames_surface",
            _claim_on_the_surface_a_frame_names(surface_id),
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
        claim on the surface is offered by the read, so it is taken only when
        the read builds the frame. Calling this on a bag you are already
        holding claims nothing: the frame is as valid, and its pixels last
        only as long as pool depth gives them — the same protection an
        untyped read gets. That is a choice, not a lapse. Hold a frame past
        the producer's ring and read it with `into=VideoFrame`, or take the
        claim yourself through `gpu_limited_access_of_the_typed_read_in_progress()`
        in your own type's constructor.
        """
        missing = [key for key in _REQUIRED_BAG_KEYS if key not in bag]
        if missing:
            raise ValueError(f"bag is not a video frame: missing key {missing[0]!r}")
        return cls(**bag)

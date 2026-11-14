# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""An output port's stream facts: what its family states about the stream it carries.

Image and Packets outputs carry facts; `connect` never compares them. Replace
an output's facts while running with `ctx.outputs.set_stream_facts(port_name,
facts)` and read the ones feeding an input with
`ctx.inputs.upstream_stream_facts(port_name)`. Each field mirrors the runtime's
wire form, so `to_wire()` and `stream_facts_from_wire()` round-trip.
"""

from __future__ import annotations

import dataclasses
from typing import Any, TypeAlias

__all__ = [
    "ImageStreamContentLightLevel",
    "ImageStreamFacts",
    "ImageStreamFrameRate",
    "ImageStreamMasteringDisplayColourVolume",
    "PacketsStreamFacts",
    "PortStreamFacts",
    "stream_facts_from_wire",
]


@dataclasses.dataclass(frozen=True)
class ImageStreamFrameRate:
    """A frame rate, as a rational number of frames per second."""

    numerator: int
    denominator: int = 1


@dataclasses.dataclass(frozen=True)
class ImageStreamContentLightLevel:
    """An HDR stream's content light level in cd/m² (CTA-861.3)."""

    max_content_light_level: int
    max_frame_average_light_level: int


@dataclasses.dataclass(frozen=True)
class ImageStreamMasteringDisplayColourVolume:
    """The mastering display an HDR stream was graded on (SMPTE ST 2086), in the H.265 SEI's units.

    Primaries are in green, blue, red order; chromaticity is in steps of
    0.00002 and luminance in steps of 0.0001 cd/m².
    """

    display_primaries_x: "tuple[int, int, int]"
    display_primaries_y: "tuple[int, int, int]"
    white_point_x: int
    white_point_y: int
    max_display_mastering_luminance: int
    min_display_mastering_luminance: int


@dataclasses.dataclass(frozen=True)
class ImageStreamFacts:
    """What an Image output states about its stream; every field optional."""

    frame_rate: "ImageStreamFrameRate | None" = None
    content_light_level: "ImageStreamContentLightLevel | None" = None
    mastering_display_colour_volume: "ImageStreamMasteringDisplayColourVolume | None" = None

    def to_wire(self) -> "dict[str, Any]":
        """The facts in the runtime's wire form, tagged `family: image`."""
        return _wire_form_of("image", self)


@dataclasses.dataclass(frozen=True)
class PacketsStreamFacts:
    """What a Packets output states before its first unit; every field optional."""

    rfc6381_codec: "str | None" = None
    """The codec with profile and level as RFC 6381 spells it, e.g. `avc1.64001f`."""
    coded_width: "int | None" = None
    coded_height: "int | None" = None
    sample_rate: "int | None" = None
    channel_count: "int | None" = None
    codec_configuration_hex: "str | None" = None
    """The codec's out-of-band configuration record (`avcC`, `OpusHead`) as lowercase hex."""

    def to_wire(self) -> "dict[str, Any]":
        """The facts in the runtime's wire form, tagged `family: packets`."""
        return _wire_form_of("packets", self)


PortStreamFacts: TypeAlias = "ImageStreamFacts | PacketsStreamFacts"
"""Either family's facts."""


def _wire_value_of(value: Any) -> Any:
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        return {
            field.name: _wire_value_of(getattr(value, field.name))
            for field in dataclasses.fields(value)
            if getattr(value, field.name) is not None
        }
    if isinstance(value, tuple):
        return list(value)
    return value


def _wire_form_of(family: str, facts: Any) -> "dict[str, Any]":
    return {"family": family, **_wire_value_of(facts)}


def stream_facts_from_wire(wire: "dict[str, Any]") -> PortStreamFacts:
    """The typed facts the runtime's wire form `wire` spells."""
    fields = {key: value for key, value in wire.items() if key != "family"}
    family = wire.get("family")
    if family == "image":
        frame_rate = fields.get("frame_rate")
        content_light_level = fields.get("content_light_level")
        mastering = fields.get("mastering_display_colour_volume")
        return ImageStreamFacts(
            frame_rate=ImageStreamFrameRate(**frame_rate) if frame_rate else None,
            content_light_level=(
                ImageStreamContentLightLevel(**content_light_level) if content_light_level else None
            ),
            mastering_display_colour_volume=(
                ImageStreamMasteringDisplayColourVolume(
                    **{
                        **mastering,
                        "display_primaries_x": tuple(mastering["display_primaries_x"]),
                        "display_primaries_y": tuple(mastering["display_primaries_y"]),
                    }
                )
                if mastering
                else None
            ),
        )
    if family == "packets":
        return PacketsStreamFacts(**fields)
    raise ValueError(f"stream facts carry family image or packets, got {family!r}")

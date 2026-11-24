# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""An output port's stream facts: the typed dataclasses and their wire form."""

from __future__ import annotations

import pytest

import tatolab.stream
from tatolab.stream.stream_facts import (
    ImageStreamContentLightLevel,
    ImageStreamFacts,
    ImageStreamFrameRate,
    ImageStreamMasteringDisplayColourVolume,
    PacketsStreamFacts,
    stream_facts_from_wire,
)


def test_packets_facts_reach_the_wire_tagged_by_family_with_the_unstated_left_out():
    facts = PacketsStreamFacts(rfc6381_codec="avc1.64001f", coded_width=1280, coded_height=720)
    assert facts.to_wire() == {
        "family": "packets",
        "rfc6381_codec": "avc1.64001f",
        "coded_width": 1280,
        "coded_height": 720,
    }


def test_image_facts_round_trip_through_their_wire_form():
    facts = ImageStreamFacts(
        frame_rate=ImageStreamFrameRate(30000, 1001),
        content_light_level=ImageStreamContentLightLevel(1000, 400),
        mastering_display_colour_volume=ImageStreamMasteringDisplayColourVolume(
            display_primaries_x=(8500, 6550, 35400),
            display_primaries_y=(39850, 2300, 14600),
            white_point_x=15635,
            white_point_y=16450,
            max_display_mastering_luminance=10_000_000,
            min_display_mastering_luminance=50,
        ),
    )
    wire = facts.to_wire()
    assert wire["family"] == "image"
    assert wire["frame_rate"] == {"numerator": 30000, "denominator": 1001}
    assert wire["mastering_display_colour_volume"]["display_primaries_x"] == [8500, 6550, 35400]
    assert stream_facts_from_wire(wire) == facts


def test_facts_graph_names_by_their_pin_type_read_as_the_facts_of_their_family():
    encoded = {"family": "packets", "rfc6381_codec": "avc1.64001f", "coded_width": 1280}
    for pin_type in ("encoded_video", "encoded_audio"):
        assert stream_facts_from_wire({"type": pin_type, **encoded}) == PacketsStreamFacts(
            rfc6381_codec="avc1.64001f", coded_width=1280
        )
    assert stream_facts_from_wire(
        {"type": "texture", "family": "image", "frame_rate": {"numerator": 30, "denominator": 1}}
    ) == ImageStreamFacts(frame_rate=ImageStreamFrameRate(numerator=30, denominator=1))


def test_facts_of_a_family_with_none_are_refused():
    with pytest.raises(ValueError, match="records"):
        stream_facts_from_wire({"family": "records"})


def test_the_stream_facts_module_is_exported():
    assert "stream_facts" in tatolab.stream.__all__
    assert "catalog" not in tatolab.stream.__all__

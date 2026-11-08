# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""An output port's stream facts: the typed dataclasses, their wire form, and `rate=` on Image."""

from __future__ import annotations

import pytest

import tatolab.stream
from tatolab.stream import Channels, Image, Output, Packets, Records, from_config, node
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


def test_facts_of_a_family_with_none_are_refused():
    with pytest.raises(ValueError, match="records"):
        stream_facts_from_wire({"family": "records"})


def test_rate_on_an_image_output_declares_its_frame_rate():
    @node.process(execution="manual")
    class Camera:
        stated = Output(Image, rate=30)
        rational = Output(Image, rate=ImageStreamFrameRate(30000, 1001))
        configured = Output(Image, rate=from_config("frames_per_second"))
        audio = Output(Channels, names=("left",), rate=48_000)

    families = {port["name"]: port["family"] for port in Camera.__tatolab_node_output_ports__}
    assert families["stated"]["frame_rate"] == {"stated": {"numerator": 30, "denominator": 1}}
    assert families["rational"]["frame_rate"] == {
        "stated": {"numerator": 30000, "denominator": 1001}
    }
    assert families["configured"]["frame_rate"] == {
        "from_config": {"config_key": "frames_per_second"}
    }
    assert families["audio"]["rate"] == {"stated": 48_000}
    assert "frame_rate" not in families["audio"]


def test_rate_on_a_family_that_takes_none_is_refused_naming_it():
    with pytest.raises((TypeError, ValueError), match="rate"):

        @node.process(execution="manual")
        class Encoder:
            encoded = Output(Packets, rate=30)


def test_a_frame_rate_on_a_channels_output_is_refused():
    with pytest.raises(ValueError, match="Image output"):
        Output(Channels, rate=ImageStreamFrameRate(30, 1))


def test_an_output_port_takes_no_catalog_entry():
    with pytest.raises(TypeError, match="catalog"):
        Output(Records, catalog={"section": "json"})  # type: ignore[call-arg]


def test_the_stream_facts_module_is_exported():
    assert "stream_facts" in tatolab.stream.__all__
    assert "catalog" not in tatolab.stream.__all__

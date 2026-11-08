# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""An output port's catalog entry: the entry dataclasses and their declaration.

The runtime parses each entry with hang's own catalog types; what is proven
here is the Python half — that each entry spells hang's field names, leaves
unstated fields out rather than sending nulls, and reaches the port's stamp.
"""

from __future__ import annotations

import pytest

import tatolab.stream
from tatolab.stream import Image, Input, Output, Packets, node
from tatolab.stream.catalog import (
    AudioEntry,
    BinaryEntry,
    JsonEntry,
    TextEntry,
    VideoEntry,
)


def test_each_entry_spells_hangs_field_names_and_leaves_the_unstated_out():
    assert VideoEntry(codec="avc1.64001f", codedWidth=1280, codedHeight=720).to_dict() == {
        "codec": "avc1.64001f",
        "codedWidth": 1280,
        "codedHeight": 720,
    }
    assert AudioEntry(
        codec="opus", sampleRate=48_000, numberOfChannels=2, description="4f707573"
    ).to_dict() == {
        "codec": "opus",
        "sampleRate": 48_000,
        "numberOfChannels": 2,
        "description": "4f707573",
    }
    assert TextEntry(format="vtt").to_dict() == {"format": "vtt"}
    assert JsonEntry(mode="stream").to_dict() == {"mode": "stream"}
    assert BinaryEntry(mode="snapshot", mime="application/msgpack").to_dict() == {
        "mode": "snapshot",
        "mime": "application/msgpack",
    }
    assert BinaryEntry(mode="snapshot").to_dict() == {"mode": "snapshot"}


def test_each_entry_names_its_section():
    assert [entry_type.section for entry_type in (VideoEntry, AudioEntry, TextEntry, JsonEntry, BinaryEntry)] == [
        "video",
        "audio",
        "text",
        "json",
        "binary",
    ]


def test_an_output_port_stamps_its_catalog_entry_as_section_and_fields():
    @node.process(execution="manual")
    class FrameSource:
        frames = Output(
            Packets,
            description="Frames as length-prefixed records",
            catalog=BinaryEntry(mode="snapshot", mime="application/octet-stream"),
        )

        levels = Output(Packets)

    assert FrameSource.__tatolab_node_output_ports__ == [
        {
            "name": "frames",
            "description": "Frames as length-prefixed records",
            "family": {"family": "packets"},
            "catalog_entry": {
                "section": "binary",
                "entry": {"mode": "snapshot", "mime": "application/octet-stream"},
            },
        },
        {"name": "levels", "description": "", "family": {"family": "packets"}},
    ]


def test_a_catalog_that_is_not_an_entry_is_refused_naming_the_port():
    with pytest.raises(TypeError, match="'frames'.*VideoEntry"):

        @node.process(execution="manual")
        class FrameSource:
            frames = Output(
                Packets,
                catalog={"section": "binary", "entry": {"mode": "snapshot"}},  # type: ignore[arg-type]
            )


def test_an_input_port_takes_no_catalog_entry():
    with pytest.raises(TypeError, match="catalog"):
        Input(Image, delivery="newest", catalog=VideoEntry(codec="avc1.64001f"))  # type: ignore[call-arg]


def test_the_catalog_module_is_exported_with_its_five_entries():
    assert "catalog" in tatolab.stream.__all__
    assert set(tatolab.stream.catalog.__all__) == {
        "AudioEntry",
        "BinaryEntry",
        "CatalogEntry",
        "JsonEntry",
        "TextEntry",
        "VideoEntry",
    }

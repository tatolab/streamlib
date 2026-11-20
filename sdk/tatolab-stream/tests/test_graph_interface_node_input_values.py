# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`GraphInput` and `GraphOutput` declare their settings and their `value` pin, so `add` checks
the input values they are given, and `PinTypeChoice` records the type a `pin_type` chooses."""

from __future__ import annotations

from typing import Any

import pytest

from tatolab.stream import (
    EncodedVideo,
    GraphInput,
    GraphOutput,
    Image,
    PinTypeChoice,
    StreamBuilder,
)


def the_values_recorded_for(node_class: Any, input_values: "dict[str, Any]") -> "dict[str, Any]":
    stream_builder = StreamBuilder("rig")
    stream_builder.add(node_class, input_values=input_values)
    (recorded,) = stream_builder._compiled_graph()["nodes"]
    return recorded["input_values"]


def test_a_chosen_pin_type_is_recorded_as_the_type_it_chooses() -> None:
    recorded = the_values_recorded_for(
        GraphInput,
        {"port": "level", "pin_type": PinTypeChoice(float), "value": 1, "range": (0, 2)},
    )

    assert recorded == {
        "port": "level",
        "pin_type": {"type": "float"},
        "value": 1.0,
        "range": [0.0, 2.0],
    }


def test_a_graph_input_records_its_family_by_name_and_its_default_as_its_value() -> None:
    recorded = the_values_recorded_for(
        GraphInput, {"port": "cue", "family": "records", "default": {"scene": 1}}
    )

    assert recorded == {"port": "cue", "family": "records", "value": {"scene": 1}}
    assert the_values_recorded_for(GraphOutput, {"port": "video", "family": Image}) == {
        "port": "video",
        "family": "image",
    }


def test_a_value_the_chosen_type_does_not_take_is_refused_naming_the_input() -> None:
    with pytest.raises(TypeError, match=r"`GraphInput`'s input `value` is set 'loud'"):
        StreamBuilder("rig").add(
            GraphInput,
            input_values={"port": "level", "pin_type": PinTypeChoice(float), "value": "loud"},
        )


def test_a_setting_a_graph_input_does_not_declare_is_refused_listing_its_inputs() -> None:
    with pytest.raises(ValueError, match=r"declares no input `codec`.*`lifetime`, `pin_type`"):
        StreamBuilder("rig").add(GraphInput, input_values={"port": "level", "codec": "h264"})


def test_a_pin_type_that_is_not_a_choice_is_refused() -> None:
    with pytest.raises(TypeError, match=r"`GraphOutput`'s input `pin_type` is set"):
        StreamBuilder("rig").add(
            GraphOutput, input_values={"port": "video", "pin_type": {"type": "texture"}}
        )


def test_the_value_ports_are_the_names_a_link_takes() -> None:
    stream_builder = StreamBuilder("rig")
    source = stream_builder.add(GraphInput, input_values={"port": "video", "family": Image})
    sink = stream_builder.add(GraphOutput, input_values={"port": "video", "family": Image})

    stream_builder.connect(source.output("value"), sink.input("value"))
    with pytest.raises(ValueError, match=r"declares no output `video`"):
        source.output("video")


def test_a_pin_type_choice_reads_the_type_it_is_written_with() -> None:
    assert PinTypeChoice(float).chosen == {"type": "float"}
    assert PinTypeChoice(EncodedVideo).chosen == {"type": "encoded_video"}
    assert PinTypeChoice(float) == PinTypeChoice(float)

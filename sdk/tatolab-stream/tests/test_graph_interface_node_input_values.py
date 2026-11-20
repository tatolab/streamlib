# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`GraphInput` and `GraphOutput` declare their settings and their `value` pin as fields and
pins, so `add` checks the input values they are given, and `PinTypeChoice` records the type a
`pin_type` chooses. What only the runtime's own nodes declare is refused on an author's class."""

from __future__ import annotations

from typing import Annotated, Any, Literal

import pytest

from tatolab.stream import (
    EncodedVideo,
    GraphInput,
    GraphOutput,
    Image,
    InputPin,
    NodeDeclarationRefusal,
    PinTypeChoice,
    SettingOnly,
    StreamBuilder,
    node,
)
from tatolab.stream._class_node_declaration import read_class_node_ports
from tatolab.stream._input_declaration_annotations import PublishedOnAnOutput
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)
from tatolab.stream._pin_type_choice import PinTypeFromSetting
from tatolab.stream._pin_value_stored_form import PinValueNotTaken, stored_form_of_a_pin_value


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


def test_a_chosen_type_no_family_carries_is_refused_as_the_runtime_refuses_it() -> None:
    with pytest.raises(
        TypeError,
        match=(
            r"chooses List of Float, which no stream's port carries yet\. To fix: choose a type "
            r"a single message carries, such as a Float, a String or a Texture"
        ),
    ):
        PinTypeChoice(list[float])


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


def ports_by_name(node_class: Any, stamp: str) -> "dict[str, dict[str, Any]]":
    return {port["name"]: port for port in getattr(node_class, stamp)}


def test_a_graph_inputs_fields_declare_the_settings_and_the_value_the_runtime_declares() -> None:
    inputs = ports_by_name(GraphInput, NODE_DECLARATION_INPUT_PORTS_STAMP)
    outputs = ports_by_name(GraphInput, NODE_DECLARATION_OUTPUT_PORTS_STAMP)
    set_by_pin_type = {"kind": "set_by_input", "input_name": "pin_type"}

    assert list(inputs) == ["port", "pin_type", "family", "range", "lifetime", "value"]
    assert "pin_default" not in inputs["port"]
    assert inputs["lifetime"]["pin_default"] == "kept"
    assert inputs["pin_type"]["input_mode"] == {"mode": "setting_only", "read": "at_setup"}
    assert inputs["value"]["input_mode"] == {"mode": "setting_only", "read": "each_run"}
    assert inputs["value"]["pin_declaration"]["pin_type"] == set_by_pin_type
    assert inputs["value"]["pin_declaration"]["former_names"] == ["default"]
    assert inputs["value"]["pin_declaration"]["may_be_unset"] is True
    assert list(outputs) == ["value"]
    assert outputs["value"]["pin_declaration"] == {"pin_type": set_by_pin_type, "flow": "value"}
    assert ports_by_name(GraphOutput, NODE_DECLARATION_INPUT_PORTS_STAMP)["value"][
        "pin_declaration"
    ] == {"pin_type": set_by_pin_type, "flow": "value"}


def test_a_port_family_set_on_a_menu_of_other_names_is_refused_as_none_of_its_options() -> None:
    with pytest.raises(PinValueNotTaken, match=r"'image', which is none of its options"):
        stored_form_of_a_pin_value(Image, {"type": "menu", "option_names": ["sine", "square"]})


class _AnAuthorsSettingWithNoDefault:
    radius: Annotated[float, SettingOnly()]


class _AnAuthorsTypeChoice:
    content_type: PinTypeChoice | None = None


class _AnAuthorsPinTypedByASetting:
    content: InputPin[Annotated[object, PinTypeFromSetting("content_type")]]


class _AnAuthorsSettingReadEachRun:
    level: Annotated[float, SettingOnly(read="each_run")] = 0.0


class _AnAuthorsPublishedField:
    level: Annotated[float, PublishedOnAnOutput()] = 0.0


@pytest.mark.parametrize(
    ("node_class", "words"),
    [
        (_AnAuthorsSettingWithNoDefault, "has no default"),
        (_AnAuthorsTypeChoice, "only the runtime's own nodes declare a setting that chooses"),
        (_AnAuthorsPinTypedByASetting, "which only the runtime's own nodes declare yet"),
        (_AnAuthorsSettingReadEachRun, "which only the runtime's own nodes declare"),
        (_AnAuthorsPublishedField, "which only the runtime's own nodes declare"),
    ],
)
def test_what_only_the_runtimes_own_nodes_declare_is_refused_on_an_authors_class(
    node_class: type, words: str
) -> None:
    with pytest.raises(NodeDeclarationRefusal, match=words):
        read_class_node_ports(node_class)


@node
class Tone:
    shape: Annotated[Literal["sine", "square"], SettingOnly()] = "sine"


def test_a_typed_nodes_menu_of_other_names_refuses_a_port_family() -> None:
    with pytest.raises(TypeError, match=r"'image', which is none of its options"):
        StreamBuilder("rig").add(Tone, input_values={"shape": Image})

# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A `@graph_generator` placed by another one is stored inside its graph as a fragment."""

from __future__ import annotations

from typing import Any

import pytest

from stream_graph_builder_nodes import BrightnessReader, FrameInverter
from tatolab.stream import (
    GraphFragmentReference,
    GraphInput,
    GraphOutput,
    Packets,
    SteppedStreamClock,
    StreamBuilder,
    graph_generator,
    run_graph_generator,
)
from tatolab.stream._stream_graph_builder import place_graph_generator

FRAME_INVERTER_TYPE = "stream_graph_builder_nodes:FrameInverter"
BRIGHTNESS_READER_TYPE = "stream_graph_builder_nodes:BrightnessReader"
GRAPH_INPUT_TYPE = "tatolab.stream:GraphInput"
GRAPH_OUTPUT_TYPE = "tatolab.stream:GraphOutput"
CAMERA_NAMES = ["front", "back"]


@graph_generator
def invert_twice(stream_builder: StreamBuilder, strength: int = 4) -> None:
    """Two inverters between one Input and one Output."""
    source = stream_builder.add(GraphInput, config={"port": "video", "family": Packets})
    first = stream_builder.add(FrameInverter, name="first", config={"strength": strength})
    second = stream_builder.add(FrameInverter, name="second")
    stream_builder.connect(source.output("value"), first.input("video_from_upstream"))
    stream_builder.connect(
        first.output("video_to_downstream"), second.input("video_from_upstream")
    )
    sink = stream_builder.add(GraphOutput, config={"port": "video", "family": Packets})
    stream_builder.connect(second.output("video_to_downstream"), sink.input("value"))


@graph_generator
def look(stream_builder: StreamBuilder) -> None:
    source = stream_builder.add(GraphInput, config={"port": "video", "family": Packets})
    soft = stream_builder.add(invert_twice, name="soft", strength=8)
    stream_builder.connect(source.output("value"), soft.input("video"))
    sink = stream_builder.add(GraphOutput, config={"port": "video", "family": Packets})
    stream_builder.connect(soft.output("video"), sink.input("value"))


def place_one_look_per_camera(stream_builder: StreamBuilder, camera_names: list[str]) -> None:
    for camera_name in camera_names:
        placed = stream_builder.add(look, name=f"{camera_name}-look")
        reader = stream_builder.add(BrightnessReader, name=f"{camera_name}-reader")
        stream_builder.connect(placed.output("video"), reader.input("video_from_upstream"))


@graph_generator
def front_rig(stream_builder: StreamBuilder) -> None:
    place_one_look_per_camera(stream_builder, CAMERA_NAMES[:1])


@graph_generator
def back_rig(stream_builder: StreamBuilder) -> None:
    place_one_look_per_camera(stream_builder, CAMERA_NAMES[1:])


@graph_generator
def places_its_caller(stream_builder: StreamBuilder) -> None:
    stream_builder.add(places_itself_through_another)


@graph_generator
def places_itself_through_another(stream_builder: StreamBuilder) -> None:
    stream_builder.add(places_its_caller)


@graph_generator
def names_a_clock(stream_builder: StreamBuilder) -> None:
    stream_builder.run_on_clock(SteppedStreamClock(step_ns=1_000_000))


@graph_generator
def places_one_that_names_a_clock(stream_builder: StreamBuilder) -> None:
    stream_builder.add(names_a_clock)


INVERT_TWICE_FRAGMENT: dict[str, Any] = {
    "nodes": [
        {"name": "graphinput", "type": GRAPH_INPUT_TYPE,
         "config": {"port": "video", "family": "packets"}},
        {"name": "first", "type": FRAME_INVERTER_TYPE, "config": {"strength": 8}},
        {"name": "second", "type": FRAME_INVERTER_TYPE, "config": {}},
        {"name": "graphoutput", "type": GRAPH_OUTPUT_TYPE,
         "config": {"port": "video", "family": "packets"}},
    ],
    "links": [
        {"source": {"node": "graphinput", "port": "value"},
         "target": {"node": "first", "port": "video_from_upstream"}},
        {"source": {"node": "first", "port": "video_to_downstream"},
         "target": {"node": "second", "port": "video_from_upstream"}},
        {"source": {"node": "second", "port": "video_to_downstream"},
         "target": {"node": "graphoutput", "port": "value"}},
    ],
}


def look_placed_as(name: str) -> dict[str, Any]:
    return {
        "name": name,
        "type": "test_graph_generator_fragments:look",
        "config": {},
        "fragment": {
            "nodes": [
                {"name": "graphinput", "type": GRAPH_INPUT_TYPE,
                 "config": {"port": "video", "family": "packets"}},
                {"name": "soft", "type": "test_graph_generator_fragments:invert_twice",
                 "config": {"strength": 8}, "fragment": INVERT_TWICE_FRAGMENT},
                {"name": "graphoutput", "type": GRAPH_OUTPUT_TYPE,
                 "config": {"port": "video", "family": "packets"}},
            ],
            "links": [
                {"source": {"node": "graphinput", "port": "value"},
                 "target": {"node": "soft", "port": "video"}},
                {"source": {"node": "soft", "port": "video"},
                 "target": {"node": "graphoutput", "port": "value"}},
            ],
        },
    }


def test_a_generator_looping_over_a_list_places_one_fragment_in_each_stream() -> None:
    for rig, camera_name in [(front_rig, "front"), (back_rig, "back")]:
        assert run_graph_generator(rig)["nodes"] == [
            look_placed_as(f"{camera_name}-look"),
            {"name": f"{camera_name}-reader", "type": BRIGHTNESS_READER_TYPE, "config": {}},
        ]
        assert run_graph_generator(rig)["links"] == [
            {"source": {"node": f"{camera_name}-look", "port": "video"},
             "target": {"node": f"{camera_name}-reader", "port": "video_from_upstream"}},
        ]


def test_a_placed_generator_answers_its_interface_ports() -> None:
    stream_builder = StreamBuilder("rig")
    placed = stream_builder.add(invert_twice)

    assert isinstance(placed, GraphFragmentReference)
    assert placed.name == "invert_twice"
    assert (placed.input_port_names, placed.output_port_names) == (("video",), ("video",))


def test_a_port_the_fragment_does_not_declare_is_refused_naming_those_it_does() -> None:
    placed = StreamBuilder("rig").add(invert_twice)

    with pytest.raises(ValueError) as refusal:
        placed.input("audio")

    assert str(refusal.value) == (
        "fragment `invert_twice` declares no Input `audio`; its graph generator's Inputs "
        "are: `video`"
    )


def test_a_generator_that_places_itself_is_refused_naming_the_chain() -> None:
    with pytest.raises(ValueError) as refusal:
        run_graph_generator(places_its_caller)

    assert str(refusal.value).startswith(
        "graph generator `test_graph_generator_fragments:places_its_caller` places itself: "
        "places_its_caller → places_itself_through_another → places_its_caller."
    )


def test_a_generator_placed_as_a_fragment_names_no_clock() -> None:
    with pytest.raises(ValueError, match="names a clock only when it fills a stream"):
        run_graph_generator(places_one_that_names_a_clock)
    assert run_graph_generator(names_a_clock)["clock"]["step_ns"] == 1_000_000


def test_a_setting_the_generator_does_not_promote_is_refused_naming_those_it_does() -> None:
    with pytest.raises(TypeError) as refusal:
        StreamBuilder("rig").add(invert_twice, radius=3)

    assert str(refusal.value) == (
        "graph generator `test_graph_generator_fragments:invert_twice`: it promotes no "
        "`radius`. The settings it promotes are: `strength`"
    )


def test_a_node_class_given_promoted_settings_is_refused_naming_config() -> None:
    with pytest.raises(TypeError, match=r"pass its settings as `config=\{\.\.\.\}`"):
        StreamBuilder("rig").add(FrameInverter, strength=3)  # pyright: ignore[reportCallIssue]


def test_a_generator_given_config_is_refused_naming_keyword_settings() -> None:
    with pytest.raises(TypeError, match="takes its promoted settings as keyword arguments"):
        StreamBuilder("rig").add(invert_twice, config={"strength": 3})  # pyright: ignore[reportArgumentType, reportCallIssue]


def test_a_fragment_nested_deeper_than_a_graph_document_reads_is_refused_naming_it() -> None:
    namespace: dict[str, Any] = {"__name__": "deep_rig", "graph_generator": graph_generator}
    generator_count = 45
    for index in range(generator_count):
        places_the_next = (
            f"    stream_builder.add(level_{index + 1}, name='l{index + 1}')\n"
            if index + 1 < generator_count
            else "    pass\n"
        )
        exec(
            f"@graph_generator\ndef level_{index}(stream_builder):\n{places_the_next}",
            namespace,
        )

    with pytest.raises(ValueError) as refusal:
        run_graph_generator(namespace["level_0"])

    message = str(refusal.value)
    assert message.startswith("fragment `l1/l2/l3/")
    assert "containers deep from the graph's root, and a graph document nests at most 126" in (
        message
    )


@pytest.mark.parametrize("reserved_setting", ["name", "config"])
def test_a_generator_promoting_a_keyword_add_takes_itself_is_refused(
    reserved_setting: str,
) -> None:
    namespace: dict[str, Any] = {"__name__": "rig_streams"}
    exec(f"def main(stream_builder, {reserved_setting}=None):\n    pass\n", namespace)

    with pytest.raises(TypeError) as refusal:
        graph_generator(namespace["main"])

    assert f"promotes `{reserved_setting}`, which `stream_builder.add` takes itself" in str(
        refusal.value
    )


@graph_generator
def invert_twice_grouped(stream_builder: StreamBuilder) -> None:
    first = stream_builder.add(FrameInverter, name="first")
    second = stream_builder.add(FrameInverter, name="second")
    stream_builder.connect(
        first.output("video_to_downstream"), second.input("video_from_upstream")
    )
    stream_builder.group(
        "invert", [first, second], description="inverts twice", colour="#f97316"
    )


@graph_generator
def groups_a_node_twice(stream_builder: StreamBuilder) -> None:
    first = stream_builder.add(FrameInverter, name="first")
    stream_builder.group("invert", [first])
    stream_builder.group("again", [first])


def test_a_generator_groups_nodes_of_its_fragment_by_name_under_a_title() -> None:
    placed = place_graph_generator(invert_twice_grouped)

    assert placed["fragment"]["groups"] == [
        {
            "title": "invert",
            "description": "inverts twice",
            "colour": "#f97316",
            "nodes": ["first", "second"],
        }
    ]


def test_a_node_grouped_twice_is_refused_naming_the_group_it_belongs_to() -> None:
    with pytest.raises(ValueError) as refusal:
        place_graph_generator(groups_a_node_twice)

    assert str(refusal.value) == (
        "node `first` already belongs to group 'invert' of fragment `groups_a_node_twice`; "
        "a node belongs to one group"
    )


def test_a_stream_groups_no_node_of_its_own() -> None:
    with pytest.raises(ValueError, match="a stream's own groups are made with `add_group`"):
        run_graph_generator(invert_twice_grouped)

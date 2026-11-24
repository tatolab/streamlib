# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`StreamBuilder.add` records a node's `input_values` under the graph document's one key for them, and checks them on a typed node.

`config=` is the older spelling of the same argument: it records the same values on a node
that declares no pin types, and is refused on a typed one.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import pytest

from stream_graph_builder_nodes import TintedLevel
from stream_graph_builder_streams import StandInMarker
from tatolab.stream import (
    Color,
    MakeBundle,
    InputPin,
    OutputPin,
    PinGroup,
    PinTypeChoice,
    StreamBuilder,
    Texture,
    graph_generator,
    node,
    run_graph_generator,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)
from tatolab.stream._typed_node_pins import TypedNodePins

TINTED_LEVEL_TYPE = "stream_graph_builder_nodes:TintedLevel"


def the_values_recorded_for(node_class: Any, **add_arguments: Any) -> "dict[str, Any]":
    stream_builder = StreamBuilder("rig")
    stream_builder.add(node_class, **add_arguments)
    (recorded,) = stream_builder._compiled_graph()["nodes"]
    assert "config" not in recorded, recorded
    return recorded["input_values"]


@graph_generator
def tinted(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TintedLevel, input_values={"level": 0.25, "voices": 3})


def test_input_values_are_recorded_under_the_documents_one_key_for_them() -> None:
    (recorded,) = run_graph_generator(tinted)["nodes"]

    assert recorded == {
        "name": "tintedlevel",
        "type": TINTED_LEVEL_TYPE,
        "input_values": {"level": 0.25, "voices": 3},
    }


@pytest.mark.parametrize("node_class", [StandInMarker, TintedLevel], ids=["untyped", "typed"])
def test_config_is_refused_naming_input_values(node_class: Any) -> None:
    """`config=` is retired on every node, so it fails where it is written.

    Mental-revert: reading `config=` again records the values on the untyped node.
    """
    with pytest.raises(TypeError, match=r"`config=` is retired, and .* takes its values as `input_values="):
        StreamBuilder("rig").add(node_class, config={"level": 0.25})  # pyright: ignore[reportCallIssue]


def test_a_typed_nodes_values_are_recorded_in_the_stored_form_of_each_pin_type() -> None:
    recorded = the_values_recorded_for(
        TintedLevel,
        input_values={
            "level": 1,
            "tint": Color(0.5, 0.25, 0.0),
            "waveform": "square",
            "steps": (0, 0.5),
            "detune": None,
        },
    )

    assert recorded == {
        "level": 1.0,
        "tint": Color(0.5, 0.25, 0.0).stored_value(),
        "waveform": "square",
        "steps": [0.0, 0.5],
        "detune": None,
    }


@pytest.mark.parametrize(
    ("input_values", "refusal"),
    [
        ({"lvl": 0.5}, "`TintedLevel` declares no input `lvl`; its inputs are: `detune`, `level`, "),
        ({"level": "loud"}, "`TintedLevel`'s input `level` is set 'loud', which is not a Float"),
        ({"level": 2.0}, "is set 2.0, outside its `NumberRange(minimum=0.0, maximum=1.0)`"),
        ({"voices": 300}, "is set 300, outside the -128 to 127 its exact size `int8` holds"),
        ({"waveform": "saw"}, "is set 'saw', which is none of its options, 'sine', 'square'"),
        ({"tint": (1.0, 0.0, 0.0, 1.0)}, "is set (1.0, 0.0, 0.0, 1.0), which is not a Color"),
        ({"steps": [0.0, "half"]}, "`TintedLevel`'s input `steps` is set 'half', which is not a Float"),
        ({"level": float("nan")}, "is set nan, and a stored number is finite"),
        ({"level": None}, "`TintedLevel`'s input `level` is set `None`, and it may not be unset"),
    ],
)
def test_a_typed_node_refuses_a_name_it_does_not_declare_and_a_value_of_the_wrong_type(
    input_values: "dict[str, Any]", refusal: str
) -> None:
    stream_builder = StreamBuilder("rig")

    with pytest.raises((TypeError, ValueError)) as refused:
        stream_builder.add(TintedLevel, input_values=input_values)

    assert refusal in str(refused.value)
    assert stream_builder._compiled_graph()["nodes"] == [], "a refused add records nothing"


def test_an_input_named_twice_by_its_cast_name_is_refused() -> None:
    with pytest.raises(ValueError, match="is set input `level` twice"):
        StreamBuilder("rig").add(TintedLevel, input_values={"level": 0.25, "Level": 0.5})


def test_input_values_that_are_not_a_mapping_are_refused() -> None:
    with pytest.raises(TypeError, match="input_values must be a mapping of input names"):
        StreamBuilder("rig").add(TintedLevel, input_values=[("level", 0.25)])  # pyright: ignore[reportArgumentType, reportCallIssue]


def test_a_typed_nodes_port_names_are_checked_against_its_declaration() -> None:
    tinted = StreamBuilder("rig").add(TintedLevel)

    assert tinted.input("Level").port_name == "level"
    assert tinted.output("result").port_name == "result"
    with pytest.raises(ValueError, match=r"`TintedLevel` declares no output `level`; its outputs are: `result`"):
        tinted.output("level")
    with pytest.raises(ValueError, match="`TintedLevel` declares no input `tone`"):
        tinted.input("tone")


def test_a_node_declaring_no_pin_types_names_any_port_unchecked() -> None:
    inverter = StreamBuilder("rig").add(StandInMarker)

    assert inverter.output("anything").port_name == "anything"
    assert inverter.input("whatever").port_name == "whatever"


class _StampedPorts:
    """A node class stamped by hand, with what bare `@node` cannot declare yet."""

    def __init__(self, *input_ports: "dict[str, Any]") -> None:
        setattr(self, NODE_DECLARATION_INPUT_PORTS_STAMP, list(input_ports))
        setattr(self, NODE_DECLARATION_OUTPUT_PORTS_STAMP, [])


def test_an_input_carrying_a_stream_is_refused_a_value_naming_the_link() -> None:
    typed_pins = TypedNodePins.of(
        _StampedPorts(
            {
                "name": "frame",
                "pin_declaration": {
                    "pin_type": {"kind": "is", "pin_type": {"type": "texture"}},
                    "flow": "stream",
                },
            }
        ),
        "Blur",
    )
    assert typed_pins is not None

    with pytest.raises(TypeError, match="input `frame` carries a stream and holds no value"):
        typed_pins.stored_input_values({"frame": 1.0})


def test_an_input_finds_its_values_by_a_name_it_had() -> None:
    typed_pins = TypedNodePins.of(
        _StampedPorts(
            {
                "name": "gain",
                "pin_declaration": {
                    "pin_type": {"kind": "is", "pin_type": {"type": "float"}},
                    "flow": "value",
                    "former_names": ["volume"],
                },
            }
        ),
        "Amplifier",
    )
    assert typed_pins is not None

    assert typed_pins.stored_input_values({"volume": 2}) == {"gain": 2.0}


@dataclass(frozen=True)
class _Layer:
    picture: InputPin[Texture]
    opacity: float = 1.0


@node
class _LayeredOver:
    """Lays each layer's picture over the one below it."""

    layers: PinGroup[_Layer]
    result: OutputPin[Texture]

    def process(self, ctx: Any) -> None:
        pass


def test_a_slot_port_of_a_pin_group_takes_a_value_of_its_fields_type_and_a_link_in_any_slot() -> None:
    """Fail-without-fix: find ports among the node's own pins alone and `layers.2.opacity` is
    refused as no input of the node."""
    stream_builder = StreamBuilder("rig")
    layered = stream_builder.add(_LayeredOver, input_values={"layers.2.opacity": 1})

    assert layered.input("layers.11.picture").port_name == "layers.11.picture"
    (recorded,) = stream_builder._compiled_graph()["nodes"]
    assert recorded["input_values"] == {"layers.2.opacity": 1.0}


@pytest.mark.parametrize(
    ("set_on", "refusal"),
    [
        ("layers.0.depth", "declares no input `layers.0.depth`"),
        ("layers.x.opacity", "declares no input `layers.x.opacity`"),
        ("layers.01.opacity", "declares no input `layers.01.opacity`"),
        ("bands.0.opacity", "declares no input `bands.0.opacity`"),
        ("layers.0.picture", "carries a stream and holds no value"),
    ],
)
def test_a_value_on_a_port_no_slot_holds_or_on_a_slots_stream_is_refused(set_on: str, refusal: str) -> None:
    with pytest.raises((TypeError, ValueError), match=refusal):
        StreamBuilder("rig").add(_LayeredOver, input_values={set_on: 0.5})


def test_a_refusal_lists_a_groups_slot_ports_among_the_inputs() -> None:
    with pytest.raises(ValueError, match=r"`layers\.<slot>\.opacity`, `layers\.<slot>\.picture`"):
        StreamBuilder("rig").add(_LayeredOver).input("level")


@dataclass(frozen=True)
class MeteredPicture:
    """A picture with the level metered off it."""

    picture: Texture
    level: float
    label: str = "unnamed"


def test_a_bundle_holding_a_texture_is_set_as_a_mapping_of_its_other_members() -> None:
    """`MakeBundle` choosing a bundle that holds a Texture takes its other members as a mapping,
    which leaves the picture to its link; one setting the picture, or leaving out a member with
    no default, is refused.

    Fail-without-fix: a bundle is taken only as its dataclass, which cannot be built without a
    picture, so `{"level": 0.5}` is refused as not a Bundle.
    """
    chosen = PinTypeChoice(MeteredPicture)

    recorded = the_values_recorded_for(
        MakeBundle, input_values={"bundle_type": chosen, "members": {"level": 0.5}}
    )

    assert recorded["members"] == {"level": 0.5}
    for members in ({"level": 0.5, "picture": None}, {"label": "kitchen"}):
        with pytest.raises((TypeError, ValueError), match="members other than a Texture"):
            StreamBuilder("rig").add(
                MakeBundle, input_values={"bundle_type": chosen, "members": members}
            )

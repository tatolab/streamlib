# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Bare `@node` reads a `NamedTuple` return as one output per field, a type parameter as a pin
whose output takes the type linked into its input, and a union and `object` as wire only
inputs of several types and of any type."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated, Any, NamedTuple

import pytest

from tatolab.stream import (
    Color,
    Description,
    FormerlyNamed,
    NodeDeclarationRefusal,
    NumberUnit,
    PinMeaning,
    State,
    Stateful,
    UnreadMessage,
    node,
)
from tatolab.stream._function_node_declaration import NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)


def ports(declared: Any, stamp: str) -> dict[str, dict[str, Any]]:
    return {port["name"]: port for port in getattr(declared, stamp)}


def inputs(declared: Any) -> dict[str, dict[str, Any]]:
    return ports(declared, NODE_DECLARATION_INPUT_PORTS_STAMP)


def outputs(declared: Any) -> dict[str, dict[str, Any]]:
    return ports(declared, NODE_DECLARATION_OUTPUT_PORTS_STAMP)


def signature(declared: Any) -> dict[str, Any]:
    return getattr(declared, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)


def refusal_of(function: Any) -> NodeDeclarationRefusal:
    with pytest.raises(NodeDeclarationRefusal) as refused:
        node(function)
    return refused.value


class Bounds(NamedTuple):
    low: Annotated[float, PinMeaning("level"), Description("The lower edge.")]
    high: Color


@node
def Spread(level: float = 0.5, tint: Color = Color(1.0, 0.5, 0.2)) -> Bounds:
    return Bounds(low=level / 2, high=tint)


def test_a_named_tuple_of_two_fields_is_two_typed_outputs() -> None:
    low, high = outputs(Spread).values()
    assert (low["name"], high["name"]) == ("low", "high")
    assert low["description"] == "The lower edge."
    assert low["pin_declaration"] == {
        "pin_type": {"kind": "is", "pin_type": {"type": "float"}},
        "flow": "value",
        "annotations": {"meaning": "level"},
    }
    assert low["family"] == {"family": "channels", "names": {"stated": ["low"]}, "dtype": "f64"}
    assert high["pin_declaration"]["pin_type"] == {"kind": "is", "pin_type": {"type": "color"}}
    assert high["family"]["names"] == {"stated": ["red", "green", "blue", "alpha"]}
    fields_of = f"{__name__}:Bounds"
    assert signature(Spread)["outputs"] == [
        {
            "port": "low",
            "field": "low",
            "value_type": "float",
            "pin_type": {"type": "float"},
            "may_hand_out_nothing": False,
            "fields_of": fields_of,
        },
        {
            "port": "high",
            "field": "high",
            "value_type": "color",
            "pin_type": {"type": "color"},
            "may_hand_out_nothing": False,
            "fields_of": fields_of,
        },
    ]
    assert Spread(level=1.0) == Bounds(0.5, Color(1.0, 0.5, 0.2))


@dataclass(frozen=True)
class Peak:
    value: float = 0.0


class LevelAndPeak(NamedTuple):
    level: float
    peak: float | None


@node
def Follow(x: float = 0.0, *, previous: State[Peak]) -> Stateful[LevelAndPeak, Peak]:
    peak = max(x, previous.value)
    return LevelAndPeak(x, peak if peak > previous.value else None), Peak(peak)


def test_a_stateful_function_returns_a_named_tuple_of_outputs_each_of_which_may_hand_out_nothing() -> None:
    assert [(output["port"], output["may_hand_out_nothing"]) for output in signature(Follow)["outputs"]] == [
        ("level", False),
        ("peak", True),
    ]


@node
def Gate[PinValue](value: PinValue, open: bool = True) -> PinValue | None:  # noqa: A002
    return value if open else None


def test_a_type_parameter_types_an_input_of_any_type_and_an_output_of_the_type_linked_into_it() -> None:
    value = inputs(Gate)["value"]
    assert value["pin_declaration"] == {"pin_type": {"kind": "any"}, "flow": "value"}
    assert "pin_default" not in value, "a wire only input holds no value"
    assert value["family"] == {
        "family": "image",
        "also_takes_families": ["array", "channels", "packets", "records"],
    }
    (result,) = outputs(Gate).values()
    assert result["pin_declaration"] == {
        "pin_type": {"kind": "same_as_input", "input_port_name": "value"},
        "flow": "value",
    }
    assert result["family"] == {"family": "image", "family_follows_input_port": "value"}
    assert signature(Gate)["inputs"][0] == {
        "parameter": "value",
        "port": "value",
        "value_type": "unread_message",
        "optional": False,
    }
    assert signature(Gate)["outputs"] == [
        {"port": "result", "field": None, "value_type": "unread_message", "may_hand_out_nothing": True}
    ]


@node
def Hold[Picture: Color](tint: Annotated[Picture, FormerlyNamed("colour")]) -> Picture:
    return tint


@node
def Pick[Level: (float, int)](level: Level | None = None) -> Level | None:
    return level


def test_a_bound_narrows_the_input_a_type_parameter_types() -> None:
    tint = inputs(Hold)["tint"]
    assert tint["pin_declaration"] == {
        "pin_type": {"kind": "is", "pin_type": {"type": "color"}},
        "flow": "value",
        "former_names": ["colour"],
    }
    assert tint["family"] == {"family": "channels"}
    assert signature(Hold)["inputs"][0]["value_type"] == "unread_message", "its value is handed on unread"
    level = inputs(Pick)["level"]
    assert level["pin_declaration"] == {
        "pin_type": {"kind": "one_of", "pin_types": [{"type": "float"}, {"type": "integer"}]},
        "flow": "value",
        "may_be_unset": True,
    }
    assert level["pin_default"] is None


@node
def Magnitude(x: float | int, anything: object, tint: Color | float | None = None) -> float:
    return float(x)


def test_a_union_takes_each_of_its_types_and_object_takes_any_both_wire_only() -> None:
    x, anything, tint = inputs(Magnitude).values()
    assert x["pin_declaration"] == {
        "pin_type": {"kind": "one_of", "pin_types": [{"type": "float"}, {"type": "integer"}]},
        "flow": "value",
    }
    assert x["family"] == {"family": "channels"}
    assert tint["pin_declaration"]["pin_type"] == {
        "kind": "one_of",
        "pin_types": [{"type": "color"}, {"type": "float"}],
    }
    assert tint["pin_declaration"]["may_be_unset"] is True
    assert anything["pin_declaration"] == {"pin_type": {"kind": "any"}, "flow": "value"}
    assert [entry["value_type"] for entry in signature(Magnitude)["inputs"]] == [
        "float",
        "unread_message",
        "unread_message",
    ]


def MagnitudeDefaulting(x: float | int = 0.0) -> float:
    return float(x)


def GateDefaulting[PinValue](value: PinValue = 0.0) -> PinValue:  # pyright: ignore[reportInvalidTypeVarUse]
    return value


def Forget(value: float = 0.0) -> object:
    return value


def Either(value: float = 0.0) -> float | int:
    return value


def Invent[PinValue](level: float = 0.0) -> PinValue:  # pyright: ignore[reportInvalidTypeVarUse]
    raise NotImplementedError


def Choose[PinValue](first: PinValue, second: PinValue, pick_first: bool = True) -> PinValue:
    return first if pick_first else second


def Tuned(x: Annotated[float | int, NumberUnit("Hz")]) -> float:
    return float(x)


def Pass(value: UnreadMessage) -> UnreadMessage:
    return value


def Twice(value: float = 0.0) -> tuple[float, float]:
    return value, value


def Maybe(value: float = 0.0) -> Bounds | None:
    return None


def Described(value: float = 0.0) -> Annotated[Bounds, Description("Both edges.")]:
    return Bounds(value, Color(0.0, 0.0, 0.0))


def test_a_wire_only_input_with_a_default_is_refused_with_its_fix() -> None:
    for function in (MagnitudeDefaulting, GateDefaulting):
        refusal = refusal_of(function)
        assert refusal.finding_code == "wire_only_input_with_a_default", function
        assert "reads only what a link brings" in str(refusal)
        assert "| None = None`" in str(refusal)


def test_an_output_whose_type_no_one_input_finds_is_refused() -> None:
    for function in (Forget, Either, Invent):
        assert refusal_of(function).finding_code == "output_type_found_from_no_input", function
    refusal = refusal_of(Choose)
    assert refusal.finding_code == "type_parameter_types_several_inputs"
    assert "`first`, `second`" in str(refusal)


def test_an_annotation_on_a_pin_of_several_types_and_an_unread_message_are_refused() -> None:
    assert refusal_of(Tuned).finding_code == "pin_annotation_not_taken"
    refusal = refusal_of(Pass)
    assert "a type parameter" in str(refusal)
    assert "`def Pass[PinValue](value: PinValue, ...) -> PinValue`" in str(refusal)


def test_a_bare_tuple_asks_for_names_and_a_named_tuple_is_never_none_or_described_whole() -> None:
    refusal = refusal_of(Twice)
    assert refusal.finding_code == "outputs_not_named"
    assert "`NamedTuple`" in str(refusal)
    assert refusal_of(Maybe).finding_code == "return_not_read_yet"
    with pytest.raises(TypeError, match="describe each output where it is typed"):
        node(Described)

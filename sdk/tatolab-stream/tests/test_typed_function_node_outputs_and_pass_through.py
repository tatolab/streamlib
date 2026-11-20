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
    Audio,
    Color,
    Description,
    FormerlyNamed,
    NodeDeclarationRefusal,
    NumberUnit,
    PinMeaning,
    State,
    Stateful,
    Tensor,
    Texture,
    Trigger,
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
def Freeze[Frame: Texture](frame: Frame | None = None) -> Frame | None:
    return frame


def test_a_type_parameter_bound_to_texture_takes_texture_links_and_hands_on_the_type_linked_in() -> None:
    frame = inputs(Freeze)["frame"]
    assert frame["pin_declaration"] == {
        "pin_type": {"kind": "is", "pin_type": {"type": "texture"}},
        "flow": "value",
        "may_be_unset": True,
    }
    assert frame["family"] == {"family": "image"}
    (result,) = outputs(Freeze).values()
    assert result["pin_declaration"] == {
        "pin_type": {"kind": "same_as_input", "input_port_name": "frame"},
        "flow": "value",
    }
    assert result["family"] == {"family": "image", "family_follows_input_port": "frame"}
    assert signature(Freeze)["inputs"][0] == {
        "parameter": "frame",
        "port": "frame",
        "value_type": "unread_message",
        "optional": True,
        "default": None,
    }, "the body is handed the frame unread"
    assert signature(Freeze)["outputs"][0]["value_type"] == "unread_message"


@node
def PictureOrArray[Carried: (Texture, Tensor)](value: Carried) -> Carried:
    return value


@node
def HoldArray[Carried: Tensor](array: Carried) -> Carried:
    return array


def test_a_type_parameter_constrained_to_types_only_a_pin_carries_takes_each_as_a_value() -> None:
    value = inputs(PictureOrArray)["value"]
    assert value["pin_declaration"] == {
        "pin_type": {"kind": "one_of", "pin_types": [{"type": "texture"}, {"type": "tensor"}]},
        "flow": "value",
    }
    assert value["family"] == {
        "family": "image",
        "also_takes_families": ["array", "channels", "packets", "records"],
    }, "an input of two families reads every family"
    array = inputs(HoldArray)["array"]
    assert array["pin_declaration"]["pin_type"] == {"kind": "is", "pin_type": {"type": "tensor"}}
    assert array["family"] == {"family": "array"}


def HoldsAPictureDirectly(frame: Texture | None = None) -> float:
    return 0.0


def HoldsAPictureOrANumberDirectly(frame: Texture | float | None = None) -> float:
    return 0.0


def test_a_parameter_typed_texture_directly_is_refused_naming_the_type_parameter_form() -> None:
    for function in (HoldsAPictureDirectly, HoldsAPictureOrANumberDirectly):
        refusal = refusal_of(function)
        assert refusal.finding_code == "pin_type_not_read_yet", function
        assert refusal.pin == "frame"
        assert str(refusal) == (
            f"`{function.__name__}`'s input `frame` is typed `Texture`, a Texture, which holds no "
            f"stored value, so a function under bare `@node` takes it only through a type "
            f"parameter bound to it, whose value its body hands on unread. To fix: "
            f"`def {function.__name__}[Carried: Texture](frame: Carried, ...) -> Carried`, keep "
            f"this node on `@node.function`, or declare a class node with "
            f"`frame: InputPin[Texture]`."
        )


def MakesAPicture(level: float = 0.0) -> Texture:
    raise NotImplementedError


def test_an_output_typed_texture_directly_is_refused_naming_the_input_it_would_hand_on() -> None:
    refusal = refusal_of(MakesAPicture)
    assert refusal.finding_code == "pin_type_not_read_yet"
    assert refusal.pin == "result"
    assert str(refusal) == (
        "`MakesAPicture`'s output `result` is typed `Texture`, a Texture, which holds no stored "
        "value, so a function's output carries it only as the type linked into the input it "
        "hands on. To fix: type that input and this output by one type parameter bound to it, "
        "`[Carried: Texture]`, keep this node on `@node.function`, or declare a class node with "
        "`result: OutputPin[Texture]`."
    )


def HoldsAudio[Carried: Audio](samples: Carried) -> Carried:
    return samples


def HoldsAudioOrANumber[Carried: (float, Audio)](samples: Carried) -> Carried:
    return samples


def HoldsATrigger[Carried: Trigger](fired: Carried) -> Carried:
    return fired


@pytest.mark.parametrize(
    ("function", "pin", "carried", "words"),
    [
        (
            HoldsAudio,
            "samples",
            "Audio",
            "`HoldsAudio`'s input `samples` is typed by a type parameter bound to `Audio`, an "
            "Audio, which flows only as a stream",
        ),
        (
            HoldsAudioOrANumber,
            "samples",
            "Audio",
            "`HoldsAudioOrANumber`'s input `samples` is typed by a type parameter constrained to "
            "`Audio`, an Audio, which flows only as a stream",
        ),
        (
            HoldsATrigger,
            "fired",
            "Trigger",
            "`HoldsATrigger`'s input `fired` is typed by a type parameter bound to `Trigger`, a "
            "Trigger, which flows only as a trigger",
        ),
    ],
)
def test_a_bound_of_a_type_that_flows_as_no_value_is_refused(
    function: Any, pin: str, carried: str, words: str
) -> None:
    refusal = refusal_of(function)
    assert refusal.finding_code == "pin_flow_not_taken"
    assert refusal.pin == pin
    assert str(refusal) == (
        f"{words}, and a function reads the newest message on each input, so it would drop the "
        f"rest. To fix: declare a class node with `{pin}: InputPin[{carried}]`."
    )


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

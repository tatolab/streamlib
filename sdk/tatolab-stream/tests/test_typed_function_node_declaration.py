# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Bare `@node` on a function reads each pin's type and what it states, and refuses each mistake as a `NodeDeclarationRefusal`."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated, Any, Literal, NamedTuple

import pytest

from tatolab.stream import (
    Asset,
    Color,
    Description,
    Float32,
    Texture,
    Int8,
    Int64,
    NodeDeclarationRefusal,
    NumberRange,
    NumberStep,
    NumberUnit,
    PinControl,
    PinMeaning,
    Rectangle,
    State,
    Stateful,
    Time,
    Transform,
    Vector2,
    Vector3,
    node,
)
from tatolab.stream._function_node_declaration import NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_KIND_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

AudibleFrequency = Annotated[
    float, NumberRange(minimum=20.0, maximum=20_000.0), NumberUnit("Hz"), PinMeaning("frequency")
]


@node.meta(description="A tone's level at its frequency.")
@node
def Tone(
    frequency: Annotated[AudibleFrequency, Description("The pitch.")] = 440.0,
    gain: Annotated[float, NumberStep(0.01), PinControl("slider")] = 1,
    voices: Int8 = 1,
    muted: bool = False,
    detune: Float32 | None = None,
) -> float:
    return frequency * gain


@dataclass(frozen=True)
class Smoothed:
    value: float = 0.0


@node
def Smooth(
    x: float = 0.0, *, now: Time, previous: State[Smoothed]
) -> Stateful[Annotated[float, PinMeaning("level")], Smoothed]:
    halfway = previous.value + (x - previous.value) / 2
    return halfway, Smoothed(halfway)


@node
def Place(
    tint: Annotated[Color, PinControl("color")] = Color(1.0, 0.5, 0.2),
    offset: Vector2 = Vector2(0.0, 0.0),
    origin: Vector3 = Vector3(0.0, 0.0, 1.0),
    area: Rectangle = Rectangle(0.0, 0.0, 640.0, 360.0),
    move: Transform = Transform.identity(),
) -> Annotated[Color, PinMeaning("tint")]:
    return tint


def input_port(declared: Any, name: str) -> dict[str, Any]:
    return next(port for port in getattr(declared, NODE_DECLARATION_INPUT_PORTS_STAMP) if port["name"] == name)


def test_a_ranged_float_input_declares_its_range_unit_and_meaning() -> None:
    frequency = input_port(Tone, "frequency")

    assert frequency["description"] == "The pitch."
    assert frequency["pin_default"] == 440.0
    assert frequency["pin_declaration"] == {
        "pin_type": {"kind": "is", "pin_type": {"type": "float"}},
        "flow": "value",
        "annotations": {
            "range": {"minimum": 20.0, "maximum": 20_000.0},
            "unit": "Hz",
            "meaning": "frequency",
        },
    }
    assert "delivery_profile" not in frequency


def test_each_number_and_bool_input_declares_its_type_size_and_annotations() -> None:
    gain = input_port(Tone, "gain")
    assert gain["pin_default"] == 1.0
    assert isinstance(gain["pin_default"], float)
    assert gain["pin_declaration"]["annotations"] == {"step": 0.01, "control": "slider"}
    assert input_port(Tone, "voices")["pin_declaration"]["pin_type"] == {
        "kind": "is",
        "pin_type": {"type": "integer", "exact_size": "int8"},
    }
    assert input_port(Tone, "muted")["pin_declaration"]["pin_type"]["pin_type"] == {"type": "boolean"}
    detune = input_port(Tone, "detune")
    assert detune["pin_default"] is None
    assert detune["pin_declaration"]["may_be_unset"] is True
    assert detune["pin_declaration"]["pin_type"]["pin_type"] == {"type": "float", "exact_size": "float32"}


def test_a_typed_function_node_is_a_function_node_the_runtime_calls_as_today() -> None:
    assert getattr(Tone, NODE_DECLARATION_KIND_STAMP) == "function"
    signature = getattr(Tone, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    assert [entry["value_type"] for entry in signature["inputs"]] == ["float", "float", "int", "bool", "float"]
    assert signature["outputs"] == [
        {
            "port": "result",
            "field": None,
            "value_type": "float",
            "pin_type": {"type": "float"},
            "may_hand_out_nothing": False,
        }
    ]
    (result,) = getattr(Tone, NODE_DECLARATION_OUTPUT_PORTS_STAMP)
    assert result["pin_declaration"] == {
        "pin_type": {"kind": "is", "pin_type": {"type": "float"}},
        "flow": "value",
    }
    assert Tone(frequency=100.0, gain=2.0) == 200.0


def test_time_and_state_are_read_as_under_the_family_grammar_and_a_stateful_output_is_typed() -> None:
    signature = getattr(Smooth, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    assert signature["time_parameter"] == "now"
    assert signature["state_parameter"] == "previous"
    (result,) = getattr(Smooth, NODE_DECLARATION_OUTPUT_PORTS_STAMP)
    assert result["pin_declaration"]["annotations"] == {"meaning": "level"}


def test_a_value_class_input_declares_its_type_and_defaults_to_its_stored_form() -> None:
    tint = input_port(Place, "tint")
    assert tint["pin_declaration"] == {
        "pin_type": {"kind": "is", "pin_type": {"type": "color"}},
        "flow": "value",
        "annotations": {"control": "color"},
    }
    assert tint["pin_default"] == {"red": 1.0, "green": 0.5, "blue": 0.2, "alpha": 1.0}
    assert input_port(Place, "offset")["pin_declaration"]["pin_type"]["pin_type"] == {
        "type": "vector",
        "component_count": "two",
    }
    assert input_port(Place, "origin")["pin_default"] == {"x": 0.0, "y": 0.0, "z": 1.0}
    assert input_port(Place, "area")["pin_default"] == {
        "x": 0.0,
        "y": 0.0,
        "width": 640.0,
        "height": 360.0,
    }
    assert input_port(Place, "move")["pin_default"] == Transform.identity().stored_value()


def test_a_value_class_is_handed_to_the_body_by_its_value_type_and_its_output_names_its_parts() -> None:
    signature = getattr(Place, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    assert [entry["value_type"] for entry in signature["inputs"]] == [
        "color",
        "vector2",
        "vector3",
        "rectangle",
        "transform",
    ]
    assert signature["outputs"][0]["value_type"] == "color"
    (result,) = getattr(Place, NODE_DECLARATION_OUTPUT_PORTS_STAMP)
    assert result["family"]["names"] == {"stated": ["red", "green", "blue", "alpha"]}
    assert result["pin_declaration"]["annotations"] == {"meaning": "tint"}
    assert Place() == Color(1.0, 0.5, 0.2)


def refusal_of(function: Any) -> NodeDeclarationRefusal:
    with pytest.raises(NodeDeclarationRefusal) as refused:
        node(function)
    return refused.value


# The declarations below are refused; each is undecorated, so the module imports, and
# at module scope, where a function node is declared.


def Blur(radius: float) -> float:
    return radius


def BlurWithNoType(radius=1.0) -> float:  # pyright: ignore[reportMissingParameterType, reportUnknownParameterType]
    return radius  # pyright: ignore[reportUnknownVariableType]


def BlurPastItsRange(radius: Annotated[float, NumberRange(minimum=0.0, maximum=64.0)] = 200.0) -> float:
    return radius


def BlurPastItsSize(taps: Int8 = 300) -> float:
    return float(taps)


def BlurDefaultingToABool(radius: float = True) -> float:
    return radius


def BlurDefaultingToAStr(radius: float = "wide") -> float:  # pyright: ignore[reportArgumentType]
    return radius


def BlurDefaultingToNone(radius: float = None) -> float:  # pyright: ignore[reportArgumentType]
    return radius


def BlurUnsetWithADefault(radius: float | None = 2.0) -> float:
    return radius or 0.0


def GateWithAUnit(open: Annotated[bool, NumberUnit("Hz")] = False) -> bool:  # noqa: A002
    return open


def GateWithAString(level: Annotated[float, "loud"] = 0.0) -> float:
    return level


def GateWithTwoUnits(level: Annotated[float, NumberUnit("Hz"), NumberUnit("kHz")] = 0.0) -> float:
    return level


def WaveDefaultingToNoOption(shape: Literal["sine", "square"] = "saw") -> float:  # pyright: ignore[reportArgumentType]
    return 0.0


def WaveOfNumbers(shape: Literal[1, 2] = 1) -> float:
    return 0.0


def LevelsOfLevels(levels: list[list[float]] = []) -> float:  # noqa: B006
    return 0.0


def LevelsOfNoType(levels: list = []) -> float:  # noqa: B006  # pyright: ignore[reportMissingTypeArgument, reportUnknownParameterType]
    return 0.0


def LevelsWithAUnitEach(levels: list[Annotated[float, NumberUnit("Hz")]] = []) -> float:  # noqa: B006
    return 0.0


def LevelsOfPictures(pictures: list[Texture] = []) -> float:  # noqa: B006
    return 0.0


def Frames(at: list[Int64] = [0]) -> list[Int64]:  # noqa: B006
    return at


def Pass(picture: Texture | None = None) -> float:
    return 0.0


def Count(total: Int64 = 0) -> float:
    return float(total)


def Log(value: Any = 0.0) -> float:
    return 0.0


def Pair(value: complex = 0j) -> float:
    return 0.0


def Fields(value: dict[str, float] | None = None) -> float:
    return 0.0


def TintDefaultingToAVector(tint: Color = Vector2(0.0, 0.0)) -> Color:  # pyright: ignore[reportArgumentType]
    return tint


def TintWithARange(tint: Annotated[Color, NumberRange(minimum=0.0, maximum=1.0)] = Color(0, 0, 0)) -> Color:
    return tint


class Corners(NamedTuple):
    low: float
    high: float


def Split(x: float = 0.0) -> Corners:
    return Corners(x, x)


def Configured(x: float = 0.0, config: Smoothed = Smoothed()) -> float:
    return x


def test_a_value_input_with_no_default_is_refused_with_its_fix() -> None:
    refusal = refusal_of(Blur)
    assert refusal.finding_code == "value_input_has_no_default"
    assert (refusal.node, refusal.pin) == ("Blur", "radius")
    assert "To fix: give it a default of its type, or type it `float | None = None`" in str(refusal)


def test_a_parameter_with_no_annotation_is_refused() -> None:
    refusal = refusal_of(BlurWithNoType)
    assert refusal.finding_code == "pin_has_no_type"
    assert "`BlurWithNoType`'s parameter `radius` has no type" in str(refusal)


def test_a_default_outside_its_range_or_its_exact_size_is_refused_naming_both() -> None:
    refusal = refusal_of(BlurPastItsRange)
    assert refusal.finding_code == "pin_default_outside_its_range"
    assert "defaults to 200.0, outside its `NumberRange(minimum=0.0, maximum=64.0)`" in str(refusal)
    refusal = refusal_of(BlurPastItsSize)
    assert refusal.finding_code == "pin_default_outside_its_range"
    assert "outside the -128 to 127 its exact size `int8` holds" in str(refusal)


@pytest.mark.parametrize(
    "function",
    [
        pytest.param(BlurDefaultingToABool, id="a bool for a Float"),
        pytest.param(BlurDefaultingToAStr, id="a str for a Float"),
        pytest.param(BlurDefaultingToNone, id="None for an input that may not be unset"),
    ],
)
def test_a_default_not_of_its_type_is_refused(function: Any) -> None:
    assert refusal_of(function).finding_code == "pin_default_not_of_its_type"


def test_an_input_that_may_be_unset_takes_no_other_default() -> None:
    assert refusal_of(BlurUnsetWithADefault).finding_code == "unset_input_with_a_default"


def test_an_annotation_a_type_does_not_take_is_refused_naming_what_it_takes() -> None:
    refusal = refusal_of(GateWithAUnit)
    assert refusal.finding_code == "pin_annotation_not_taken"
    assert "which a Boolean pin does not take; it takes `PinMeaning`, `PinControl`" in str(refusal)


def test_an_object_no_pin_reads_and_an_annotation_written_twice_are_refused() -> None:
    assert refusal_of(GateWithAString).finding_code == "pin_annotation_not_taken"
    assert refusal_of(GateWithTwoUnits).finding_code == "pin_annotation_repeated"


def test_a_type_the_runtime_cannot_carry_yet_is_refused_by_name() -> None:
    for function, named in ((Pass, "typed `Texture`, a Texture, which holds no stored value"),):
        refusal = refusal_of(function)
        assert refusal.finding_code == "pin_type_not_read_yet", function
        assert named in str(refusal)


def test_a_value_class_default_of_another_class_or_a_range_on_it_is_refused() -> None:
    refusal = refusal_of(TintDefaultingToAVector)
    assert refusal.finding_code == "pin_default_not_of_its_type"
    assert "which is not a Color" in str(refusal)
    refusal = refusal_of(TintWithARange)
    assert refusal.finding_code == "pin_annotation_not_taken"
    assert "which a Color pin does not take" in str(refusal)


def test_a_wide_integer_any_and_an_unknown_type_are_refused() -> None:
    assert refusal_of(Count).finding_code == "exact_size_only_as_an_element"
    refusal = refusal_of(Log)
    assert refusal.finding_code == "pin_typed_any"
    assert str(refusal) == (
        "`Log`'s input `value` is typed `Any`, which checks nothing. To fix: `object`, or a "
        "type parameter when an output hands the value on."
    )
    assert refusal_of(Pair).finding_code == "pin_type_unknown"
    assert refusal_of(Fields).finding_code == "pin_type_unknown"


def test_a_named_tuple_names_its_outputs_and_a_config_is_refused() -> None:
    assert node(Split) is not None, "a named tuple names its outputs"
    assert refusal_of(Configured).finding_code == "parameter_not_a_pin"


def test_a_refusal_is_a_type_error_so_a_load_reports_it_as_one() -> None:
    with pytest.raises(TypeError):
        node(Blur)


@node
def Caption(
    text: str = "",
    shape: Literal["sine", "square"] = "square",
    picture: Asset = Asset("0eaf9ee2-0f73-4154-a3f6-a99c261b850d"),
    levels: list[float] = [0.25, 0.5],  # noqa: B006
    tints: list[Color] = [Color(1.0, 0.0, 0.0)],  # noqa: B006
) -> list[str]:
    return [text, shape, picture.asset, *map(str, levels), *map(str, tints)]


def test_a_string_menu_asset_and_list_are_declared_with_their_stored_defaults() -> None:
    assert input_port(Caption, "text")["pin_declaration"]["pin_type"]["pin_type"] == {
        "type": "string"
    }
    assert input_port(Caption, "shape")["pin_declaration"]["pin_type"]["pin_type"] == {
        "type": "menu",
        "option_names": ["sine", "square"],
    }
    assert input_port(Caption, "shape")["pin_default"] == "square"
    assert input_port(Caption, "picture")["pin_default"] == {
        "asset": "0eaf9ee2-0f73-4154-a3f6-a99c261b850d"
    }
    assert input_port(Caption, "levels")["pin_declaration"]["pin_type"]["pin_type"] == {
        "type": "list_of",
        "element_type": {"type": "float"},
    }
    assert input_port(Caption, "levels")["pin_default"] == [0.25, 0.5]
    assert input_port(Caption, "tints")["pin_default"] == [
        {"red": 1.0, "green": 0.0, "blue": 0.0, "alpha": 1.0}
    ]


def test_each_typed_pin_travels_on_the_family_of_its_message_and_states_its_pin_type() -> None:
    families = {
        port["name"]: port["family"]["family"]
        for port in getattr(Caption, NODE_DECLARATION_INPUT_PORTS_STAMP)
    }
    assert families == {
        "text": "records",
        "shape": "records",
        "picture": "records",
        "levels": "array",
        "tints": "array",
    }
    (result,) = getattr(Caption, NODE_DECLARATION_OUTPUT_PORTS_STAMP)
    assert result["family"] == {"family": "records"}
    signature = getattr(Caption, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    assert [entry["value_type"] for entry in signature["inputs"]] == [
        "string",
        "menu",
        "asset",
        "list_of",
        "list_of",
    ]
    assert signature["outputs"][0]["pin_type"] == {
        "type": "list_of",
        "element_type": {"type": "string"},
    }


def test_a_64_bit_integer_types_a_list_element() -> None:
    declared = node(Frames)
    assert input_port(declared, "at")["pin_declaration"]["pin_type"]["pin_type"] == {
        "type": "list_of",
        "element_type": {"type": "integer", "exact_size": "int64"},
    }


@pytest.mark.parametrize(
    ("function", "finding_code", "words"),
    [
        pytest.param(
            WaveDefaultingToNoOption,
            "pin_default_not_of_its_type",
            "which is none of its options, 'sine', 'square'",
            id="a menu default that is no option",
        ),
        pytest.param(WaveOfNumbers, "menu_options_not_valid", "options are names", id="menu of numbers"),
        pytest.param(LevelsOfLevels, "list_element_not_taken", "`list` is not", id="a List of a List"),
        pytest.param(LevelsOfNoType, "list_element_not_taken", "names the type", id="a bare list"),
        pytest.param(
            LevelsWithAUnitEach, "list_element_not_taken", "no annotation of its own", id="element unit"
        ),
        pytest.param(
            LevelsOfPictures, "list_element_not_taken", "`Texture` is not", id="a List of a stream type"
        ),
    ],
)
def test_a_menu_or_list_no_pin_may_declare_is_refused_naming_why(
    function: Any, finding_code: str, words: str
) -> None:
    refusal = refusal_of(function)
    assert refusal.finding_code == finding_code
    assert words in str(refusal)

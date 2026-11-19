# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Bare `@node` on a function reads each pin's type and what it states, and refuses each mistake as a `NodeDeclarationRefusal`."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated, Any, Literal, NamedTuple

import pytest

from tatolab.stream import (
    Description,
    Float32,
    ImageFrame,
    Int8,
    Int64,
    NodeDeclarationRefusal,
    NumberRange,
    NumberStep,
    NumberUnit,
    PinControl,
    PinMeaning,
    State,
    Stateful,
    Time,
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
        {"port": "result", "field": None, "value_type": "float", "may_hand_out_nothing": False}
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


def Caption(text: str = "") -> float:
    return float(len(text))


def Wave(shape: Literal["sine", "square"] = "sine") -> float:
    return 0.0


def Pass(picture: ImageFrame | None = None) -> float:
    return 0.0


def Count(total: Int64 = 0) -> float:
    return float(total)


def Log(value: Any = 0.0) -> float:
    return 0.0


def Pair(value: complex = 0j) -> float:
    return 0.0


def Fields(value: dict[str, float] | None = None) -> float:
    return 0.0


class Corners(NamedTuple):
    low: float
    high: float


def Split(x: float = 0.0) -> Corners:
    return Corners(x, x)


def Record(x: float = 0.0) -> Smoothed:
    return Smoothed(x)


def Configured(x: float = 0.0, config: Smoothed = Smoothed()) -> float:
    return x


class Mixer:
    pass


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
    for function, named in (
        (Caption, "`str`, the String pin type"),
        (Wave, "`Literal`, the Menu pin type"),
        (Pass, "`ImageFrame`, a read type"),
    ):
        refusal = refusal_of(function)
        assert refusal.finding_code == "pin_type_not_read_yet", function
        assert named in str(refusal)


def test_a_wide_integer_any_and_an_unknown_type_are_refused() -> None:
    assert refusal_of(Count).finding_code == "exact_size_only_as_an_element"
    assert refusal_of(Log).finding_code == "pin_typed_any"
    assert refusal_of(Pair).finding_code == "pin_type_unknown"
    assert refusal_of(Fields).finding_code == "pin_type_unknown"


def test_several_outputs_a_bundle_and_a_config_are_refused_until_they_are_read() -> None:
    assert refusal_of(Split).finding_code == "return_not_read_yet"
    assert refusal_of(Record).finding_code == "return_not_read_yet"
    assert refusal_of(Configured).finding_code == "parameter_not_a_pin"


def test_a_refusal_is_a_type_error_so_a_load_reports_it_as_one() -> None:
    with pytest.raises(TypeError):
        node(Blur)


def test_bare_node_on_a_class_is_refused_pointing_at_the_class_grammar() -> None:
    with pytest.raises(TypeError, match="`@node.process`"):
        node(Mixer)  # pyright: ignore[reportArgumentType]

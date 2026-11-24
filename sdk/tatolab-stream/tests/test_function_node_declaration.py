# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Bare `@node` reads a function's signature into a node, and refuses by name what is not one."""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Annotated, Any, Iterator, NamedTuple

import pytest

from tatolab.stream import (
    Description,
    Texture,
    State,
    Stateful,
    Time,
    node,
)
from tatolab.stream._function_node_declaration import (
    NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP,
    NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP,
    NODE_DECLARATION_LANGUAGE_STAMP,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_CATEGORY_STAMP,
    NODE_DECLARATION_DESCRIPTION_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_KIND_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)


@node.meta(description="Map 0..1 onto low..high.")
@node
def remap(x: float = 0.0, low: float = 0.0, high: float = 1.0) -> float:
    """The docstring, which the catalog never reads."""
    return low + x * (high - low)


@dataclass(frozen=True)
class SmoothedValue:
    value: float = 0.0


@node
def smooth(
    x: float = 0.0, *, now: Time, previous: State[SmoothedValue]
) -> Stateful[float, SmoothedValue]:
    halfway = previous.value + (x - previous.value) / 2
    return halfway, SmoothedValue(halfway)


@node.meta(category="Colour")
@node
def brighten(level: float = 0.0) -> float:
    return level * 2.0


def test_a_function_node_is_still_called_as_the_function_it_wraps():
    assert remap(0.5, low=2.0, high=4.0) == 3.0
    assert smooth(4.0, now=0, previous=SmoothedValue()) == (2.0, SmoothedValue(2.0))


def test_a_function_node_carries_kind_function_and_language_python():
    assert getattr(remap, NODE_DECLARATION_KIND_STAMP) == "function"
    assert getattr(remap, NODE_DECLARATION_LANGUAGE_STAMP) == "python"
    assert getattr(remap, NODE_DECLARATION_DESCRIPTION_STAMP) == "Map 0..1 onto low..high."
    assert remap.__module__ == __name__
    assert remap.__qualname__ == "remap"


def test_a_function_node_stamps_no_config():
    """A node takes each setting as an input, so nothing stamps a config class or its schema.

    Mental-revert: stamping them again sets both attributes on every function node.
    """
    assert not hasattr(remap, "__tatolab_node_config_class__")
    assert not hasattr(remap, "__tatolab_node_config_schema__")
    assert "config_parameter" not in getattr(remap, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)


def test_a_function_node_has_the_category_it_declares_and_none_when_it_declares_none():
    assert getattr(remap, NODE_DECLARATION_CATEGORY_STAMP) is None
    assert getattr(brighten, NODE_DECLARATION_CATEGORY_STAMP) == "Colour"


def test_a_default_is_the_pin_value():
    signature_inputs = getattr(remap, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)["inputs"]
    assert [each["default"] for each in signature_inputs] == [0.0, 0.0, 1.0]


def test_time_and_state_are_found_by_type_and_are_never_pins():
    signature = getattr(smooth, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    assert [each["name"] for each in getattr(smooth, NODE_DECLARATION_INPUT_PORTS_STAMP)] == ["x"]
    assert signature["time_parameter"] == "now"
    assert signature["state_parameter"] == "previous"
    assert signature["state_record_class"] == f"{__name__}:SmoothedValue"
    assert [each["port"] for each in signature["outputs"]] == ["result"]


def test_a_stateful_node_declares_its_state_fields_and_a_stateless_one_none():
    assert getattr(smooth, NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP) == ["value"]
    assert getattr(remap, NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP) == []


# What bare `@node` refuses on a function, each undecorated at module scope so only the
# refusal under test applies.


def untyped_parameter(x=0.0) -> float:  # pyright: ignore[reportMissingParameterType]
    return x


def variadic_positional(*values: float) -> float:
    return sum(values)


def variadic_keyword(**values: float) -> float:
    return sum(values.values())


def returns_none(x: float = 0.0) -> None:
    return None


def no_return_annotation(x: float = 0.0):  # pyright: ignore[reportMissingTypeArgument]
    return x


def state_without_stateful(x: float = 0.0, *, previous: State[SmoothedValue]) -> float:
    return x


def stateful_without_state(x: float = 0.0) -> Stateful[float, SmoothedValue]:
    return x, SmoothedValue(x)


async def asynchronous(x: float = 0.0) -> float:
    return x


def generator(x: float = 0.0) -> Iterator[float]:
    yield x


@dataclass(frozen=True)
class StateWithoutDefaults:
    value: float


def state_without_defaults(
    x: float = 0.0, *, previous: State[StateWithoutDefaults]
) -> Stateful[float, StateWithoutDefaults]:
    return x, StateWithoutDefaults(x)


@dataclass(frozen=True)
class GainConfig:
    gain: float = 2.0


def takes_a_config(x: float = 0.0, *, config: GainConfig) -> float:
    return x * config.gain


@pytest.mark.parametrize(
    ("not_a_function_node", "the_refusal_names"),
    [
        (lambda x: x, "a lambda"),
        (untyped_parameter, "`x` has no type"),
        (variadic_positional, "`*values`"),
        (variadic_keyword, "`**values`"),
        (returns_none, "returns `None`"),
        (no_return_annotation, "no return annotation"),
        (state_without_stateful, "returns no `Stateful[...]`"),
        (stateful_without_state, "takes no `State[...]`"),
        (asynchronous, "`async def`"),
        (generator, "a generator"),
        (state_without_defaults, "gives no default for value"),
        (takes_a_config, "takes `config`, and a node takes each setting as an input"),
    ],
)
def test_what_is_not_a_function_node_is_refused_by_name(
    not_a_function_node: Any, the_refusal_names: str
):
    with pytest.raises(TypeError, match=re.escape(the_refusal_names)):
        node(not_a_function_node)


def test_a_nested_function_or_a_closure_is_refused():
    scale = 2.0

    def nested(x: float = 0.0) -> float:
        return x * scale

    with pytest.raises(TypeError, match="defined inside another function"):
        node(nested)


# Port descriptions: `Annotated[T, Description("...")]` on a parameter or an output.

GAIN_PORT_DESCRIPTION = "The factor `x` is multiplied by."


@node.meta(description="Scales `x`.")
@node
def scaled(
    x: Annotated[float, Description("The value to scale.")] = 0.0,
    gain: Annotated[float, Description(GAIN_PORT_DESCRIPTION)] = 1.0,
    offset: Annotated[float | None, Description("Added after scaling.")] = None,
    undescribed: float = 0.0,
) -> Annotated[float, Description("`x` times `gain`.")]:
    """The docstring, never read as a port's description."""
    return x * gain + (offset or 0.0) + undescribed


def _described_ports(function_node: Any, stamp: str) -> "dict[str, str]":
    return {port["name"]: port["description"] for port in getattr(function_node, stamp)}


def test_a_described_input_carries_its_description_and_an_undescribed_one_the_empty_string():
    assert _described_ports(scaled, NODE_DECLARATION_INPUT_PORTS_STAMP) == {
        "x": "The value to scale.",
        "gain": "The factor `x` is multiplied by.",
        "offset": "Added after scaling.",
        "undescribed": "",
    }


def test_the_return_annotation_describes_the_result_output():
    assert _described_ports(scaled, NODE_DECLARATION_OUTPUT_PORTS_STAMP) == {
        "result": "`x` times `gain`."
    }


class DescribedSplitOutputs(NamedTuple):
    low: Annotated[float, Description("What falls under the threshold.")]
    high: Annotated[float | None, Description("What reaches it, or nothing.")]
    flagged: bool


@node
def described_split(x: float = 0.0) -> DescribedSplitOutputs:
    return DescribedSplitOutputs(low=min(x, 0.5), high=None, flagged=False)


def test_each_field_of_a_named_tuple_of_outputs_describes_its_own_output():
    assert _described_ports(described_split, NODE_DECLARATION_OUTPUT_PORTS_STAMP) == {
        "low": "What falls under the threshold.",
        "high": "What reaches it, or nothing.",
        "flagged": "",
    }


@node
def described_stateful(
    x: float = 0.0, *, previous: State[SmoothedValue]
) -> Stateful[Annotated[float, Description("The smoothed value.")], SmoothedValue]:
    smoothed = (previous.value + x) / 2.0
    return smoothed, SmoothedValue(smoothed)


def test_a_stateful_output_is_described_inside_stateful():
    assert _described_ports(described_stateful, NODE_DECLARATION_OUTPUT_PORTS_STAMP) == {
        "result": "The smoothed value."
    }


@node.shader("float4 tinted(ImageInput image, float amount) { return image.pixel * amount; }")
def tinted(
    image: Annotated[Texture, Description("The picture tinted.")], amount: float = 1.0
) -> Annotated[Texture, Description("The tinted picture.")]: ...


def test_a_shader_nodes_ports_are_described_the_same_way():
    assert _described_ports(tinted, NODE_DECLARATION_INPUT_PORTS_STAMP) == {
        "image": "The picture tinted.",
        "amount": "",
    }
    assert _described_ports(tinted, NODE_DECLARATION_OUTPUT_PORTS_STAMP) == {
        "result": "The tinted picture."
    }


def time_described(*, now: Annotated[Time, Description("The read's time.")]) -> float:
    return float(now)


def outputs_described_as_a_whole(
    x: float = 0.0,
) -> Annotated[DescribedSplitOutputs, Description("Both halves.")]:
    return DescribedSplitOutputs(low=x, high=None, flagged=False)


def described_twice(
    x: Annotated[float, Description("One."), Description("Two.")] = 0.0,
) -> float:
    return x


@pytest.mark.parametrize(
    ("function", "refusal"),
    [
        (time_described, "only a port takes one"),
        (outputs_described_as_a_whole, "describe each output where it is typed"),
        (described_twice, "it takes one"),
    ],
)
def test_a_description_where_no_one_port_is_typed_is_refused(function: Any, refusal: str):
    with pytest.raises(TypeError, match=refusal):
        node(function)


def test_a_docstring_never_stands_in_for_a_function_nodes_description():
    assert getattr(described_split, NODE_DECLARATION_DESCRIPTION_STAMP) == ""

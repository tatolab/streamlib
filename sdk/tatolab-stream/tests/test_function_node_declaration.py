# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`@node.function` reads a function's signature into a node, and refuses by name what is not one."""

from __future__ import annotations

import dataclasses
from dataclasses import dataclass
from typing import Annotated, Any, Iterator

import pytest

from tatolab.stream import Image, ImageFrame, State, Stateful, Time, UnreadMessage, node
from tatolab.stream._function_node_declaration import (
    NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP,
    NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP,
    NODE_DECLARATION_LANGUAGE_STAMP,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_CATEGORY_STAMP,
    NODE_DECLARATION_CONFIG_SCHEMA_STAMP,
    NODE_DECLARATION_DESCRIPTION_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_KIND_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)


@node.function
def remap(x: float, low: float = 0.0, high: float = 1.0) -> float:
    """Map 0..1 onto low..high."""
    return low + x * (high - low)


@dataclass(frozen=True)
class SmoothedValue:
    value: float = 0.0


@node.function
def smooth(
    x: float, *, now: Time, previous: State[SmoothedValue]
) -> Stateful[float, SmoothedValue]:
    halfway = previous.value + (x - previous.value) / 2
    return halfway, SmoothedValue(halfway)


@dataclass(frozen=True)
class SplitColour:
    red: float
    green: float


@node.function(description="Split a grey level into two channels.", category="Colour")
def split(grey: float) -> SplitColour:
    return SplitColour(red=grey, green=grey)


@dataclass(frozen=True)
class GainConfig:
    gain: float = 2.0


@node.function
def amplify(x: float, config: GainConfig) -> float:
    return x * config.gain


@node.function
def tint(frame: ImageFrame, strength: float | None = None) -> ImageFrame:
    return frame


def test_a_function_node_is_still_called_as_the_function_it_wraps():
    assert remap(0.5, low=2.0, high=4.0) == 3.0
    assert smooth(4.0, now=0, previous=SmoothedValue()) == (2.0, SmoothedValue(2.0))


def test_a_function_node_carries_kind_function_and_language_python():
    assert getattr(remap, NODE_DECLARATION_KIND_STAMP) == "function"
    assert getattr(remap, NODE_DECLARATION_LANGUAGE_STAMP) == "python"
    assert getattr(remap, NODE_DECLARATION_DESCRIPTION_STAMP) == "Map 0..1 onto low..high."
    assert remap.__module__ == __name__
    assert remap.__qualname__ == "remap"


def test_a_function_node_is_filed_under_its_module_unless_it_names_a_category():
    assert getattr(remap, NODE_DECLARATION_CATEGORY_STAMP) == __name__
    assert getattr(split, NODE_DECLARATION_CATEGORY_STAMP) == "Colour"


def test_each_plain_parameter_is_a_newest_channels_input_and_the_return_is_result():
    assert getattr(remap, NODE_DECLARATION_INPUT_PORTS_STAMP) == [
        {
            "name": name,
            "description": "",
            "delivery_profile": "newest",
            "family": {"family": "channels"},
            **pin_default,
        }
        for name, pin_default in (("x", {}), ("low", {"pin_default": 0.0}), ("high", {"pin_default": 1.0}))
    ]
    assert getattr(remap, NODE_DECLARATION_OUTPUT_PORTS_STAMP) == [
        {
            "name": "result",
            "description": "",
            "family": {"family": "channels", "names": {"stated": ["result"]}, "dtype": "f64"},
        }
    ]


def test_a_default_is_the_pin_value_and_a_parameter_without_one_must_be_wired():
    signature_inputs = getattr(remap, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)["inputs"]
    assert [each.get("default", "wired") for each in signature_inputs] == ["wired", 0.0, 1.0]


def test_time_and_state_are_found_by_type_and_are_never_pins():
    signature = getattr(smooth, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    assert [each["name"] for each in getattr(smooth, NODE_DECLARATION_INPUT_PORTS_STAMP)] == [
        "x"
    ]
    assert signature["time_parameter"] == "now"
    assert signature["state_parameter"] == "previous"
    assert signature["state_record_class"] == f"{__name__}:SmoothedValue"
    assert [each["port"] for each in signature["outputs"]] == ["result"]


def test_a_stateful_node_declares_its_state_fields_and_a_stateless_one_none():
    assert getattr(smooth, NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP) == ["value"]
    assert getattr(remap, NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP) == []


def test_a_returned_dataclass_is_one_output_per_field():
    assert [each["name"] for each in getattr(split, NODE_DECLARATION_OUTPUT_PORTS_STAMP)] == [
        "red",
        "green",
    ]
    assert getattr(split, NODE_DECLARATION_DESCRIPTION_STAMP) == (
        "Split a grey level into two channels."
    )


def test_config_is_one_dataclass_whose_schema_is_published_and_never_a_pin():
    assert [each["name"] for each in getattr(amplify, NODE_DECLARATION_INPUT_PORTS_STAMP)] == [
        "x"
    ]
    assert getattr(amplify, NODE_DECLARATION_CONFIG_SCHEMA_STAMP)["properties"]["gain"] == {
        "type": "number",
        "default": 2.0,
    }
    assert getattr(amplify, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)["config_parameter"] == (
        "config"
    )


def test_a_read_type_is_its_family_and_an_optional_input_defaults_to_none():
    input_ports = getattr(tint, NODE_DECLARATION_INPUT_PORTS_STAMP)
    assert [each["family"]["family"] for each in input_ports] == ["image", "channels"]
    signature_inputs = getattr(tint, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)["inputs"]
    assert signature_inputs[1]["optional"] is True
    assert signature_inputs[1]["default"] is None
    assert getattr(tint, NODE_DECLARATION_OUTPUT_PORTS_STAMP)[0]["family"] == {"family": "image"}


# What `@node.function` refuses, each undecorated at module scope so only the
# refusal under test applies.


class ClassWithProcess:
    def process(self, ctx: Any) -> None: ...


def untyped_parameter(x) -> float:  # pyright: ignore[reportMissingParameterType]
    return x


def variadic_positional(*values: float) -> float:
    return sum(values)


def variadic_keyword(**values: float) -> float:
    return sum(values.values())


def returns_none(x: float) -> None:
    return None


def no_return_annotation(x: float):  # pyright: ignore[reportMissingTypeArgument]
    return x


def state_without_stateful(x: float, previous: State[SmoothedValue]) -> float:
    return x


def stateful_without_state(x: float) -> Stateful[float, SmoothedValue]:
    return x, SmoothedValue(x)


async def asynchronous(x: float) -> float:
    return x


def generator(x: float) -> Iterator[float]:
    yield x


def unsupported_input_type(x: str) -> float:
    return 0.0


@dataclass(frozen=True)
class StateWithoutDefaults:
    value: float


def state_without_defaults(
    x: float, previous: State[StateWithoutDefaults]
) -> Stateful[float, StateWithoutDefaults]:
    return x, StateWithoutDefaults(x)


def config_that_is_not_a_dataclass(x: float, config: dict) -> float:
    return x


def string_default(x: float, label: float = "loud") -> float:  # pyright: ignore[reportArgumentType]
    return x


def none_default_on_a_pin_that_is_not_optional(x: float = None) -> float:  # pyright: ignore[reportArgumentType]
    return x


@pytest.mark.parametrize(
    ("not_a_function_node", "the_refusal_names"),
    [
        (ClassWithProcess, "`@node.process`"),
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
        (unsupported_input_type, "names no port family"),
        (state_without_defaults, "gives no default for value"),
        (config_that_is_not_a_dataclass, "one dataclass"),
        (string_default, "only a number or a bool pin takes a default"),
        (none_default_on_a_pin_that_is_not_optional, "defaults to `None` but is not typed optional"),
    ],
)
def test_what_is_not_a_function_node_is_refused_by_name(
    not_a_function_node: Any, the_refusal_names: str
):
    with pytest.raises(TypeError, match=_literally(the_refusal_names)):
        node.function(not_a_function_node)


def test_a_nested_function_or_a_closure_is_refused():
    scale = 2.0

    def nested(x: float) -> float:
        return x * scale

    with pytest.raises(TypeError, match="defined inside another function"):
        node.function(nested)


def test_the_bare_node_refusal_names_node_function_too():
    with pytest.raises(TypeError, match=_literally("`@node.function`")):
        node(remap)


def _literally(text: str) -> str:
    import re

    return re.escape(text)



@node.function
def hand_on(value: UnreadMessage, open: bool = True) -> UnreadMessage | None:  # noqa: A002
    return value if open else None


@node.function
def positive_or_nothing(x: float) -> float | None:
    return x if x > 0.0 else None


def pick_one_of_two(first: UnreadMessage, second: UnreadMessage) -> UnreadMessage:
    return first


@node.function
def hand_on_an_image(frame: Annotated[UnreadMessage, Image] | None = None) -> UnreadMessage | None:
    return frame


@dataclass(frozen=True)
class HeldMessage:
    message: UnreadMessage | None = None


@node.function
def hold_the_first_message(
    value: UnreadMessage, *, previous: State[HeldMessage]
) -> Stateful[UnreadMessage | None, HeldMessage]:
    held = previous.message or value
    return held, HeldMessage(held)


def scale_by_an_image(x: Annotated[float, Image]) -> float:
    return x


def scale_by_a_described_level(x: Annotated[float, "How far to scale."]) -> float:
    return x


def test_an_unread_message_input_takes_every_family_and_its_output_follows_it():
    (value_port, _) = getattr(hand_on, NODE_DECLARATION_INPUT_PORTS_STAMP)
    assert value_port["family"]["family"] == "image"
    assert set(value_port["family"]["also_takes_families"]) == {
        "array",
        "channels",
        "packets",
        "records",
    }
    (result_port,) = getattr(hand_on, NODE_DECLARATION_OUTPUT_PORTS_STAMP)
    assert result_port["family"] == {"family": "image", "family_follows_input_port": "value"}
    signature = getattr(hand_on, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    assert signature["inputs"][0]["value_type"] == "unread_message"
    assert signature["outputs"] == [
        {"port": "result", "field": None, "value_type": "unread_message", "may_hand_out_nothing": True}
    ]


def test_an_output_that_may_be_none_is_one_that_may_hand_out_nothing():
    signature = getattr(positive_or_nothing, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    assert signature["outputs"][0]["value_type"] == "float"
    assert signature["outputs"][0]["may_hand_out_nothing"] is True
    remap_signature = getattr(remap, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    assert remap_signature["outputs"][0]["may_hand_out_nothing"] is False


def hand_on_a_message_it_was_never_handed(value: float) -> UnreadMessage:
    raise NotImplementedError


def test_an_unread_message_output_without_exactly_one_unread_message_input_is_refused():
    with pytest.raises(TypeError, match="take exactly one `UnreadMessage` input"):
        node.function(pick_one_of_two)
    with pytest.raises(TypeError, match="take exactly one `UnreadMessage` input"):
        node.function(hand_on_a_message_it_was_never_handed)


@dataclass(frozen=True)
class TwoLevels:
    low: float = 0.0
    high: float = 1.0


def two_levels_or_nothing(x: float) -> TwoLevels | None:
    return None


def test_a_record_of_outputs_that_may_be_none_is_refused_naming_the_fix():
    with pytest.raises(TypeError, match=_literally("type each field that may hand out nothing")):
        node.function(two_levels_or_nothing)


def test_an_unread_message_annotated_with_a_family_takes_that_family_alone():
    (frame_port,) = getattr(hand_on_an_image, NODE_DECLARATION_INPUT_PORTS_STAMP)
    assert frame_port["family"] == {"family": "image"}
    (result_port,) = getattr(hand_on_an_image, NODE_DECLARATION_OUTPUT_PORTS_STAMP)
    assert result_port["family"] == {"family": "image", "family_follows_input_port": "frame"}
    signature = getattr(hand_on_an_image, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    assert signature["inputs"][0]["value_type"] == "unread_message"
    assert signature["inputs"][0]["optional"] is True


def test_a_state_field_may_hold_an_unread_message():
    signature = getattr(hold_the_first_message, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    assert signature["state_parameter"] == "previous"
    assert signature["outputs"][0]["value_type"] == "unread_message"


def test_a_family_named_on_anything_but_an_unread_message_is_refused_and_other_metadata_is_not():
    with pytest.raises(TypeError, match=_literally("only an `UnreadMessage` takes one")):
        node.function(scale_by_an_image)
    signature = getattr(
        node.function(scale_by_a_described_level), NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP
    )
    assert signature["inputs"][0]["value_type"] == "float"

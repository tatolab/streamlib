# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`@node(...)` on a function declares a node written as one function, its ports read by the one
reader a class's fields and pins are read by, and refuses each mistake as a
`NodeDeclarationRefusal` with its own finding code."""

from __future__ import annotations

import dataclasses
from typing import Annotated, Any, Callable, Literal

import pytest

from tatolab.stream import (
    Description,
    Input,
    InputPin,
    NodeDeclarationRefusal,
    NumberRange,
    OutputPin,
    PinGroup,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    SettingOnly,
    Texture,
    Trigger,
    WireOnly,
    node,
)
from tatolab.stream._function_node_declaration import FunctionNode
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_DESCRIPTION_STAMP,
    NODE_DECLARATION_EXECUTION_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_KIND_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
    NODE_DECLARATION_PIN_GROUPS_STAMP,
)
from tatolab.stream._node_function import InputReadOffItsDeclaration, NodeFunction


@node(execution="continuous", interval_ms=10)
class TintAsAClass:
    video: Annotated[InputPin[Texture], Description("The pictures to tint.")]
    flash: InputPin[Trigger]
    amount: Annotated[float, NumberRange(minimum=0.0, maximum=1.0)] = 0.5
    mode: Literal["add", "multiply"] = "add"
    wet: Annotated[float, WireOnly()] = 1.0
    result: OutputPin[Texture]


@node.meta(description="Tints each picture.")
@node(execution="continuous", interval_ms=10)
def tint(
    *,
    video: Annotated[InputPin[Texture], Description("The pictures to tint.")],
    flash: InputPin[Trigger],
    amount: Input[Annotated[float, NumberRange(minimum=0.0, maximum=1.0)]] = Input(0.5),
    mode: Input[Literal["add", "multiply"]] = Input("add"),
    wet: Input[Annotated[float, WireOnly()]] = Input(1.0),
    result: OutputPin[Texture],
    ctx: RuntimeContextFullAccess,
) -> Callable[[RuntimeContextLimitedAccess], None]:
    return lambda frame_ctx: None


def test_a_node_function_declares_the_ports_a_class_of_the_same_fields_and_pins_declares() -> None:
    """Mental-revert: read a function's parameters with a reader of their own and a pin's
    description, a value's range or a wire-only mode drifts from the class's."""
    assert getattr(tint, NODE_DECLARATION_INPUT_PORTS_STAMP) == getattr(
        TintAsAClass, NODE_DECLARATION_INPUT_PORTS_STAMP
    )
    assert getattr(tint, NODE_DECLARATION_OUTPUT_PORTS_STAMP) == getattr(
        TintAsAClass, NODE_DECLARATION_OUTPUT_PORTS_STAMP
    )
    assert getattr(tint, NODE_DECLARATION_KIND_STAMP) == "process"
    assert getattr(tint, NODE_DECLARATION_EXECUTION_STAMP) == {"mode": "continuous", "interval_ms": 10}
    assert getattr(tint, NODE_DECLARATION_PIN_GROUPS_STAMP) == []
    assert getattr(tint, NODE_DECLARATION_DESCRIPTION_STAMP) == "Tints each picture."


def test_a_node_function_holds_each_parameter_and_finds_the_context_by_its_type() -> None:
    assert isinstance(tint, NodeFunction)
    assert (tint.__module__, tint.__qualname__) == (__name__, "tint")
    assert tint.full_access_context_parameter == "ctx"
    assert [(member.attribute, member.form, member.default) for member in tint.members] == [
        ("video", "input_pin", tint.members[0].default),
        ("flash", "input_pin", tint.members[1].default),
        ("amount", "field", 0.5),
        ("mode", "field", "add"),
        ("wet", "field", 1.0),
        ("result", "output_pin", tint.members[5].default),
    ]


@node
def doubled(level: float = 1.0) -> float:
    return level * 2


def test_bare_node_on_a_function_still_declares_a_pure_function_node() -> None:
    assert isinstance(doubled, FunctionNode)
    assert getattr(doubled, NODE_DECLARATION_KIND_STAMP) == "function"


def test_a_node_function_with_no_keyword_reads_as_reactive() -> None:
    @node()
    def gain(*, level: Input[float] = Input(1.0)) -> None:
        return None

    assert getattr(gain, NODE_DECLARATION_EXECUTION_STAMP) == {"mode": "reactive", "interval_ms": 0}


def test_get_on_the_input_that_declares_a_default_is_refused_by_its_own_kind() -> None:
    with pytest.raises(InputReadOffItsDeclaration, match="declares an input's default"):
        Input(0.5).get()


@dataclasses.dataclass(frozen=True)
class Layer:
    picture: InputPin[Texture]


def refusal_of(declare: Callable[[], Any]) -> NodeDeclarationRefusal:
    with pytest.raises(NodeDeclarationRefusal) as refused:
        declare()
    return refused.value


def declared_with_keywords(function: Callable[..., Any]) -> Callable[[], Any]:
    return lambda: node(execution="reactive")(function)


def test_a_positional_parameter_is_refused() -> None:
    def gain(level: Input[float] = Input(1.0)) -> None: ...

    refused = refusal_of(declared_with_keywords(gain))
    assert (refused.finding_code, refused.pin) == ("node_function_parameter_not_keyword_only", "level")


def test_an_untyped_parameter_is_refused() -> None:
    def gain(*, level=Input(1.0)) -> None: ...  # pyright: ignore[reportMissingParameterType]

    refused = refusal_of(declared_with_keywords(gain))
    assert (refused.finding_code, refused.pin) == ("node_function_parameter_not_typed", "level")


def test_a_value_typed_as_a_class_field_is_refused_naming_input() -> None:
    def gain(*, level: float = 1.0) -> None: ...

    refused = refusal_of(declared_with_keywords(gain))
    assert (refused.finding_code, refused.pin) == ("node_function_parameter_not_an_input", "level")
    assert "Input[...] = Input(...)" in str(refused)


def test_the_limited_context_in_the_body_is_refused_naming_the_per_frame_function() -> None:
    def gain(*, ctx: RuntimeContextLimitedAccess) -> None: ...

    refused = refusal_of(declared_with_keywords(gain))
    assert (refused.finding_code, refused.pin) == ("node_function_parameter_not_an_input", "ctx")
    assert "each frame" in str(refused)


def test_an_input_defaulting_to_a_plain_value_is_refused() -> None:
    def gain(*, level: Input[float] = 1.0) -> None: ...  # pyright: ignore[reportArgumentType]

    refused = refusal_of(declared_with_keywords(gain))
    assert (refused.finding_code, refused.pin) == ("input_default_not_an_input", "level")
    assert "Input(1.0)" in str(refused)


def test_an_input_read_at_setup_is_refused() -> None:
    def gain(*, level: Input[Annotated[float, SettingOnly()]] = Input(1.0)) -> None: ...

    refused = refusal_of(declared_with_keywords(gain))
    assert (refused.finding_code, refused.pin) == ("setting_on_a_node_function", "level")


def test_a_pin_group_is_refused() -> None:
    def stack(*, layers: PinGroup[Layer]) -> None: ...

    refused = refusal_of(declared_with_keywords(stack))
    assert (refused.finding_code, refused.pin) == ("pin_group_on_a_node_function", "layers")


def test_a_pin_given_a_value_is_refused_as_a_class_pin_is() -> None:
    def gain(*, video: InputPin[Texture] = 1) -> None: ...  # pyright: ignore[reportArgumentType]

    refused = refusal_of(declared_with_keywords(gain))
    assert (refused.finding_code, refused.pin) == ("pin_has_a_value", "video")


def test_a_stream_adds_a_node_function_by_its_import_path_with_its_values() -> None:
    from tatolab.stream import StreamBuilder

    stream_builder = StreamBuilder("rig")
    assert stream_builder.add(tint, input_values={"amount": 0.25}).name == "tint"

    (added,) = stream_builder._compiled_graph()["nodes"]  # pyright: ignore[reportPrivateUsage]
    assert (added["type"], added["input_values"]) == (f"{__name__}:tint", {"amount": 0.25})

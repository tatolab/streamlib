# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`@node(...)` on a function declares a node written as one function, and refuses each mistake
as a `NodeDeclarationRefusal` with its own finding code; a class under `@node` is refused as the
retired form, with the function that replaces it."""

from __future__ import annotations

import dataclasses
from typing import Annotated, Any, Callable, Iterator, Literal

import pytest

from tatolab.stream import (
    Description,
    Input,
    InputPin,
    NodeDeclarationRefusal,
    NumberRange,
    OutputPin,
    PinGroup,
    PinTypeChoice,
    PinTypeFromSetting,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    SettingOnly,
    Tensor,
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


def test_a_node_function_declares_each_parameter_as_its_port() -> None:
    assert [port["name"] for port in getattr(tint, NODE_DECLARATION_INPUT_PORTS_STAMP)] == [
        "video",
        "flash",
        "amount",
        "mode",
        "wet",
    ]
    (amount,) = [
        port for port in getattr(tint, NODE_DECLARATION_INPUT_PORTS_STAMP) if port["name"] == "amount"
    ]
    assert amount["pin_declaration"]["annotations"]["range"] == {"minimum": 0.0, "maximum": 1.0}
    assert amount["pin_default"] == 0.5
    assert [port["name"] for port in getattr(tint, NODE_DECLARATION_OUTPUT_PORTS_STAMP)] == ["result"]
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


def test_a_value_typed_with_no_input_is_refused_naming_input() -> None:
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


def setting_only_inside_its_input(*, level: Input[Annotated[float, SettingOnly()]] = Input(1.0)) -> None: ...


def setting_only_around_its_input(*, level: Annotated[Input[float], SettingOnly()] = Input(1.0)) -> None: ...


@pytest.mark.parametrize("gain", [setting_only_inside_its_input, setting_only_around_its_input])
def test_an_input_read_at_setup_is_refused_inside_or_around_its_input(gain: Callable[..., Any]) -> None:
    """Mental-revert: look for `SettingOnly` only inside `Input[...]`, and the one written around it
    declares a setting whose set rebuilds the node."""
    refused = refusal_of(declared_with_keywords(gain))
    assert (refused.finding_code, refused.pin) == ("setting_on_a_node_function", "level")


def convert(
    *,
    content_type: Input[PinTypeChoice] = Input(PinTypeChoice(Texture)),
    result: OutputPin[Annotated[Texture | Tensor, PinTypeFromSetting("content_type")]],
) -> None: ...


def test_an_input_choosing_a_pin_type_is_live_and_its_output_takes_the_type_it_holds() -> None:
    """Mental-revert: read a `PinTypeChoice` input as a setting, and the node function declares a
    setting read at setup, whose set rebuilds the node; refuse it, and no node function chooses
    a pin's type."""
    declared = declared_with_keywords(convert)()
    (content_type,) = getattr(declared, NODE_DECLARATION_INPUT_PORTS_STAMP)
    (result,) = getattr(declared, NODE_DECLARATION_OUTPUT_PORTS_STAMP)
    assert content_type["name"] == "content_type"
    assert content_type["input_mode"] == {"mode": "wire_or_value"}
    assert content_type["pin_declaration"]["pin_type"]["pin_type"]["type"] == "pin_type_choice"
    assert result["pin_declaration"]["pin_type"] == {
        "kind": "set_by_input",
        "input_name": "content_type",
    }


def test_a_parameter_named_private_is_refused() -> None:
    """Mental-revert: skip a `_` parameter as private state, and the body is handed the bare
    `Input(2.0)` its declaration wrote."""

    def gain(*, level: Input[float] = Input(1.0), _hidden: Input[float] = Input(2.0)) -> None: ...

    refused = refusal_of(declared_with_keywords(gain))
    assert (refused.finding_code, refused.pin) == ("node_function_parameter_private", "_hidden")


def test_the_context_taken_twice_is_refused() -> None:
    def gain(*, ctx: RuntimeContextFullAccess, again: RuntimeContextFullAccess) -> None: ...

    refused = refusal_of(declared_with_keywords(gain))
    assert (refused.finding_code, refused.pin) == ("node_function_takes_the_context_twice", "again")


def test_a_generator_body_is_refused() -> None:
    def gain(*, level: Input[float] = Input(1.0)) -> Iterator[None]:
        yield None

    refused = refusal_of(declared_with_keywords(gain))
    assert (refused.finding_code, refused.node) == ("node_function_not_a_plain_def", "gain")


@dataclasses.dataclass(frozen=True)
class PlacedLayer:
    picture: InputPin[Texture]
    opacity: Annotated[float, NumberRange(minimum=0.0, maximum=1.0)] = 1.0


def test_a_pin_group_is_declared_with_each_field_of_its_slot() -> None:
    """Mental-revert: refuse a `PinGroup` parameter, or stamp no group, and the node function
    declares no layers to wire."""

    @node(execution="reactive")
    def stack(*, layers: Annotated[PinGroup[PlacedLayer], Description("Each layer.")]) -> None: ...

    assert isinstance(stack, NodeFunction)
    (declared,) = getattr(stack, NODE_DECLARATION_PIN_GROUPS_STAMP)
    assert declared["name"] == "layers"
    (layers,) = stack.pin_groups
    assert (layers.attribute, layers.name, layers.slot_class) == ("layers", "layers", PlacedLayer)
    assert [member.port for member in layers.members] == ["picture", "opacity"]


@dataclasses.dataclass(frozen=True)
class LayerHoldingASetting:
    picture: InputPin[Texture]
    blend: Annotated[float, SettingOnly()] = 1.0


def test_a_slot_holding_a_setting_is_refused() -> None:
    def stack(*, layers: PinGroup[LayerHoldingASetting]) -> None: ...

    refused = refusal_of(declared_with_keywords(stack))
    assert (refused.finding_code, refused.pin) == ("setting_on_a_node_function", "layers.blend")


def test_a_pin_group_given_a_value_is_refused() -> None:
    def stack(*, layers: PinGroup[Layer] = ()) -> None: ...  # pyright: ignore[reportArgumentType]

    refused = refusal_of(declared_with_keywords(stack))
    assert (refused.finding_code, refused.pin) == ("pin_has_a_value", "layers")


def test_a_pin_given_a_value_is_refused() -> None:
    def gain(*, video: InputPin[Texture] = 1) -> None: ...  # pyright: ignore[reportArgumentType]

    refused = refusal_of(declared_with_keywords(gain))
    assert (refused.finding_code, refused.pin) == ("pin_has_a_value", "video")


def test_a_stream_adds_a_node_function_by_its_import_path_with_its_values() -> None:
    from tatolab.stream import StreamBuilder

    stream_builder = StreamBuilder("rig")
    assert stream_builder.add(tint, input_values={"amount": 0.25}).name == "tint"

    (added,) = stream_builder._compiled_graph()["nodes"]  # pyright: ignore[reportPrivateUsage]
    assert (added["type"], added["input_values"]) == (f"{__name__}:tint", {"amount": 0.25})


def test_a_class_under_node_is_refused_showing_the_node_written_as_one_function() -> None:
    """Mental-revert: read a class's fields and pins as its ports, and a node in the retired form
    loads, with no word of the form that replaced it."""

    class Gain:
        video: InputPin[Texture]
        level: float = 1.0

        def process(self, ctx: RuntimeContextLimitedAccess) -> None: ...

    for declare in (lambda: node(Gain), lambda: node(execution="reactive")(Gain)):
        refused = refusal_of(declare)
        assert (refused.finding_code, refused.node) == ("declaration_retired", "Gain")
        assert "`Gain` is declared with a class under `@node`, which is retired" in str(refused)
        for function_form in ("on a `def`", "Input[float] = Input(4.0)", "each_frame", "computed("):
            assert function_form in str(refused)

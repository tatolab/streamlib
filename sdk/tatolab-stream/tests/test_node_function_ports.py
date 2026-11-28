# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`@node(...)` on a function declares each value input as an `Input` and each stream, trigger and output as a pin, and refuses each mistake as a `NodeDeclarationRefusal`."""

from __future__ import annotations

from typing import Annotated, Any, Callable

import pytest

from tatolab.stream import (
    Audio,
    AudioWindowContract,
    Description,
    FormerlyNamed,
    Input,
    InputPin,
    InputValueLifetime,
    NodeDeclarationRefusal,
    NumberRange,
    OutputPin,
    PinMeaning,
    StreamDelivery,
    Texture,
    Trigger,
    WireOnly,
    node,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_EXECUTION_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_KIND_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)


@node.meta(description="Blurs each picture by its radius.")
@node(execution="reactive")
def Blur(
    *,
    video: Annotated[InputPin[Texture], Description("The pictures to blur.")],
    voice: InputPin[Annotated[Audio, StreamDelivery("ordered", borrow_quota=2)]],
    reset: InputPin[Trigger],
    radius: Input[
        Annotated[float, NumberRange(minimum=0.0, maximum=64.0), PinMeaning("radius")]
    ] = Input(4.0),
    wet: Input[Annotated[float, WireOnly()]] = Input(1.0),
    nudge: Input[Annotated[float, InputValueLifetime("reset")]] = Input(0.0),
    result: OutputPin[Texture],
    level: OutputPin[float],
) -> None:
    return None


def input_port(declared: Any, name: str) -> dict[str, Any]:
    return next(port for port in getattr(declared, NODE_DECLARATION_INPUT_PORTS_STAMP) if port["name"] == name)


def output_port(declared: Any, name: str) -> dict[str, Any]:
    return next(port for port in getattr(declared, NODE_DECLARATION_OUTPUT_PORTS_STAMP) if port["name"] == name)


def test_a_node_function_is_a_process_node_with_no_config() -> None:
    assert getattr(Blur, NODE_DECLARATION_KIND_STAMP) == "process"
    assert not hasattr(Blur, "__tatolab_node_config_class__")
    assert not hasattr(Blur, "__tatolab_node_config_schema__")
    assert getattr(Blur, NODE_DECLARATION_EXECUTION_STAMP) == {"mode": "reactive", "interval_ms": 0}


def test_each_input_and_pin_is_a_port_in_the_order_written() -> None:
    assert [port["name"] for port in getattr(Blur, NODE_DECLARATION_INPUT_PORTS_STAMP)] == [
        "video",
        "voice",
        "reset",
        "radius",
        "wet",
        "nudge",
    ]
    assert [port["name"] for port in getattr(Blur, NODE_DECLARATION_OUTPUT_PORTS_STAMP)] == [
        "result",
        "level",
    ]


def test_an_input_is_a_value_input_with_its_type_annotations_and_default() -> None:
    radius = input_port(Blur, "radius")

    assert radius == {
        "name": "radius",
        "description": "",
        "pin_declaration": {
            "pin_type": {"kind": "is", "pin_type": {"type": "float"}},
            "flow": "value",
            "annotations": {"range": {"minimum": 0.0, "maximum": 64.0}, "meaning": "radius"},
        },
        "pin_default": 4.0,
        "input_mode": {"mode": "wire_or_value"},
    }


def test_an_input_states_wire_only_and_its_lifetime() -> None:
    """An input stating no mode states it takes a wire or a value, so the runtime sets it any
    value of its type and not only what a Channels input carries."""
    assert input_port(Blur, "wet")["input_mode"] == {"mode": "wire_only"}
    assert input_port(Blur, "nudge")["lifetime"] == "reset"
    assert input_port(Blur, "radius")["input_mode"] == {"mode": "wire_or_value"}
    assert "lifetime" not in input_port(Blur, "radius")


def test_an_input_pin_streams_its_type_and_states_no_default() -> None:
    video = input_port(Blur, "video")

    assert video == {
        "name": "video",
        "description": "The pictures to blur.",
        "pin_declaration": {
            "pin_type": {"kind": "is", "pin_type": {"type": "texture"}},
            "flow": "stream",
        },
    }


def test_a_stream_delivery_reaches_the_port_as_its_delivery_and_borrow_quota() -> None:
    voice = input_port(Blur, "voice")

    assert voice["delivery_profile"] == "ordered"
    assert voice["borrow_quota"] == 2


def test_an_input_pin_of_a_trigger_fires() -> None:
    reset = input_port(Blur, "reset")

    assert reset["pin_declaration"]["flow"] == "trigger"
    assert "delivery_profile" not in reset


def test_an_output_pin_declares_its_type() -> None:
    assert output_port(Blur, "level") == {
        "name": "level",
        "description": "",
        "pin_declaration": {
            "pin_type": {"kind": "is", "pin_type": {"type": "float"}},
            "flow": "value",
        },
    }


@node(execution="continuous", interval_ms=16, scheduling="high")
def Ticker(*, rate: Input[float] = Input(1.0), tick: OutputPin[Trigger]) -> None:
    return None


def test_a_node_function_takes_how_it_runs_as_keyword_arguments() -> None:
    assert getattr(Ticker, NODE_DECLARATION_EXECUTION_STAMP) == {"mode": "continuous", "interval_ms": 16}
    assert output_port(Ticker, "tick")["pin_declaration"]["flow"] == "trigger"


def refusal_of(function: Callable[..., Any]) -> NodeDeclarationRefusal:
    with pytest.raises(NodeDeclarationRefusal) as refused:
        node(execution="reactive")(function)
    return refused.value


def input_of_a_stream_type(*, voice: Input[Audio]) -> None: ...


def test_an_input_of_a_type_that_only_streams_is_refused_with_its_pin() -> None:
    refusal = refusal_of(input_of_a_stream_type)

    assert refusal.finding_code == "field_typed_as_a_stream"
    assert (refusal.node, refusal.pin) == ("input_of_a_stream_type", "voice")
    assert "To fix: `voice: InputPin[Audio]`" in str(refusal)


def delivery_on_an_input(*, gain: Input[Annotated[float, StreamDelivery("newest")]] = Input(1.0)) -> None: ...


def delivery_on_an_output(*, result: OutputPin[Annotated[Texture, StreamDelivery("newest")]]) -> None: ...


def delivery_on_a_trigger(*, reset: InputPin[Annotated[Trigger, StreamDelivery("ordered")]]) -> None: ...


def newest_beside_a_window(
    *,
    voice: InputPin[
        Annotated[
            Audio,
            StreamDelivery("newest"),
            AudioWindowContract(sample_rate=16_000, dtype="f32", window_size=512),
        ]
    ],
) -> None: ...


@pytest.mark.parametrize(
    ("function", "pin", "words"),
    [
        (delivery_on_an_input, "gain", "Stream input takes"),
        (delivery_on_an_output, "result", "Stream input takes"),
        (delivery_on_a_trigger, "reset", "is a Trigger"),
        (newest_beside_a_window, "voice", "beside `StreamDelivery('newest')`"),
    ],
)
def test_a_stream_delivery_anywhere_but_a_stream_input_is_refused(
    function: Callable[..., Any], pin: str, words: str
) -> None:
    refusal = refusal_of(function)

    assert refusal.finding_code == "pin_annotation_not_taken"
    assert refusal.pin == pin
    assert words in str(refusal)


def Louder(gain: Annotated[float, StreamDelivery("ordered")] = 1.0) -> float:
    return gain


def test_a_stream_delivery_on_a_pure_function_parameter_is_refused() -> None:
    with pytest.raises(NodeDeclarationRefusal) as refused:
        node(Louder)

    assert refused.value.finding_code == "pin_annotation_not_taken"


def test_an_audio_window_on_an_input_pin_reaches_the_port() -> None:
    @node(execution="reactive")
    def Listener(
        *,
        voice: InputPin[
            Annotated[Audio, AudioWindowContract(sample_rate=16_000, dtype="f32", window_size=512)]
        ],
    ) -> None: ...

    assert input_port(Listener, "voice")["audio_window"]["window_size"] == 512


def former_name_of_another_input(
    *,
    old: Input[float] = Input(0.0),
    current: Input[Annotated[float, FormerlyNamed("old")]] = Input(1.0),
) -> None: ...


def test_a_former_name_another_input_holds_is_refused() -> None:
    refusal = refusal_of(former_name_of_another_input)

    assert refusal.finding_code == "former_name_finds_no_one_pin"
    assert "or rename `old`" in str(refusal)


def test_an_optional_input_and_pin_keep_their_declaration_annotations() -> None:
    @node(execution="reactive")
    def OptionalOnes(
        *,
        gain: Input[float | None] = Input(None),
        voice: InputPin[Annotated[Audio, StreamDelivery("ordered")] | None],
    ) -> None: ...

    gain = input_port(OptionalOnes, "gain")
    assert gain["input_mode"] == {"mode": "wire_or_value"}
    assert gain["pin_default"] is None
    assert input_port(OptionalOnes, "voice")["delivery_profile"] == "ordered"

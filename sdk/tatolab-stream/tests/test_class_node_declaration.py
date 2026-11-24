# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Bare `@node` on a class declares each value input as a field and each stream, trigger and output as a pin, and refuses each mistake as a `NodeDeclarationRefusal`."""

from __future__ import annotations

from typing import Annotated, Any, ClassVar, Literal

import pytest

from tatolab.stream import (
    Audio,
    AudioWindowContract,
    Description,
    FormerlyNamed,
    InputPin,
    InputValueLifetime,
    NodeDeclarationRefusal,
    NumberRange,
    OutputPin,
    PinMeaning,
    SettingOnly,
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
@node
class Blur:
    video: Annotated[InputPin[Texture], Description("The pictures to blur.")]
    voice: InputPin[Annotated[Audio, StreamDelivery("ordered", borrow_quota=2)]]
    reset: InputPin[Trigger]
    radius: Annotated[float, NumberRange(minimum=0.0, maximum=64.0), PinMeaning("radius")] = 4.0
    quality: Annotated[Literal["fast", "fine"], SettingOnly()] = "fast"
    wet: Annotated[float, WireOnly()] = 1.0
    nudge: Annotated[float, InputValueLifetime("reset")] = 0.0
    result: OutputPin[Texture]
    level: OutputPin[float]

    frames_seen: ClassVar[int] = 0
    _kernel: list[float]

    def process(self) -> None:
        self._kernel = []


def input_port(declared: Any, name: str) -> dict[str, Any]:
    return next(port for port in getattr(declared, NODE_DECLARATION_INPUT_PORTS_STAMP) if port["name"] == name)


def output_port(declared: Any, name: str) -> dict[str, Any]:
    return next(port for port in getattr(declared, NODE_DECLARATION_OUTPUT_PORTS_STAMP) if port["name"] == name)


def test_a_class_node_is_a_process_node_with_no_config() -> None:
    assert getattr(Blur, NODE_DECLARATION_KIND_STAMP) == "process"
    assert not hasattr(Blur, "__tatolab_node_config_class__")
    assert not hasattr(Blur, "__tatolab_node_config_schema__")
    assert getattr(Blur, NODE_DECLARATION_EXECUTION_STAMP) == {"mode": "reactive", "interval_ms": 0}


def test_each_field_and_pin_is_a_port_in_the_order_written_and_state_is_none() -> None:
    assert [port["name"] for port in getattr(Blur, NODE_DECLARATION_INPUT_PORTS_STAMP)] == [
        "video",
        "voice",
        "reset",
        "radius",
        "quality",
        "wet",
        "nudge",
    ]
    assert [port["name"] for port in getattr(Blur, NODE_DECLARATION_OUTPUT_PORTS_STAMP)] == [
        "result",
        "level",
    ]


def test_a_field_is_a_value_input_with_its_type_annotations_and_default() -> None:
    radius = input_port(Blur, "radius")

    assert radius == {
        "name": "radius",
        "description": "",
        "family": {"family": "channels"},
        "pin_declaration": {
            "pin_type": {"kind": "is", "pin_type": {"type": "float"}},
            "flow": "value",
            "annotations": {"range": {"minimum": 0.0, "maximum": 64.0}, "meaning": "radius"},
        },
        "pin_default": 4.0,
        "input_mode": {"mode": "wire_or_value"},
    }


def test_a_setting_only_field_takes_no_wire_and_is_read_at_setup() -> None:
    quality = input_port(Blur, "quality")

    assert quality["input_mode"] == {"mode": "setting_only", "read": "at_setup"}
    assert quality["pin_default"] == "fast"
    assert quality["family"] == {"family": "records"}


def test_a_field_states_wire_only_and_its_lifetime() -> None:
    """A field stating no mode states it takes a wire or a value, so the runtime sets it any value
    of its type and not only what a Channels input carries."""
    assert input_port(Blur, "wet")["input_mode"] == {"mode": "wire_only"}
    assert input_port(Blur, "nudge")["lifetime"] == "reset"
    assert input_port(Blur, "radius")["input_mode"] == {"mode": "wire_or_value"}
    assert "lifetime" not in input_port(Blur, "radius")


def test_an_input_pin_streams_its_type_and_states_no_default() -> None:
    video = input_port(Blur, "video")

    assert video == {
        "name": "video",
        "description": "The pictures to blur.",
        "family": {"family": "image"},
        "pin_declaration": {
            "pin_type": {"kind": "is", "pin_type": {"type": "texture"}},
            "flow": "stream",
        },
    }


def test_a_stream_delivery_reaches_the_port_as_its_delivery_and_borrow_quota() -> None:
    voice = input_port(Blur, "voice")

    assert voice["family"] == {"family": "channels"}
    assert voice["delivery_profile"] == "ordered"
    assert voice["borrow_quota"] == 2


def test_an_input_pin_of_a_trigger_fires() -> None:
    reset = input_port(Blur, "reset")

    assert reset["pin_declaration"]["flow"] == "trigger"
    assert reset["family"] == {"family": "records"}
    assert "delivery_profile" not in reset


def test_an_output_pin_declares_its_type_and_family() -> None:
    assert output_port(Blur, "result")["family"] == {"family": "image"}
    assert output_port(Blur, "level") == {
        "name": "level",
        "description": "",
        "family": {"family": "channels", "names": {"stated": ["level"]}, "dtype": "f64"},
        "pin_declaration": {
            "pin_type": {"kind": "is", "pin_type": {"type": "float"}},
            "flow": "value",
        },
    }


@node(execution="continuous", interval_ms=16, scheduling="high")
class Ticker:
    rate: float = 1.0
    tick: OutputPin[Trigger]


def test_a_class_node_takes_how_it_runs_as_keyword_arguments() -> None:
    assert getattr(Ticker, NODE_DECLARATION_EXECUTION_STAMP) == {"mode": "continuous", "interval_ms": 16}
    assert output_port(Ticker, "tick")["pin_declaration"]["flow"] == "trigger"


def test_keyword_arguments_on_a_function_are_refused() -> None:
    def Half(x: float = 0.0) -> float:
        return x / 2

    with pytest.raises(TypeError, match="A function takes `@node` bare"):
        node(execution="continuous")(Half)  # pyright: ignore[reportArgumentType]


def refusal_of(node_class: type) -> NodeDeclarationRefusal:
    with pytest.raises(NodeDeclarationRefusal) as refused:
        node(node_class)
    return refused.value


class FieldOfAStreamType:
    voice: Audio


def test_a_field_of_a_type_that_only_streams_is_refused_with_its_pin() -> None:
    refusal = refusal_of(FieldOfAStreamType)

    assert refusal.finding_code == "field_typed_as_a_stream"
    assert (refusal.node, refusal.pin) == ("FieldOfAStreamType", "voice")
    assert "To fix: `voice: InputPin[Audio]`" in str(refusal)


class DeliveryOnAField:
    gain: Annotated[float, StreamDelivery("newest")] = 1.0


class DeliveryOnAnOutput:
    result: OutputPin[Annotated[Texture, StreamDelivery("newest")]]


class DeliveryOnATrigger:
    reset: InputPin[Annotated[Trigger, StreamDelivery("ordered")]]


class NewestBesideAWindow:
    voice: InputPin[
        Annotated[
            Audio,
            StreamDelivery("newest"),
            AudioWindowContract(sample_rate=16_000, dtype="f32", window_size=512),
        ]
    ]


@pytest.mark.parametrize(
    ("node_class", "pin", "words"),
    [
        (DeliveryOnAField, "gain", "only a class node's Stream input takes"),
        (DeliveryOnAnOutput, "result", "only a class node's Stream input takes"),
        (DeliveryOnATrigger, "reset", "is a Trigger"),
        (NewestBesideAWindow, "voice", "beside `StreamDelivery('newest')`"),
    ],
)
def test_a_stream_delivery_anywhere_but_a_stream_input_is_refused(
    node_class: type, pin: str, words: str
) -> None:
    refusal = refusal_of(node_class)

    assert refusal.finding_code == "pin_annotation_not_taken"
    assert refusal.pin == pin
    assert words in str(refusal)


def Louder(gain: Annotated[float, StreamDelivery("ordered")] = 1.0) -> float:
    return gain


def test_a_stream_delivery_on_a_function_parameter_is_refused() -> None:
    with pytest.raises(NodeDeclarationRefusal) as refused:
        node(Louder)

    assert refused.value.finding_code == "pin_annotation_not_taken"


def test_an_audio_window_on_an_input_pin_reaches_the_port() -> None:
    @node
    class Listener:
        voice: InputPin[
            Annotated[Audio, AudioWindowContract(sample_rate=16_000, dtype="f32", window_size=512)]
        ]

    assert input_port(Listener, "voice")["audio_window"]["window_size"] == 512


class TakesAConfig:
    def __init__(self, config: dict[str, Any]) -> None:
        self.config = config


class TakesParameters:
    def __init__(self, radius: float) -> None:
        self.radius = radius


class PublicStateNotAPin:
    frames: dict[str, int] = {}  # noqa: RUF012


class PinGivenAValue:
    video: InputPin[Texture] = None  # pyright: ignore[reportAssignmentType]


class SettingOnlyOnAPin:
    video: InputPin[Annotated[Texture, SettingOnly()]]


class TwoModes:
    gain: Annotated[float, SettingOnly(), WireOnly()] = 1.0


class FormerNameOfAnotherField:
    old: float = 0.0
    current: Annotated[float, FormerlyNamed("old")] = 1.0


@pytest.mark.parametrize(
    ("node_class", "finding_code", "fix"),
    [
        (TakesAConfig, "class_takes_a_config", "`radius: Annotated[float, SettingOnly()] = 4.0`"),
        (TakesParameters, "init_takes_parameters", "declare each value the node reads as a field"),
        (PublicStateNotAPin, "attribute_not_a_pin", "`_frames`, or a `ClassVar[...]`"),
        (PinGivenAValue, "pin_has_a_value", "drop the value"),
        (SettingOnlyOnAPin, "pin_annotation_not_taken", "only a class node's field takes"),
        (TwoModes, "pin_annotation_repeated", "keep one"),
        (FormerNameOfAnotherField, "former_name_finds_no_one_pin", "or rename `old`"),
    ],
)
def test_each_mistake_in_a_class_node_is_refused_with_what_to_write(
    node_class: type, finding_code: str, fix: str
) -> None:
    refusal = refusal_of(node_class)

    assert refusal.finding_code == finding_code
    assert refusal.node == node_class.__name__
    assert fix in str(refusal)


def test_an_optional_field_and_pin_keep_their_declaration_annotations() -> None:
    @node
    class OptionalOnes:
        gain: Annotated[float, SettingOnly()] | None = None
        voice: InputPin[Annotated[Audio, StreamDelivery("ordered")] | None]

    gain = input_port(OptionalOnes, "gain")
    assert gain["input_mode"] == {"mode": "setting_only", "read": "at_setup"}
    assert gain["pin_default"] is None
    assert input_port(OptionalOnes, "voice")["delivery_profile"] == "ordered"


def test_class_state_annotated_as_a_class_variable_is_no_port() -> None:
    @node
    class AnnotatedState:
        gain: float = 1.0
        frames_seen: Annotated[ClassVar[int], Description("Frames seen.")] = 0

    assert [port["name"] for port in getattr(AnnotatedState, NODE_DECLARATION_INPUT_PORTS_STAMP)] == ["gain"]

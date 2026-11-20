# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The port family a typed pin's value travels on."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any, Final

from ._contract_pin_types import PinDeclarationOnTheWire, PinType
from ._pin_types import PIN_VALUE_CLASSES

__all__ = [
    "family_a_pin_travels_on",
    "family_a_pin_type_travels_on",
    "family_declaration_of_a_typed_output",
]


def family_a_pin_type_travels_on(pin_type: "Mapping[str, Any]") -> str:
    """The port family whose message a value of `pin_type` travels as.

    A number, a Boolean and a value of parts travel as a control message on Channels, and a
    List of them as an Array frame; a String, a Menu, an Asset, a Trigger and any other List as
    a Records message. A Texture travels as an Image frame, Audio as a Channels block, a Tensor
    as an Array frame and encoded media as Packets. A named type travels as the bytes its class
    packs, which belong to no family; its port is declared on Records, and the runtime refuses
    its link to any pin that states no type.
    """
    if pin_type["type"] == "list_of":
        return "array" if pin_type["element_type"]["type"] in _TYPES_OF_A_CONTROL_MESSAGE else "records"
    if pin_type["type"] in _TYPES_OF_A_CONTROL_MESSAGE:
        return "channels"
    return _FAMILY_OF_EACH_STREAM_PIN_TYPE.get(pin_type["type"], "records")


def family_a_pin_travels_on(declaration: PinDeclarationOnTheWire) -> str:
    """The port family whose message the value of a pin declaring exactly one type travels as."""
    return family_a_pin_type_travels_on(_the_one_pin_type_declared(declaration))


def family_declaration_of_a_typed_output(
    port_name: str, declaration: PinDeclarationOnTheWire
) -> dict[str, Any]:
    """The family declaration of the output `port_name` a pin of exactly one type declares: a
    value on Channels names one channel per part, or the port itself for a number, each a 64-bit
    float, and encoded media whose type names one codec states it as the codec of every packet."""
    pin_type = _the_one_pin_type_declared(declaration)
    family = family_a_pin_type_travels_on(pin_type)
    family_declaration: dict[str, Any] = {"family": family}
    if family == "channels" and pin_type["type"] in _TYPES_OF_A_CONTROL_MESSAGE:
        value_class = next(
            (value_class for value_class in PIN_VALUE_CLASSES if value_class.pin_type == pin_type),
            None,
        )
        channel_names = [port_name] if value_class is None else list(value_class.part_names())
        family_declaration.update({"names": {"stated": channel_names}, "dtype": "f64"})
    if pin_type["type"] in ("encoded_video", "encoded_audio"):
        codecs = pin_type.get("codecs") or []
        if len(codecs) == 1:
            family_declaration["codec"] = codecs[0]
    return family_declaration


def _the_one_pin_type_declared(declaration: PinDeclarationOnTheWire) -> PinType:
    declared_pin_type = declaration["pin_type"]
    assert declared_pin_type["kind"] == "is", "a typed pin declares exactly one type"
    return declared_pin_type["pin_type"]


# The pin types whose value travels as a control message, its parts one on each channel.
_TYPES_OF_A_CONTROL_MESSAGE: Final[frozenset[str]] = frozenset(
    {"boolean", "integer", "float", "color", "vector", "rectangle", "transform"}
)

# The family each pin type that holds no stored value travels on.
_FAMILY_OF_EACH_STREAM_PIN_TYPE: Final[dict[str, str]] = {
    "texture": "image",
    "audio": "channels",
    "tensor": "array",
    "encoded_video": "packets",
    "encoded_audio": "packets",
    "trigger": "records",
}

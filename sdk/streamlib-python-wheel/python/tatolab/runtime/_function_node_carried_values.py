# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The values a function node's signature carries, its prepared forms, and the refusal when it does not compile."""

from __future__ import annotations

import dataclasses
import importlib
import json
import typing
from typing import Any, Callable, Optional

from tatolab.runtime._function_node_stored_values import STORED_VALUE_TYPES, the_record_an_island_carries

#: The value type a function node signature gives an `UnreadMessage` input or output. Spelled
#: here, not imported, because the `tatolab-stream` in a project's venv may predate it, and the
#: string is the wire contract both sides hold.
UNREAD_MESSAGE_VALUE_TYPE = "unread_message"

#: The value types a function node signature gives a Color, a Vector of each size, a Rectangle
#: and a Transform, each carried as its parts: a `tuple` of numbers in the order the type packs
#: them. Spelled here for the reason `UNREAD_MESSAGE_VALUE_TYPE` is: the strings are the wire
#: contract both sides hold.
PART_VALUE_TYPES = ("color", "vector2", "vector3", "vector4", "rectangle", "transform")

#: The value type a function node signature gives an input or output of a module's named pin
#: type, carried as the bytes its class packs. Spelled here for the reason
#: `UNREAD_MESSAGE_VALUE_TYPE` is.
NAMED_VALUE_TYPE = "named"

#: A value an input reads while nothing links into it, as the island carries it: a number as an
#: f64, a value of parts as the `tuple` of its parts, and a String, a Menu, an Asset or a List
#: as the record `{"value": <stored JSON form>}` it travels as.
PinValueCarried = typing.Union[float, "tuple[float, ...]", "dict[str, Any]"]

# The scalar value types an evaluate entry carries, each as an f64.
_SCALAR_VALUE_TYPES_AN_EVALUATE_ENTRY_CARRIES = {float: "float", int: "int", bool: "bool"}

# The values of parts an evaluate entry carries, each as one f64 per part in the order its type
# packs them. A Transform's parts are one tuple, which a compiled body does not build.
PART_VALUE_TYPES_AN_EVALUATE_ENTRY_CARRIES = ("color", "vector2", "vector3", "vector4", "rectangle")

# The value types a body run as Python takes and gives: the scalars, a value of parts, a String,
# a Menu, an Asset, a List, a value of a named type, and a message it hands on unread.
_VALUE_TYPES_A_BODY_RUN_AS_PYTHON_CARRIES = (
    "float",
    "int",
    "bool",
    *PART_VALUE_TYPES,
    *STORED_VALUE_TYPES,
    NAMED_VALUE_TYPE,
    UNREAD_MESSAGE_VALUE_TYPE,
)

# The largest integer magnitude an f64 holds exactly.
_LARGEST_INTEGER_AN_F64_HOLDS_EXACTLY = 2**53


class FunctionNodeDoesNotCompile(Exception):
    """A function node whose body or signature the evaluate entry cannot carry."""


# What a node whose every value is a number, a bool or a dict carries beyond numbers: nothing.
_NO_VALUES_CARRIED_BEYOND_NUMBERS: "dict[str, list[Any]]" = {
    "inputs_that_take_none": [],
    "inputs_carrying_an_unread_message": [],
    "outputs_carrying_an_unread_message": [],
    "outputs_that_may_hand_out_nothing": [],
    "inputs_carrying_a_stored_value": [],
    "outputs_carrying_a_stored_value": [],
    "inputs_carried_in_parts": [],
    "outputs_carried_in_parts": [],
}


def _how_the_values_of_a_signature_are_carried(
    function_signature: "dict[str, Any]",
) -> "dict[str, list[Any]]":
    """Which inputs and outputs of `function_signature` take `None`, carry an `UnreadMessage`, or may hand out nothing.

    Each input and output that carries a String, a Menu, an Asset or a List is listed with its
    pin type as JSON, which the native side packs and reads it by, and each that carries a
    Color, a Vector, a Rectangle or a Transform with its value type, which a compiled call
    carries in one slot per part.
    """
    inputs = function_signature["inputs"]
    outputs = function_signature["outputs"]
    return {
        "inputs_that_take_none": [
            index for index, each_input in enumerate(inputs) if each_input["optional"]
        ],
        "inputs_carrying_an_unread_message": [
            index
            for index, each_input in enumerate(inputs)
            if each_input["value_type"] == UNREAD_MESSAGE_VALUE_TYPE
        ],
        "outputs_carrying_an_unread_message": [
            index
            for index, each_output in enumerate(outputs)
            if each_output["value_type"] == UNREAD_MESSAGE_VALUE_TYPE
        ],
        "outputs_that_may_hand_out_nothing": [
            index
            for index, each_output in enumerate(outputs)
            if each_output.get("may_hand_out_nothing")
        ],
        "inputs_carrying_a_stored_value": _each_carrying_a_stored_value(inputs),
        "outputs_carrying_a_stored_value": _each_carrying_a_stored_value(outputs),
        "inputs_carried_in_parts": _each_carried_in_parts(inputs),
        "outputs_carried_in_parts": _each_carried_in_parts(outputs),
    }


def _each_carried_in_parts(ports: "list[dict[str, Any]]") -> "list[tuple[int, str]]":
    return [
        (index, each_port["value_type"])
        for index, each_port in enumerate(ports)
        if each_port["value_type"] in PART_VALUE_TYPES
    ]


def _each_carrying_a_stored_value(ports: "list[dict[str, Any]]") -> "list[tuple[int, str]]":
    return [
        (index, json.dumps(each_port["pin_type"]))
        for index, each_port in enumerate(ports)
        if each_port["value_type"] in STORED_VALUE_TYPES
    ]


@dataclasses.dataclass(frozen=True)
class FunctionNodeCompiledToItsEvaluateEntry:
    """A function node type compiled to its evaluate entry, and the order its values travel in."""

    evaluate_entry_address: int
    input_ports: "tuple[str, ...]"
    pin_defaults_by_input_port: "dict[str, PinValueCarried]"
    output_ports: "tuple[str, ...]"
    config_fields: "tuple[str, ...]"
    config_field_defaults: "dict[str, float]"
    state_fields: "tuple[str, ...]"
    initial_state_values: "tuple[float, ...]"
    is_stateful: bool
    body_loaded_from_numbas_cache: bool
    compiled_wrapper_kept_alive: Any
    input_value_types: "tuple[str, ...]" = ()
    how_its_values_are_carried: "dict[str, list[Any]]" = dataclasses.field(
        default_factory=lambda: dict(_NO_VALUES_CARRIED_BEYOND_NUMBERS)
    )
    input_pin_types: "tuple[Optional[dict[str, Any]], ...]" = ()
    output_pin_types: "tuple[Optional[dict[str, Any]], ...]" = ()


@dataclasses.dataclass(frozen=True)
class FunctionNodeRunAsPythonInItsReader:
    """A function node type whose body runs as Python in its reader's interpreter, and why it does not compile."""

    body: Callable[..., Any]
    function_signature: "dict[str, Any]"
    input_ports: "tuple[str, ...]"
    pin_defaults_by_input_port: "dict[str, PinValueCarried]"
    output_ports: "tuple[str, ...]"
    state_record_class: Optional[type]
    state_value_types: "dict[str, str]"
    initial_state_values: "tuple[float, ...]"
    is_stateful: bool
    why_it_does_not_compile: str
    input_value_types: "tuple[str, ...]" = ()
    how_its_values_are_carried: "dict[str, list[Any]]" = dataclasses.field(
        default_factory=lambda: dict(_NO_VALUES_CARRIED_BEYOND_NUMBERS)
    )
    input_pin_types: "tuple[Optional[dict[str, Any]], ...]" = ()
    output_pin_types: "tuple[Optional[dict[str, Any]], ...]" = ()


FunctionNodeReadyToEvaluate = typing.Union[
    FunctionNodeCompiledToItsEvaluateEntry, FunctionNodeRunAsPythonInItsReader
]


def _pin_defaults_by_input_port(
    import_path: str, function_signature: "dict[str, Any]"
) -> "dict[str, PinValueCarried]":
    return {
        each_input["port"]: pin_value_carried(
            each_input["value_type"],
            each_input["default"],
            f"{import_path}'s input `{each_input['parameter']}` default",
            each_input.get("pin_type"),
        )
        for each_input in function_signature["inputs"]
        if each_input.get("default") is not None
    }



def pin_value_carried(
    value_type: str, stored_value: Any, what_it_is: str, pin_type: "Optional[dict[str, Any]]" = None
) -> PinValueCarried:
    """`stored_value`, the stored JSON form of a value of `value_type`, as the island carries it.

    A String, a Menu, an Asset or a List is checked against `pin_type`, the one its signature
    entry states.
    """
    if value_type in STORED_VALUE_TYPES:
        if pin_type is None:
            raise FunctionNodeDoesNotCompile(
                f"{what_it_is}: its signature states no pin type for its {value_type} value"
            )
        try:
            return the_record_an_island_carries(pin_type, stored_value, what_it_is)
        except (TypeError, ValueError) as not_of_its_type:
            raise FunctionNodeDoesNotCompile(str(not_of_its_type)) from not_of_its_type
    if value_type in PART_VALUE_TYPES:
        try:
            return value_class_of_a_part_value_type(value_type).from_stored_value(stored_value).parts
        except (TypeError, ValueError) as not_of_its_type:
            raise FunctionNodeDoesNotCompile(f"{what_it_is}: {not_of_its_type}") from not_of_its_type
    return _exactly_as_f64(stored_value, what_it_is)


# The number of components a Vector of each value type holds.
_COMPONENTS_OF_EACH_VECTOR_VALUE_TYPE = {"vector2": 2, "vector3": 3, "vector4": 4}


def parts_a_value_type_reads(value_type: str, carried: Any) -> Any:
    """The parts an input of `value_type`, one of `PART_VALUE_TYPES`, reads of what another node
    of its island handed out, with the free conversion a link allows.

    One number spreads over every component of a Vector, and over the red, green and blue of a
    Color, opaque. A Vector or a Color into one of another size keeps the components it shares
    and reads each missing one as 0, a Color's alpha as 1. A Rectangle and a Transform read
    their parts as they are. Mirrors the reading of a link from outside the island.
    """
    if value_type == "color":
        component_count = 4
    else:
        component_count = _COMPONENTS_OF_EACH_VECTOR_VALUE_TYPE.get(value_type)
        if component_count is None:
            return carried
    if isinstance(carried, (int, float)) and not isinstance(carried, bool):
        number = float(carried)
        return (number, number, number, 1.0) if value_type == "color" else (number,) * component_count
    if not isinstance(carried, tuple):
        return carried
    missing = (0.0, 0.0, 0.0, 1.0 if value_type == "color" else 0.0)
    return tuple(
        carried[index] if index < len(carried) else missing[index] for index in range(component_count)
    )


def value_class_of_a_part_value_type(value_type: str) -> Any:
    """The `tatolab.stream` value class a function node's signature names `value_type`, one of `PART_VALUE_TYPES`.

    Imported here, not at the top, because the `tatolab-stream` in a project's venv may predate
    the value classes; only a node that declares one names its value type.
    """
    from tatolab.stream._pin_types import PIN_VALUE_CLASSES

    return next(value_class for value_class in PIN_VALUE_CLASSES if value_class.value_type == value_type)


def _initial_state_values(
    import_path: str, state_record_class: Optional[type], state_value_types: "dict[str, str]"
) -> "tuple[float, ...]":
    """The state a new node of this type starts from: one record of its defaults, as f64s."""
    if state_record_class is None:
        return ()
    initial_state = state_record_class()
    return tuple(
        _exactly_as_f64(getattr(initial_state, field), f"{import_path}'s state `{field}` default")
        for field in state_value_types
    )


def _exactly_as_f64(value: Any, what_it_is: str) -> float:
    """`value` as the f64 an evaluate entry carries, refusing an int it cannot hold exactly."""
    if isinstance(value, bool):
        return 1.0 if value else 0.0
    if isinstance(value, int) and abs(value) > _LARGEST_INTEGER_AN_F64_HOLDS_EXACTLY:
        raise FunctionNodeDoesNotCompile(
            f"{what_it_is} is {value}, and an evaluate entry carries each value as an f64, "
            f"which holds an integer exactly only up to 2**53"
        )
    return float(value)


def the_dataclass_a_signature_entry_writes_its_bundle_as(
    signature_entry: "dict[str, Any]", what_it_is: str
) -> Optional[type]:
    """The dataclass a signature entry's bundle, or its List's bundle elements, is written as,
    found by the import path it names; `None` for an entry that names none.

    Refused as `FunctionNodeDoesNotCompile`, naming `what_it_is`, when the path finds no
    dataclass.
    """
    import_path = signature_entry.get("bundle_class")
    if import_path is None:
        return None
    module_name, _, qualified_name = import_path.partition(":")
    try:
        found: Any = importlib.import_module(module_name)
        for attribute in qualified_name.split("."):
            found = getattr(found, attribute)
    except (ImportError, AttributeError, ValueError) as not_found:
        raise FunctionNodeDoesNotCompile(
            f"{what_it_is} is a bundle written as `{import_path}`, and its import path finds no "
            f"class: {not_found}. To fix: keep the dataclass at that path."
        ) from not_found
    if not (isinstance(found, type) and dataclasses.is_dataclass(found)):
        raise FunctionNodeDoesNotCompile(
            f"{what_it_is} is a bundle written as `{import_path}`, whose path names {found!r}, "
            f"not a dataclass. To fix: keep the dataclass at that path."
        )
    return found


def the_class_a_named_pin_type_names(named_pin_type: str, what_it_is: str) -> type:
    """The class whose import path, `module:QualifiedClassName`, is the name of `named_pin_type`.

    Refused as `FunctionNodeDoesNotCompile`, naming `what_it_is`, when the path finds no class
    with the `from_bytes` and `to_bytes` a value of the type travels by.
    """
    module_name, _, qualified_name = named_pin_type.partition(":")
    try:
        found: Any = importlib.import_module(module_name)
        for attribute in qualified_name.split("."):
            found = getattr(found, attribute)
    except (ImportError, AttributeError, ValueError) as not_found:
        raise FunctionNodeDoesNotCompile(
            f"{what_it_is} is the named type `{named_pin_type}`, and its import path finds no "
            f"class: {not_found}. To fix: keep the class at that path, or retype the pin."
        ) from not_found
    if not isinstance(found, type) or not callable(getattr(found, "from_bytes", None)) or not callable(
        getattr(found, "to_bytes", None)
    ):
        raise FunctionNodeDoesNotCompile(
            f"{what_it_is} is the named type `{named_pin_type}`, whose path names {found!r}, "
            f"not a class with `from_bytes` and `to_bytes`. To fix: put `@named_pin_type` on "
            f"the class at that path."
        )
    return found

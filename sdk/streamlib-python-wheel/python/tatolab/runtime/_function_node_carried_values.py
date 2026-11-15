# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The values a function node's signature carries, its prepared forms, and the refusal when it does not compile."""

from __future__ import annotations

import dataclasses
import typing
from typing import Any, Callable, Optional




#: The value type a function node signature gives an `UnreadMessage` input or output. Spelled
#: here, not imported, because the `tatolab-stream` in a project's venv may predate it, and the
#: string is the wire contract both sides hold.
UNREAD_MESSAGE_VALUE_TYPE = "unread_message"

# The scalar value types an evaluate entry carries, each as an f64.
_SCALAR_VALUE_TYPES_AN_EVALUATE_ENTRY_CARRIES = {float: "float", int: "int", bool: "bool"}

# The value types a body run as Python takes and gives: the scalars, a `dict` as Records, and a
# message it hands on unread.
_VALUE_TYPES_A_BODY_RUN_AS_PYTHON_CARRIES = (
    "float",
    "int",
    "bool",
    "records",
    UNREAD_MESSAGE_VALUE_TYPE,
)

# The largest integer magnitude an f64 holds exactly.
_LARGEST_INTEGER_AN_F64_HOLDS_EXACTLY = 2**53


class FunctionNodeDoesNotCompile(Exception):
    """A function node whose body or signature the evaluate entry cannot carry."""


# What a node whose every value is a number, a bool or a dict carries beyond numbers: nothing.
_NO_VALUES_CARRIED_BEYOND_NUMBERS: "dict[str, list[int]]" = {
    "inputs_that_take_none": [],
    "inputs_carrying_an_unread_message": [],
    "outputs_carrying_an_unread_message": [],
    "outputs_that_may_hand_out_nothing": [],
}


def _how_the_values_of_a_signature_are_carried(
    function_signature: "dict[str, Any]",
) -> "dict[str, list[int]]":
    """Which inputs and outputs of `function_signature` take `None`, carry an `UnreadMessage`, or may hand out nothing."""
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
    }


@dataclasses.dataclass(frozen=True)
class FunctionNodeCompiledToItsEvaluateEntry:
    """A function node type compiled to its evaluate entry, and the order its values travel in."""

    evaluate_entry_address: int
    input_ports: "tuple[str, ...]"
    pin_defaults_by_input_port: "dict[str, float]"
    output_ports: "tuple[str, ...]"
    config_fields: "tuple[str, ...]"
    config_field_defaults: "dict[str, float]"
    config_field_default_factories: "dict[str, Callable[[], Any]]"
    state_fields: "tuple[str, ...]"
    initial_state_values: "tuple[float, ...]"
    is_stateful: bool
    body_loaded_from_numbas_cache: bool
    compiled_wrapper_kept_alive: Any
    input_value_types: "tuple[str, ...]" = ()
    how_its_values_are_carried: "dict[str, list[int]]" = dataclasses.field(
        default_factory=lambda: dict(_NO_VALUES_CARRIED_BEYOND_NUMBERS)
    )


@dataclasses.dataclass(frozen=True)
class FunctionNodeRunAsPythonInItsReader:
    """A function node type whose body runs as Python in its reader's interpreter, and why it does not compile."""

    body: Callable[..., Any]
    function_signature: "dict[str, Any]"
    input_ports: "tuple[str, ...]"
    pin_defaults_by_input_port: "dict[str, float]"
    output_ports: "tuple[str, ...]"
    ports_carrying_records: "frozenset[str]"
    config_class: Optional[type]
    state_record_class: Optional[type]
    state_value_types: "dict[str, str]"
    initial_state_values: "tuple[float, ...]"
    is_stateful: bool
    why_it_does_not_compile: str
    input_value_types: "tuple[str, ...]" = ()
    how_its_values_are_carried: "dict[str, list[int]]" = dataclasses.field(
        default_factory=lambda: dict(_NO_VALUES_CARRIED_BEYOND_NUMBERS)
    )


FunctionNodeReadyToEvaluate = typing.Union[
    FunctionNodeCompiledToItsEvaluateEntry, FunctionNodeRunAsPythonInItsReader
]


def _pin_defaults_by_input_port(import_path: str, function_signature: "dict[str, Any]") -> "dict[str, float]":
    return {
        each_input["port"]: _exactly_as_f64(
            each_input["default"], f"{import_path}'s input `{each_input['parameter']}` default"
        )
        for each_input in function_signature["inputs"]
        if each_input.get("default") is not None
    }


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

# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Run a function node body as Python in its reader's interpreter."""

from __future__ import annotations

import copy
import dataclasses
import inspect
from typing import Any, Optional


from tatolab.runtime._function_node_carried_values import (
    _exactly_as_f64,
    FunctionNodeDoesNotCompile,
    FunctionNodeReadyToEvaluate,
    FunctionNodeRunAsPythonInItsReader,
    NAMED_VALUE_TYPE,
    PART_VALUE_TYPES,
    UNREAD_MESSAGE_VALUE_TYPE,
    parts_a_value_type_reads,
    the_class_a_named_pin_type_names,
    the_dataclass_a_signature_entry_writes_its_bundle_as,
    value_class_of_a_part_value_type,
)
from tatolab.runtime._function_node_stored_values import (
    STORED_VALUE_RECORD_KEY,
    STORED_VALUE_TYPES,
    the_stored_value_of,
    the_value_a_body_takes,
)


class FunctionNodeBodyRunAsPython:
    """One function node's body, run as Python with its state; the native evaluator calls `evaluate`."""

    def __init__(
        self, run_as_python: FunctionNodeRunAsPythonInItsReader, node_id: str, config: Any
    ) -> None:
        refuse_a_config_given_a_function_node_taking_none(node_id, config)
        self._run_as_python = run_as_python
        self._node_id = node_id
        state_record_class = run_as_python.state_record_class
        self._state = state_record_class() if state_record_class is not None else None
        signature = run_as_python.function_signature
        self._value_type_by_input_parameter = {
            each_input["parameter"]: each_input["value_type"] for each_input in signature["inputs"]
        }
        self._pin_type_by_input_parameter: "dict[str, Any]" = {
            each_input["parameter"]: each_input.get("pin_type") for each_input in signature["inputs"]
        }
        self._input_parameters = list(self._value_type_by_input_parameter)
        self._bundle_class_by_input_parameter: "dict[str, Optional[type]]" = {
            each_input["parameter"]: the_dataclass_a_signature_entry_writes_its_bundle_as(
                each_input, f"function node `{node_id}`'s input `{each_input['parameter']}`"
            )
            for each_input in signature["inputs"]
        }
        self._named_class_by_input_parameter: "dict[str, type]" = {
            each_input["parameter"]: the_class_a_named_pin_type_names(
                each_input["pin_type"]["named_pin_type"],
                f"function node `{node_id}`'s input `{each_input['parameter']}`",
            )
            for each_input in signature["inputs"]
            if each_input["value_type"] == NAMED_VALUE_TYPE
        }
        self._named_class_by_output_port: "dict[str, type]" = {
            each_output["port"]: the_class_a_named_pin_type_names(
                each_output["pin_type"]["named_pin_type"],
                f"function node `{node_id}`'s output `{each_output['port']}`",
            )
            for each_output in signature["outputs"]
            if each_output["value_type"] == NAMED_VALUE_TYPE
        }
        self._positional_only_parameters = [
            parameter.name
            for parameter in inspect.signature(run_as_python.body).parameters.values()
            if parameter.kind is inspect.Parameter.POSITIONAL_ONLY
        ]

    def take_the_config(self, config: Any) -> None:
        """Refuse a `config` that sets anything, as a Python function node takes each value as an
        input."""
        refuse_a_config_given_a_function_node_taking_none(self._node_id, config)

    def runs(self, run_as_python: FunctionNodeReadyToEvaluate) -> bool:
        """Whether this is the body of `run_as_python`, so an island fed again can call it with its state."""
        return self._run_as_python is run_as_python

    def evaluate(self, input_values: "list[Any]", now_ns: int) -> "list[Any]":
        """Call the body with `input_values` in port order at `now_ns`, answering its outputs in port order.

        A number or bool output is answered as a `float`, a Color, Vector, Rectangle or
        Transform as the `tuple` of its parts, a String, Menu, Asset, List or bundle as the record
        `{"value": <stored JSON form>}` it travels as, a value of a named type as the `bytes`
        its class packs, a `dict` as given.
        The new state is kept only once every output is answered.
        """
        signature = self._run_as_python.function_signature
        arguments: "dict[str, Any]" = {}
        for parameter_name, input_value in zip(self._input_parameters, input_values):
            arguments[parameter_name] = self._the_input_the_body_takes(
                parameter_name, self._value_type_by_input_parameter[parameter_name], input_value
            )
        if signature["time_parameter"] is not None:
            arguments[signature["time_parameter"]] = now_ns
        state_handed_to_the_body = None
        if signature["state_parameter"] is not None:
            state_handed_to_the_body = copy.deepcopy(self._state)
            arguments[signature["state_parameter"]] = state_handed_to_the_body
        positional_arguments = [
            arguments.pop(parameter_name) for parameter_name in self._positional_only_parameters
        ]
        returned = self._run_as_python.body(*positional_arguments, **arguments)
        if signature["state_parameter"] is not None:
            output, new_state = returned
        else:
            output, new_state = returned, self._state
        outputs = signature["outputs"]
        output_values = [
            output if each_output["field"] is None else getattr(output, each_output["field"])
            for each_output in outputs
        ]
        values_handed_to_be_handed_on_unread = [
            input_value
            for parameter_name, input_value in zip(self._input_parameters, input_values)
            if self._value_type_by_input_parameter[parameter_name] == UNREAD_MESSAGE_VALUE_TYPE
        ] + _the_field_values_of(state_handed_to_the_body)
        answered = [
            self._the_output_the_evaluator_takes(
                each_output, output_value, values_handed_to_be_handed_on_unread
            )
            for each_output, output_value in zip(outputs, output_values)
        ]
        self._the_state_values_of(new_state)
        self._state = new_state
        return answered

    def state_values(self) -> "list[float]":
        """The node's state as the f64s its stream carries, one per field of a state record of numbers and bools."""
        return self._the_state_values_of(self._state)

    def _the_state_values_of(self, state: Any) -> "list[float]":
        return [
            _exactly_as_f64(getattr(state, field), f"function node `{self._node_id}`'s state `{field}`")
            for field in self._run_as_python.state_value_types
        ]

    def resume_from_state_values(self, state_values: "list[float]") -> None:
        """Resume from the state its stream carries, as `state_values` gives it."""
        state_record_class = self._run_as_python.state_record_class
        assert state_record_class is not None
        self._state = state_record_class(
            **{
                field: _python_value_of_an_f64(state_value, value_type)
                for (field, value_type), state_value in zip(
                    self._run_as_python.state_value_types.items(), state_values
                )
            }
        )

    def _the_input_the_body_takes(self, parameter_name: str, value_type: str, input_value: Any) -> Any:
        if value_type == UNREAD_MESSAGE_VALUE_TYPE:
            return input_value
        if value_type == NAMED_VALUE_TYPE:
            if input_value is None:
                return None
            if not isinstance(input_value, bytes):
                raise TypeError(
                    f"function node `{self._node_id}`'s input `{parameter_name}` takes the bytes "
                    f"of a named type, and was handed {type(input_value).__name__}"
                )
            return self._named_class_by_input_parameter[parameter_name].from_bytes(input_value)
        if value_type in PART_VALUE_TYPES:
            return value_class_of_a_part_value_type(value_type).from_parts(
                parts_a_value_type_reads(value_type, input_value)
            )
        if value_type in STORED_VALUE_TYPES:
            what_it_is = f"function node `{self._node_id}`'s input `{parameter_name}`"
            if not isinstance(input_value, dict) or STORED_VALUE_RECORD_KEY not in input_value:
                raise TypeError(
                    f"{what_it_is} takes a {value_type} value, and was handed {input_value!r}"
                )
            return the_value_a_body_takes(
                self._pin_type_by_input_parameter[parameter_name],
                input_value[STORED_VALUE_RECORD_KEY],
                what_it_is,
                self._bundle_class_by_input_parameter[parameter_name],
            )
        return _python_value_of_an_f64(float(input_value), value_type)

    def _the_output_the_evaluator_takes(
        self,
        each_output: "dict[str, Any]",
        output_value: Any,
        values_handed_to_be_handed_on_unread: "list[Any]",
    ) -> Any:
        what_it_is = f"function node `{self._node_id}`'s output `{each_output['port']}`"
        if output_value is None and each_output.get("may_hand_out_nothing"):
            return None
        if each_output["value_type"] == UNREAD_MESSAGE_VALUE_TYPE:
            if output_value is not None and any(
                output_value is handed for handed in values_handed_to_be_handed_on_unread
            ):
                return output_value
            if output_value is None or isinstance(output_value, (bool, int, float, dict)):
                raise TypeError(
                    f"{what_it_is} hands on a message it was handed, and the body gave "
                    f"{type(output_value).__name__}"
                )
            return output_value
        if each_output["value_type"] == NAMED_VALUE_TYPE:
            named_class = self._named_class_by_output_port[each_output["port"]]
            if output_value is None or type(output_value) is not named_class:
                raise TypeError(
                    f"{what_it_is} is the named type `{each_output['pin_type']['named_pin_type']}`, "
                    f"and the body gave {type(output_value).__name__}"
                )
            return bytes(output_value.to_bytes())
        if each_output["value_type"] in PART_VALUE_TYPES:
            value_class = value_class_of_a_part_value_type(each_output["value_type"])
            if output_value is None or type(output_value) is not value_class:
                raise TypeError(
                    f"{what_it_is} is a `{value_class.__name__}`, and the body gave "
                    f"{type(output_value).__name__}"
                )
            return output_value.parts
        if each_output["value_type"] in STORED_VALUE_TYPES:
            return {
                STORED_VALUE_RECORD_KEY: the_stored_value_of(
                    each_output["pin_type"], output_value, what_it_is
                )
            }
        if not isinstance(output_value, (bool, int, float)):
            raise TypeError(
                f"{what_it_is} is {each_output['value_type']}, and the body gave "
                f"{type(output_value).__name__}"
            )
        return _exactly_as_f64(output_value, what_it_is)


def _the_field_values_of(state_record: Any) -> "list[Any]":
    if state_record is None or not dataclasses.is_dataclass(state_record):
        return []
    return [getattr(state_record, field.name) for field in dataclasses.fields(state_record)]


def the_value_a_config_field_takes(value_it_holds: Any, input_value: float) -> Any:
    """`input_value` as the type of the value the config field holds: a bool, an int when it is whole, else a float."""
    if isinstance(value_it_holds, bool):
        return input_value != 0.0
    if isinstance(value_it_holds, int) and input_value.is_integer():
        return int(input_value)
    return input_value


def refuse_a_config_given_a_function_node_taking_none(node_id: str, config: Any) -> None:
    """Refuse a `config` that sets anything on a function node declaring no config field, a
    Python one among them, which takes each value as an input of its own."""
    config_values_by_field = dict(config) if isinstance(config, dict) else {}
    if config_values_by_field:
        raise FunctionNodeDoesNotCompile(
            f"function node `{node_id}`'s config sets "
            f"{', '.join(sorted(config_values_by_field))}, and it takes no config: each value "
            f"is an input of its own, set by `input_values`"
        )


def _python_value_of_an_f64(value: float, value_type: str) -> Any:
    if value_type == "int":
        return int(value)
    if value_type == "bool":
        return value != 0.0
    return value

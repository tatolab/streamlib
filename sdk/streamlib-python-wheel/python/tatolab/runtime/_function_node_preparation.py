# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Prepare a function node to evaluate, compiled or run as Python, and how one is called.

A body that does not compile, or that takes or gives a `dict`, runs as Python in
its reader's interpreter, called at the read with the interpreter's lock held.
"""

from __future__ import annotations

import dataclasses
import typing
from typing import Any, Callable, Optional

from tatolab.stream import log
from tatolab.stream._function_node_declaration import (
    NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP,
    FunctionNode,
)
from tatolab.stream._node_declaration import NODE_DECLARATION_CONFIG_CLASS_STAMP
from tatolab.stream._rust_function_node_declaration import (
    RustFunctionNode,
)

from tatolab.runtime._function_node_carried_values import (
    _exactly_as_f64,
    _how_the_values_of_a_signature_are_carried,
    _initial_state_values,
    _pin_defaults_by_input_port,
    _SCALAR_VALUE_TYPES_AN_EVALUATE_ENTRY_CARRIES,
    _VALUE_TYPES_A_BODY_RUN_AS_PYTHON_CARRIES,
    FunctionNodeCompiledToItsEvaluateEntry,
    FunctionNodeDoesNotCompile,
    FunctionNodeReadyToEvaluate,
    FunctionNodeRunAsPythonInItsReader,
    UNREAD_MESSAGE_VALUE_TYPE,
)
from tatolab.runtime._function_node_evaluate_entry_compiler import (
    _the_evaluate_entry_a_rust_function_node_exports,
    compile_function_node_to_its_evaluate_entry,
)
from tatolab.runtime._function_node_body_run_as_python import (
    FunctionNodeBodyRunAsPython,
)


def how_one_function_node_is_called(
    prepared: FunctionNodeReadyToEvaluate,
    node_id: str,
    config: Any,
    linked_exposed_config_fields: "tuple[str, ...]" = (),
) -> "tuple[Any, list[float], list[int]]":
    """What the native evaluator calls `node_id` through, the config values it hands the call,
    and the index among them each input past the node's own sets.

    A compiled node is called through its entry's address with its config as
    f64s; a node run as Python through an object holding its config and state.
    Each of `linked_exposed_config_fields` is an input past the node's own, in
    that order, setting the config field of its name at each evaluation.
    """
    if isinstance(prepared, FunctionNodeCompiledToItsEvaluateEntry):
        return (
            prepared.evaluate_entry_address,
            config_values_of_one_function_node(prepared, node_id, config),
            [
                _the_config_value_index_of(prepared, node_id, field)
                for field in linked_exposed_config_fields
            ],
        )
    body_run_as_python = FunctionNodeBodyRunAsPython(prepared, node_id, config)
    body_run_as_python.config_fields_set_by_the_inputs_past_its_own = linked_exposed_config_fields
    return body_run_as_python, [], []


def _the_config_value_index_of(
    compiled: FunctionNodeCompiledToItsEvaluateEntry, node_id: str, field: str
) -> int:
    if field not in compiled.config_fields:
        raise FunctionNodeDoesNotCompile(
            f"function node `{node_id}` exposes `{field}` as an input, which its config class "
            f"does not declare; its fields are {', '.join(compiled.config_fields) or 'none'}"
        )
    return compiled.config_fields.index(field)


def prepare_function_node_to_evaluate(function_node: Any, import_path: str) -> FunctionNodeReadyToEvaluate:
    """`function_node` compiled to its evaluate entry, or ready to run as Python when its body does not compile.

    What neither path carries is refused by name.
    """
    if isinstance(function_node, RustFunctionNode):
        return _the_evaluate_entry_a_rust_function_node_exports(function_node, import_path)
    run_as_python = function_node_run_as_python_in_its_reader(function_node, import_path, "")
    try:
        return compile_function_node_to_its_evaluate_entry(function_node, import_path)
    except FunctionNodeDoesNotCompile as does_not_compile:
        log.info(
            f"{import_path} runs as Python in its reader's interpreter, since it does not "
            f"compile: {does_not_compile}"
        )
        return dataclasses.replace(run_as_python, why_it_does_not_compile=str(does_not_compile))


def function_node_run_as_python_in_its_reader(
    function_node: Any, import_path: str, why_it_does_not_compile: str
) -> FunctionNodeRunAsPythonInItsReader:
    """`function_node` ready to run as Python, refusing by name a signature a body run as Python cannot carry."""
    if not isinstance(function_node, FunctionNode):
        raise FunctionNodeDoesNotCompile(
            f"{import_path} names {function_node!r}, which is not a `@node.function` function"
        )
    function_signature: "dict[str, Any]" = getattr(
        function_node, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP
    )
    body: Callable[..., Any] = function_node.__wrapped__
    annotations = typing.get_type_hints(body)
    for each_port in (*function_signature["inputs"], *function_signature["outputs"]):
        if each_port["value_type"] not in _VALUE_TYPES_A_BODY_RUN_AS_PYTHON_CARRIES:
            raise FunctionNodeDoesNotCompile(
                f"{import_path}'s port `{each_port['port']}` is {each_port['value_type']}, and a "
                f"function node carries numbers, bools, Colors, Vectors, Rectangles, Transforms, "
                f"Strings, Menus, Assets, Lists and dicts in this release. To fix: make it one "
                f"of them."
            )
    for each_input in function_signature["inputs"]:
        if each_input["optional"] and each_input["value_type"] != UNREAD_MESSAGE_VALUE_TYPE:
            raise FunctionNodeDoesNotCompile(
                f"{import_path}'s input `{each_input['parameter']}` is optional, and a function "
                f"node takes a value on every input but an `UnreadMessage` in this release. To "
                f"fix: give it a default."
            )
    state_parameter: Optional[str] = function_signature["state_parameter"]
    state_record_class: Optional[type] = (
        None if state_parameter is None else typing.get_args(annotations[state_parameter])[0]
    )
    state_value_types = _scalar_field_value_types_or_none(state_record_class) or {}
    return FunctionNodeRunAsPythonInItsReader(
        body=body,
        function_signature=function_signature,
        input_ports=tuple(each_input["port"] for each_input in function_signature["inputs"]),
        pin_defaults_by_input_port=_pin_defaults_by_input_port(import_path, function_signature),
        output_ports=tuple(each_output["port"] for each_output in function_signature["outputs"]),
        ports_carrying_records=frozenset(
            each_port["port"]
            for each_port in (*function_signature["inputs"], *function_signature["outputs"])
            if each_port["value_type"] == "records"
        ),
        config_class=getattr(function_node, NODE_DECLARATION_CONFIG_CLASS_STAMP, None),
        state_record_class=state_record_class,
        state_value_types=state_value_types,
        initial_state_values=(
            _initial_state_values(import_path, state_record_class, state_value_types)
            if state_value_types
            else ()
        ),
        is_stateful=state_record_class is not None,
        why_it_does_not_compile=why_it_does_not_compile,
        input_value_types=tuple(each_input["value_type"] for each_input in function_signature["inputs"]),
        how_its_values_are_carried=_how_the_values_of_a_signature_are_carried(function_signature),
        input_pin_types=tuple(each_input.get("pin_type") for each_input in function_signature["inputs"]),
        output_pin_types=tuple(
            each_output.get("pin_type") for each_output in function_signature["outputs"]
        ),
    )


def _scalar_field_value_types_or_none(record_class: Optional[type]) -> "Optional[dict[str, str]]":
    """Each field's value type when every field of `record_class` is a number or a bool, else `None`."""
    if record_class is None:
        return None
    field_annotations = typing.get_type_hints(record_class)
    value_types: "dict[str, str]" = {}
    for field in dataclasses.fields(record_class):
        value_type = _SCALAR_VALUE_TYPES_AN_EVALUATE_ENTRY_CARRIES.get(field_annotations[field.name])
        if value_type is None:
            return None
        value_types[field.name] = value_type
    return value_types


def config_values_of_one_function_node(
    compiled: FunctionNodeCompiledToItsEvaluateEntry, node_id: str, config: Any
) -> "list[float]":
    """`node_id`'s config, as the f64s its evaluate entry takes, each field in its declared order."""
    config_values_by_field = dict(config) if isinstance(config, dict) else {}
    fields_it_does_not_declare = sorted(set(config_values_by_field) - set(compiled.config_fields))
    if fields_it_does_not_declare:
        raise FunctionNodeDoesNotCompile(
            f"function node `{node_id}`'s config sets {', '.join(fields_it_does_not_declare)}, "
            f"which its config class does not declare; its fields are "
            f"{', '.join(compiled.config_fields) or 'none'}"
        )
    config_values: "list[float]" = []
    for field in compiled.config_fields:
        what_it_is = f"function node `{node_id}`'s config `{field}`"
        if field in config_values_by_field:
            config_values.append(_exactly_as_f64(config_values_by_field[field], what_it_is))
        elif field in compiled.config_field_defaults:
            config_values.append(compiled.config_field_defaults[field])
        elif field in compiled.config_field_default_factories:
            config_values.append(
                _exactly_as_f64(compiled.config_field_default_factories[field](), what_it_is)
            )
        else:
            raise FunctionNodeDoesNotCompile(
                f"function node `{node_id}`'s config sets no `{field}`, and the field declares "
                f"no default"
            )
    return config_values

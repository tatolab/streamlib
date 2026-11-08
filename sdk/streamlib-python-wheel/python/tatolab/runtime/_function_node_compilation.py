# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Compile `@node.function` bodies to their evaluate entry and feed each island to its port.

A body is compiled by Numba in nopython mode. Its dataclasses (its state
record, its config, a several-output return) are lowered as named tuples of the
same fields, so the body constructs and reads them unchanged. A wrapper with the
evaluate entry's C signature unpacks the f64 values the island hands it into the
body's parameters and packs its outputs and new state back. The body is cached
on disk by Numba; the wrapper is compiled at each load.
"""

from __future__ import annotations

import collections
import dataclasses
import inspect
import types
import typing
from typing import Any, Callable, Optional

from tatolab.stream import NodeLinkDataAccess
from tatolab.stream._function_node_declaration import (
    NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP,
    FunctionNode,
)
from tatolab.stream._node_declaration import NODE_DECLARATION_CONFIG_CLASS_STAMP

from tatolab.runtime._engine import feed_an_input_port_from_a_function_node_island

# The scalar value types an evaluate entry carries, each as an f64.
_SCALAR_VALUE_TYPES_AN_EVALUATE_ENTRY_CARRIES = {float: "float", int: "int", bool: "bool"}


class FunctionNodeDoesNotCompile(Exception):
    """A function node whose body or signature the evaluate entry cannot carry."""


@dataclasses.dataclass(frozen=True)
class FunctionNodeCompiledToItsEvaluateEntry:
    """A function node type compiled to its evaluate entry, and the order its values travel in."""

    evaluate_entry_address: int
    input_ports: "tuple[str, ...]"
    pin_defaults_by_input_port: "dict[str, float]"
    output_ports: "tuple[str, ...]"
    config_fields: "tuple[str, ...]"
    config_field_defaults: "dict[str, float]"
    initial_state_values: "tuple[float, ...]"
    compiled_wrapper_kept_alive: Any


def feed_each_input_port_from_its_function_node_island(
    link_data_access: NodeLinkDataAccess,
    islands: "list[dict[str, Any]]",
    load_function_node: Callable[[str], Any],
) -> "list[FunctionNodeCompiledToItsEvaluateEntry]":
    """Compile every function node the setup command's `islands` carry and feed each to its port.

    Answers what was compiled, which the caller keeps for as long as the ports
    are read: the native evaluator calls into it.
    """
    compiled_by_import_path: "dict[str, FunctionNodeCompiledToItsEvaluateEntry]" = {}
    for island in islands:
        function_nodes_handed_over: "list[Any]" = []
        node_index_by_node_id: "dict[str, int]" = {}
        output_ports_by_node_id: "dict[str, tuple[str, ...]]" = {}
        for island_node in island["nodes"]:
            node_id = island_node["node"]
            import_path = island_node["import_path"]
            if import_path not in compiled_by_import_path:
                compiled_by_import_path[import_path] = compile_function_node_to_its_evaluate_entry(
                    load_function_node(import_path), import_path
                )
            compiled = compiled_by_import_path[import_path]
            function_nodes_handed_over.append(
                (
                    node_id,
                    compiled.evaluate_entry_address,
                    _where_each_input_takes_its_value(
                        compiled,
                        node_id,
                        island_node["links_into"],
                        node_index_by_node_id,
                        output_ports_by_node_id,
                    ),
                    _config_values(compiled, node_id, island_node.get("config")),
                    list(compiled.initial_state_values),
                    len(compiled.output_ports),
                )
            )
            node_index_by_node_id[node_id] = len(node_index_by_node_id)
            output_ports_by_node_id[node_id] = compiled.output_ports
        island_output = island["output"]
        feed_an_input_port_from_a_function_node_island(
            link_data_access=link_data_access,
            input_port=island["input_port"],
            function_nodes=function_nodes_handed_over,
            island_output=(
                node_index_by_node_id[island_output["node"]],
                _output_index(output_ports_by_node_id, island_output["node"], island_output["port"]),
            ),
        )
    return list(compiled_by_import_path.values())


def _where_each_input_takes_its_value(
    compiled: FunctionNodeCompiledToItsEvaluateEntry,
    node_id: str,
    links_into: "list[dict[str, Any]]",
    node_index_by_node_id: "dict[str, int]",
    output_ports_by_node_id: "dict[str, tuple[str, ...]]",
) -> "list[tuple[Optional[tuple[int, int]], float]]":
    linked_from_by_input_port = {link["port"]: link["from"] for link in links_into}
    where_each_input_takes_its_value: "list[tuple[Optional[tuple[int, int]], float]]" = []
    for input_port in compiled.input_ports:
        linked_from = linked_from_by_input_port.get(input_port)
        if linked_from is not None:
            source_node_id = linked_from["node"]
            where_each_input_takes_its_value.append(
                (
                    (
                        node_index_by_node_id[source_node_id],
                        _output_index(output_ports_by_node_id, source_node_id, linked_from["port"]),
                    ),
                    0.0,
                )
            )
        elif input_port in compiled.pin_defaults_by_input_port:
            where_each_input_takes_its_value.append(
                (None, compiled.pin_defaults_by_input_port[input_port])
            )
        else:
            raise FunctionNodeDoesNotCompile(
                f"function node `{node_id}`'s input `{input_port}` is linked to nothing and "
                f"declares no default, so it has no value to evaluate with. To fix: link it, or "
                f"give its parameter a default."
            )
    return where_each_input_takes_its_value


def _output_index(
    output_ports_by_node_id: "dict[str, tuple[str, ...]]", node_id: str, output_port: str
) -> int:
    output_ports = output_ports_by_node_id[node_id]
    if output_port not in output_ports:
        raise FunctionNodeDoesNotCompile(
            f"function node `{node_id}` has no output `{output_port}`; its outputs are "
            f"{', '.join(output_ports)}"
        )
    return output_ports.index(output_port)


def _config_values(
    compiled: FunctionNodeCompiledToItsEvaluateEntry, node_id: str, config: Any
) -> "list[float]":
    config_values_by_field = dict(config) if isinstance(config, dict) else {}
    config_values: "list[float]" = []
    for field in compiled.config_fields:
        if field in config_values_by_field:
            config_values.append(float(config_values_by_field[field]))
        elif field in compiled.config_field_defaults:
            config_values.append(compiled.config_field_defaults[field])
        else:
            raise FunctionNodeDoesNotCompile(
                f"function node `{node_id}`'s config sets no `{field}`, and the field declares "
                f"no default"
            )
    return config_values


def compile_function_node_to_its_evaluate_entry(
    function_node: Any, import_path: str
) -> FunctionNodeCompiledToItsEvaluateEntry:
    """Compile `function_node`'s body with Numba to its evaluate entry, refusing by name what does not compile."""
    if not isinstance(function_node, FunctionNode):
        raise FunctionNodeDoesNotCompile(
            f"{import_path} names {function_node!r}, which is not a `@node.function` function"
        )
    function_signature: "dict[str, Any]" = getattr(
        function_node, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP
    )
    body: Callable[..., Any] = function_node.__wrapped__
    annotations = typing.get_type_hints(body)
    for each_input in function_signature["inputs"]:
        _refuse_a_value_the_evaluate_entry_cannot_carry(
            import_path, f"input `{each_input['parameter']}`", each_input["value_type"]
        )
        if each_input["optional"]:
            raise FunctionNodeDoesNotCompile(
                f"{import_path}'s input `{each_input['parameter']}` is optional, and a compiled "
                f"function node takes a number on every input. To fix: give it a numeric default."
            )
    for each_output in function_signature["outputs"]:
        _refuse_a_value_the_evaluate_entry_cannot_carry(
            import_path, f"output `{each_output['port']}`", each_output["value_type"]
        )

    config_class: Optional[type] = getattr(function_node, NODE_DECLARATION_CONFIG_CLASS_STAMP, None)
    state_parameter: Optional[str] = function_signature["state_parameter"]
    state_record_class: Optional[type] = (
        None if state_parameter is None else typing.get_args(annotations[state_parameter])[0]
    )
    output_record_class = _output_record_class(annotations["return"], state_record_class)
    config_value_types = _scalar_field_value_types(import_path, "config", config_class)
    state_value_types = _scalar_field_value_types(import_path, "state", state_record_class)

    named_tuple_by_dataclass = {
        dataclass_lowered: collections.namedtuple(  # pyright: ignore[reportUntypedNamedTuple]
            dataclass_lowered.__name__,
            [field.name for field in dataclasses.fields(dataclass_lowered)],
        )
        for dataclass_lowered in (config_class, state_record_class, output_record_class)
        if dataclass_lowered is not None
    }
    try:
        import numba
        from numba.core.errors import NumbaError
        from numba.np.numpy_support import carray
    except ImportError as numba_missing:
        raise FunctionNodeDoesNotCompile(
            f"{import_path} is a function node, and Numba, which compiles it, is not installed "
            f"in this stream's environment. To fix: install `numba`, or `tatolab-ops`, which "
            f"depends on it."
        ) from numba_missing

    body_with_its_dataclasses_lowered = numba.njit(cache=True, error_model="numpy")(
        _the_body_with_its_dataclasses_lowered(body, named_tuple_by_dataclass)
    )
    wrapper_source, wrapper_globals = _evaluate_entry_wrapper_source(
        body,
        function_signature,
        config_class,
        config_value_types,
        state_record_class,
        state_value_types,
        output_record_class,
        named_tuple_by_dataclass,
    )
    wrapper_globals["carray"] = carray
    wrapper_globals["body_with_its_dataclasses_lowered"] = body_with_its_dataclasses_lowered
    exec(compile(wrapper_source, f"<evaluate entry of {import_path}>", "exec"), wrapper_globals)
    float64_pointer = numba.types.CPointer(numba.types.float64)
    evaluate_entry_signature = numba.types.int32(
        float64_pointer, float64_pointer, float64_pointer, float64_pointer, numba.types.int64
    )
    try:
        compiled_wrapper = numba.cfunc(evaluate_entry_signature, error_model="numpy")(
            wrapper_globals["evaluate"]
        )
    except NumbaError as does_not_compile:
        raise FunctionNodeDoesNotCompile(
            f"{import_path}'s body does not compile to native code, and a function node that "
            f"does not compile is not run in this release. Numba said: {does_not_compile}"
        ) from does_not_compile

    evaluate_entry_address = compiled_wrapper.address
    if evaluate_entry_address is None:
        raise FunctionNodeDoesNotCompile(f"{import_path} compiled to no evaluate entry address")

    pin_defaults_by_input_port = {
        each_input["port"]: float(each_input["default"])
        for each_input in function_signature["inputs"]
        if each_input.get("default") is not None
    }
    config_field_defaults = {
        field.name: float(field.default)
        for field in (dataclasses.fields(config_class) if config_class is not None else ())
        if field.default is not dataclasses.MISSING
    }
    initial_state_values = (
        tuple(float(getattr(state_record_class(), name)) for name in state_value_types)
        if state_record_class is not None
        else ()
    )
    return FunctionNodeCompiledToItsEvaluateEntry(
        evaluate_entry_address=evaluate_entry_address,
        input_ports=tuple(each_input["port"] for each_input in function_signature["inputs"]),
        pin_defaults_by_input_port=pin_defaults_by_input_port,
        output_ports=tuple(each_output["port"] for each_output in function_signature["outputs"]),
        config_fields=tuple(config_value_types),
        config_field_defaults=config_field_defaults,
        initial_state_values=initial_state_values,
        compiled_wrapper_kept_alive=compiled_wrapper,
    )


def _refuse_a_value_the_evaluate_entry_cannot_carry(
    import_path: str, what_it_types: str, value_type: str
) -> None:
    if value_type not in _SCALAR_VALUE_TYPES_AN_EVALUATE_ENTRY_CARRIES.values():
        raise FunctionNodeDoesNotCompile(
            f"{import_path}'s {what_it_types} is {value_type}, and a compiled function node "
            f"carries numbers and bools in this release. To fix: make it `float`, `int` or `bool`."
        )


def _output_record_class(return_annotation: Any, state_record_class: Optional[type]) -> Optional[type]:
    output_annotation = (
        typing.get_args(return_annotation)[0] if state_record_class is not None else return_annotation
    )
    if isinstance(output_annotation, type) and dataclasses.is_dataclass(output_annotation):
        return output_annotation
    return None


def _scalar_field_value_types(
    import_path: str, what_the_record_is: str, record_class: Optional[type]
) -> "dict[str, str]":
    if record_class is None:
        return {}
    field_annotations = typing.get_type_hints(record_class)
    value_types: "dict[str, str]" = {}
    for field in dataclasses.fields(record_class):
        value_type = _SCALAR_VALUE_TYPES_AN_EVALUATE_ENTRY_CARRIES.get(field_annotations[field.name])
        if value_type is None:
            raise FunctionNodeDoesNotCompile(
                f"{import_path}'s {what_the_record_is} field `{field.name}` is "
                f"{field_annotations[field.name]!r}, and a compiled function node carries "
                f"numbers and bools in this release. To fix: make it `float`, `int` or `bool`."
            )
        value_types[field.name] = value_type
    return value_types


def _the_body_with_its_dataclasses_lowered(
    body: Callable[..., Any], named_tuple_by_dataclass: "dict[type, type]"
) -> Callable[..., Any]:
    """`body` reading the named tuple in place of each dataclass its globals name,
    with its keyword-only parameters made positional."""
    lowered_globals = dict(body.__globals__)
    for global_name, global_value in body.__globals__.items():
        if isinstance(global_value, type) and global_value in named_tuple_by_dataclass:
            lowered_globals[global_name] = named_tuple_by_dataclass[global_value]
    code = body.__code__
    code_taking_every_parameter_positionally = code.replace(
        co_argcount=code.co_argcount + code.co_kwonlyargcount, co_kwonlyargcount=0
    )
    lowered = types.FunctionType(
        code_taking_every_parameter_positionally, lowered_globals, body.__name__
    )
    lowered.__module__ = body.__module__
    lowered.__qualname__ = body.__qualname__
    return lowered


def _evaluate_entry_wrapper_source(
    body: Callable[..., Any],
    function_signature: "dict[str, Any]",
    config_class: Optional[type],
    config_value_types: "dict[str, str]",
    state_record_class: Optional[type],
    state_value_types: "dict[str, str]",
    output_record_class: Optional[type],
    named_tuple_by_dataclass: "dict[type, type]",
) -> "tuple[str, dict[str, Any]]":
    wrapper_globals: "dict[str, Any]" = {}
    value_type_by_input_parameter = {
        each_input["parameter"]: each_input["value_type"] for each_input in function_signature["inputs"]
    }
    input_parameters = list(value_type_by_input_parameter)
    lines = ["def evaluate(input_values, config_values, state_values, output_values, now_ns):"]
    if input_parameters:
        lines.append(f"    inputs = carray(input_values, ({len(input_parameters)},))")
    if config_value_types:
        lines.append(f"    config = carray(config_values, ({len(config_value_types)},))")
    if state_value_types:
        lines.append(f"    state = carray(state_values, ({len(state_value_types)},))")
    output_ports = [each_output["port"] for each_output in function_signature["outputs"]]
    lines.append(f"    outputs = carray(output_values, ({len(output_ports)},))")

    arguments: "list[str]" = []
    for parameter_name in inspect.signature(body).parameters:
        if parameter_name == function_signature["time_parameter"]:
            arguments.append("now_ns")
        elif parameter_name == function_signature["state_parameter"]:
            assert state_record_class is not None
            wrapper_globals["StateRecord"] = named_tuple_by_dataclass[state_record_class]
            arguments.append(
                "StateRecord("
                + ", ".join(
                    _from_f64(f"state[{index}]", value_type)
                    for index, value_type in enumerate(state_value_types.values())
                )
                + ")"
            )
        elif parameter_name == function_signature["config_parameter"]:
            assert config_class is not None
            wrapper_globals["ConfigRecord"] = named_tuple_by_dataclass[config_class]
            arguments.append(
                "ConfigRecord("
                + ", ".join(
                    _from_f64(f"config[{index}]", value_type)
                    for index, value_type in enumerate(config_value_types.values())
                )
                + ")"
            )
        else:
            arguments.append(
                _from_f64(
                    f"inputs[{input_parameters.index(parameter_name)}]",
                    value_type_by_input_parameter[parameter_name],
                )
            )
    lines.append(f"    returned = body_with_its_dataclasses_lowered({', '.join(arguments)})")
    if state_record_class is not None:
        lines.append("    output, new_state = returned")
        for index, (field_name, value_type) in enumerate(state_value_types.items()):
            lines.append(f"    state[{index}] = {_to_f64(f'new_state.{field_name}', value_type)}")
    else:
        lines.append("    output = returned")
    if output_record_class is not None:
        for index, each_output in enumerate(function_signature["outputs"]):
            output_value = _to_f64(f"output.{each_output['field']}", each_output["value_type"])
            lines.append(f"    outputs[{index}] = {output_value}")
    else:
        output_value = _to_f64("output", function_signature["outputs"][0]["value_type"])
        lines.append(f"    outputs[0] = {output_value}")
    lines.append("    return 0")
    return "\n".join(lines) + "\n", wrapper_globals


def _to_f64(value_expression: str, value_type: str) -> str:
    if value_type == "bool":
        return f"(1.0 if {value_expression} else 0.0)"
    return f"float({value_expression})"


def _from_f64(value_expression: str, value_type: str) -> str:
    if value_type == "int":
        return f"int({value_expression})"
    if value_type == "bool":
        return f"({value_expression} != 0.0)"
    return value_expression

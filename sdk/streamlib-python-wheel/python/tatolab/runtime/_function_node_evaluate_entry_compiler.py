# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Compile a function node to its evaluate entry, a Numba body behind a C-signature wrapper.

A body is compiled by Numba in nopython mode. Its dataclasses (its state
record, its config, a several-output return) are lowered as named tuples of the
same fields, so the body constructs and reads them unchanged. A wrapper with the
evaluate entry's C signature unpacks the f64 values the island hands it into the
body's parameters and packs its outputs and new state back. The body is cached
on disk by Numba; the wrapper is compiled at each load.

An `UnreadMessage` reaches a compiled body as a token, the index of the input
that holds it plus one, and an output the body may hand out nothing on carries,
past every output, a slot saying whether it handed one out.
"""

from __future__ import annotations

import collections
import dataclasses
import inspect
import types
import typing
from typing import Any, Callable, Optional

from tatolab.stream._function_node_declaration import (
    NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP,
    FunctionNode,
)
from tatolab.stream._node_declaration import NODE_DECLARATION_CONFIG_CLASS_STAMP
from tatolab.stream._rust_function_node_declaration import (
    RustFunctionNode,
    RustFunctionNodeDeclaration,
)

from tatolab.runtime._engine import (
    MODULE_HOST_TABLE_ABI_MAJOR,
    MODULE_HOST_TABLE_ABI_MINOR,
)
from tatolab.runtime._function_node_carried_values import (
    _exactly_as_f64,
    _how_the_values_of_a_signature_are_carried,
    _initial_state_values,
    _pin_defaults_by_input_port,
    _SCALAR_VALUE_TYPES_AN_EVALUATE_ENTRY_CARRIES,
    FunctionNodeCompiledToItsEvaluateEntry,
    FunctionNodeDoesNotCompile,
    UNREAD_MESSAGE_VALUE_TYPE,
)

# The status the evaluate entry answers when the body raises; `MODULE_HOST_TABLE_STATUS_REFUSED`.
_EVALUATE_ENTRY_STATUS_REFUSED = -1


def compile_function_node_to_its_evaluate_entry(
    function_node: Any, import_path: str
) -> FunctionNodeCompiledToItsEvaluateEntry:
    """Compile `function_node`'s body with Numba to its evaluate entry, refusing by name what does not compile.

    A Rust function node exports its entry already; it is read, never compiled.
    """
    if isinstance(function_node, RustFunctionNode):
        return _the_evaluate_entry_a_rust_function_node_exports(function_node, import_path)
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
        if each_input["optional"] and each_input["value_type"] != UNREAD_MESSAGE_VALUE_TYPE:
            raise FunctionNodeDoesNotCompile(
                f"{import_path}'s input `{each_input['parameter']}` is optional, and a compiled "
                f"function node takes a number on every input but an `UnreadMessage`. To fix: "
                f"give it a numeric default."
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
        from numba.np.numpy_support import carray
    except ImportError as numba_missing:
        raise FunctionNodeDoesNotCompile(
            f"{import_path} is a function node, and Numba, which compiles function nodes, is "
            f"not installed in this stream's environment. To compile it: install `numba`."
        ) from numba_missing

    constructor_by_dataclass = {
        dataclass_lowered: numba.njit(
            _a_constructor_keeping_the_dataclass_defaults(import_path, dataclass_lowered, named_tuple)
        )
        for dataclass_lowered, named_tuple in named_tuple_by_dataclass.items()
        if dataclass_lowered is not config_class
    }
    body_with_its_dataclasses_lowered = numba.njit(cache=True, error_model="numpy")(
        _the_body_with_its_dataclasses_lowered(body, constructor_by_dataclass)
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
    except Exception as does_not_compile:
        # Numba refuses a body through `NumbaError` and through exceptions of
        # its own outside it, such as `UnsupportedBytecodeError`.
        raise FunctionNodeDoesNotCompile(
            f"{import_path}'s body does not compile to native code. Numba said: "
            f"{type(does_not_compile).__name__}: {does_not_compile}"
        ) from does_not_compile

    evaluate_entry_address = compiled_wrapper.address
    if evaluate_entry_address is None:
        raise FunctionNodeDoesNotCompile(f"{import_path} compiled to no evaluate entry address")

    pin_defaults_by_input_port = _pin_defaults_by_input_port(import_path, function_signature)
    config_fields = dataclasses.fields(config_class) if config_class is not None else ()
    config_field_defaults = {
        field.name: _exactly_as_f64(field.default, f"{import_path}'s config `{field.name}` default")
        for field in config_fields
        if field.default is not dataclasses.MISSING
    }
    config_field_default_factories: "dict[str, Callable[[], Any]]" = {
        field.name: field.default_factory
        for field in config_fields
        if field.default_factory is not dataclasses.MISSING
    }
    return FunctionNodeCompiledToItsEvaluateEntry(
        evaluate_entry_address=evaluate_entry_address,
        input_ports=tuple(each_input["port"] for each_input in function_signature["inputs"]),
        pin_defaults_by_input_port=pin_defaults_by_input_port,
        output_ports=tuple(each_output["port"] for each_output in function_signature["outputs"]),
        config_fields=tuple(config_value_types),
        config_field_defaults=config_field_defaults,
        config_field_default_factories=config_field_default_factories,
        state_fields=tuple(state_value_types),
        initial_state_values=_initial_state_values(import_path, state_record_class, state_value_types),
        is_stateful=state_record_class is not None,
        body_loaded_from_numbas_cache=sum(body_with_its_dataclasses_lowered.stats.cache_hits.values())
        > 0,
        compiled_wrapper_kept_alive=compiled_wrapper,
        input_value_types=tuple(each_input["value_type"] for each_input in function_signature["inputs"]),
        how_its_values_are_carried=_how_the_values_of_a_signature_are_carried(function_signature),
    )


def _the_evaluate_entry_a_rust_function_node_exports(
    function_node: RustFunctionNode, import_path: str
) -> FunctionNodeCompiledToItsEvaluateEntry:
    declaration = function_node.rust_function_node_declaration
    _refuse_a_rust_function_node_built_for_another_module_host_table(declaration, import_path)
    function_signature: "dict[str, Any]" = getattr(
        function_node, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP
    )
    return FunctionNodeCompiledToItsEvaluateEntry(
        evaluate_entry_address=declaration.evaluate_entry_address,
        input_ports=tuple(input_pin.port for input_pin in declaration.input_pins),
        pin_defaults_by_input_port={
            input_pin.port: input_pin.pin_default
            for input_pin in declaration.input_pins
            if input_pin.pin_default is not None
        },
        output_ports=tuple(output_port.name for output_port in declaration.output_ports),
        config_fields=tuple(config_field.name for config_field in declaration.config_fields),
        config_field_defaults={
            config_field.name: config_field.default for config_field in declaration.config_fields
        },
        config_field_default_factories={},
        state_fields=tuple(state_field.name for state_field in declaration.state_fields),
        initial_state_values=tuple(state_field.default for state_field in declaration.state_fields),
        is_stateful=declaration.state_parameter is not None,
        body_loaded_from_numbas_cache=False,
        compiled_wrapper_kept_alive=function_node,
        input_value_types=tuple(input_pin.value_type for input_pin in declaration.input_pins),
        how_its_values_are_carried=_how_the_values_of_a_signature_are_carried(function_signature),
    )


def _refuse_a_rust_function_node_built_for_another_module_host_table(
    declaration: RustFunctionNodeDeclaration, import_path: str
) -> None:
    built_for = (
        f"{declaration.module_host_table_abi_major}.{declaration.module_host_table_abi_minor}"
    )
    runtime_serves = f"{MODULE_HOST_TABLE_ABI_MAJOR}.{MODULE_HOST_TABLE_ABI_MINOR}"
    if declaration.module_host_table_abi_major != MODULE_HOST_TABLE_ABI_MAJOR:
        raise FunctionNodeDoesNotCompile(
            f"{import_path} is a Rust function node built against module host table {built_for}, "
            f"and this runtime serves {runtime_serves}, another major. To fix: rebuild its "
            f"module against a `tatolab-module-sdk` {MODULE_HOST_TABLE_ABI_MAJOR}.x."
        )
    if declaration.module_host_table_abi_minor > MODULE_HOST_TABLE_ABI_MINOR:
        raise FunctionNodeDoesNotCompile(
            f"{import_path} is a Rust function node built against module host table {built_for}, "
            f"newer than the {runtime_serves} this runtime serves. To fix: update the runtime, or "
            f"build its module against an older `tatolab-module-sdk`."
        )


def _refuse_a_value_the_evaluate_entry_cannot_carry(
    import_path: str, what_it_types: str, value_type: str
) -> None:
    if (
        value_type not in _SCALAR_VALUE_TYPES_AN_EVALUATE_ENTRY_CARRIES.values()
        and value_type != UNREAD_MESSAGE_VALUE_TYPE
    ):
        raise FunctionNodeDoesNotCompile(
            f"{import_path}'s {what_it_types} is {value_type}, and a compiled function node "
            f"carries numbers, bools and `UnreadMessage`s in this release. To fix: make it "
            f"`float`, `int` or `bool`."
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


def _a_constructor_keeping_the_dataclass_defaults(
    import_path: str, dataclass_lowered: type, named_tuple: type
) -> Callable[..., Any]:
    """A function building `named_tuple` from `dataclass_lowered`'s fields, defaulting
    each field its dataclass gives a plain default; Numba's named tuples take none."""
    constructor_globals: "dict[str, Any]" = {"named_tuple": named_tuple}
    parameters: "list[str]" = []
    for field in dataclasses.fields(dataclass_lowered):
        if field.default_factory is not dataclasses.MISSING:
            raise FunctionNodeDoesNotCompile(
                f"{import_path}'s body builds `{dataclass_lowered.__qualname__}`, whose field "
                f"`{field.name}` defaults through `default_factory`, and a compiled body cannot "
                f"call a Python factory. To fix: give `{field.name}` a plain default."
            )
        if field.default is dataclasses.MISSING:
            parameters.append(field.name)
        else:
            constructor_globals[f"default_{field.name}"] = field.default
            parameters.append(f"{field.name}=default_{field.name}")
    field_names = ", ".join(field.name for field in dataclasses.fields(dataclass_lowered))
    source = f"def construct({', '.join(parameters)}):\n    return named_tuple({field_names})\n"
    exec(
        compile(source, f"<constructor of {dataclass_lowered.__qualname__}>", "exec"),
        constructor_globals,
    )
    return constructor_globals["construct"]


def _the_body_with_its_dataclasses_lowered(
    body: Callable[..., Any], constructor_by_dataclass: "dict[type, Any]"
) -> Callable[..., Any]:
    """`body` building the named tuple in place of each dataclass its globals name,
    with its keyword-only parameters made positional."""
    lowered_globals = dict(body.__globals__)
    for global_name, global_value in body.__globals__.items():
        if isinstance(global_value, type) and global_value in constructor_by_dataclass:
            lowered_globals[global_name] = constructor_by_dataclass[global_value]
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
    optional_input_parameters = {
        each_input["parameter"] for each_input in function_signature["inputs"] if each_input["optional"]
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
    handed_out_slot_by_output_index = {
        output_index: len(output_ports) + handed_out_slot_index
        for handed_out_slot_index, output_index in enumerate(
            index
            for index, each_output in enumerate(function_signature["outputs"])
            if each_output.get("may_hand_out_nothing")
        )
    }
    lines.append(
        f"    outputs = carray(output_values, ({len(output_ports) + len(handed_out_slot_by_output_index)},))"
    )

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
            input_expression = f"inputs[{input_parameters.index(parameter_name)}]"
            if parameter_name in optional_input_parameters:
                arguments.append(f"(None if {input_expression} == 0.0 else {input_expression})")
            else:
                arguments.append(
                    _from_f64(input_expression, value_type_by_input_parameter[parameter_name])
                )
    lines.append("    try:")
    lines.append(f"        returned = body_with_its_dataclasses_lowered({', '.join(arguments)})")
    lines.append("    except Exception:")
    lines.append(f"        return {_EVALUATE_ENTRY_STATUS_REFUSED}")
    if state_record_class is not None:
        lines.append("    output, new_state = returned")
        for index, (field_name, value_type) in enumerate(state_value_types.items()):
            lines.append(f"    state[{index}] = {_to_f64(f'new_state.{field_name}', value_type)}")
    else:
        lines.append("    output = returned")
    for index, each_output in enumerate(function_signature["outputs"]):
        output_expression = "output" if output_record_class is None else f"output.{each_output['field']}"
        handed_out_slot = handed_out_slot_by_output_index.get(index)
        if handed_out_slot is None:
            lines.append(f"    outputs[{index}] = {_to_f64(output_expression, each_output['value_type'])}")
            continue
        lines.append(f"    output_{index} = {output_expression}")
        lines.append(f"    if output_{index} is None:")
        lines.append(f"        outputs[{index}] = 0.0")
        lines.append(f"        outputs[{handed_out_slot}] = 0.0")
        lines.append("    else:")
        lines.append(
            f"        outputs[{index}] = "
            f"{_optional_to_f64(f'output_{index}', each_output['value_type'])}"
        )
        lines.append(f"        outputs[{handed_out_slot}] = 1.0")
    lines.append("    return 0")
    return "\n".join(lines) + "\n", wrapper_globals


def _to_f64(value_expression: str, value_type: str) -> str:
    if value_type == "bool":
        return f"(1.0 if {value_expression} else 0.0)"
    return f"float({value_expression})"


def _optional_to_f64(value_expression: str, value_type: str) -> str:
    """`_to_f64` of a value Numba types as optional, which its `float` refuses and its arithmetic unwraps."""
    if value_type == "bool":
        return f"(1.0 if {value_expression} else 0.0)"
    return f"(0.0 + {value_expression})"


def _from_f64(value_expression: str, value_type: str) -> str:
    if value_type == "int":
        return f"int({value_expression})"
    if value_type == "bool":
        return f"({value_expression} != 0.0)"
    return value_expression

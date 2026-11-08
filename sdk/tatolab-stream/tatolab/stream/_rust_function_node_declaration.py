# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`node.rust_function(...)`: a function node a module wrote in Rust with `#[function_node]`.

The module's native half hands its Python half the declaration
`tatolab-module-sdk` builds, a dict. Reading it stamps the same declaration a
`@node.function` carries, with `language` `rust`, so the graph and the catalog
see one kind of node whichever language wrote it.
"""

from __future__ import annotations

import ctypes
import dataclasses
from collections.abc import Mapping
from typing import Any, Optional

from ._exposed_name_cast import ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe
from ._function_node_declaration import (
    FUNCTION_NODE_KIND,
    NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP,
    NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP,
    NODE_DECLARATION_LANGUAGE_STAMP,
    FunctionNode,
)
from ._node_config_schema import json_schema_for_a_node_declaring_no_config

__all__ = ["RustFunctionNode", "RustFunctionNodeDeclaration"]

RUST_FUNCTION_NODE_LANGUAGE = "rust"

# The value types an evaluate entry carries, each as one f64, and the JSON
# Schema type a config field of each is.
_JSON_SCHEMA_TYPE_BY_VALUE_TYPE = {"float": "number", "int": "integer", "bool": "boolean"}

# `ModuleHostTableFunctionNodeEvaluateEntry`: inputs, config, state, outputs, now.
_EVALUATE_ENTRY_C_SIGNATURE = ctypes.CFUNCTYPE(
    ctypes.c_int32,
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.c_int64,
)
_MODULE_HOST_TABLE_STATUS_OK = 0

# The module host table major whose evaluate entry `_EVALUATE_ENTRY_C_SIGNATURE` spells.
_MODULE_HOST_TABLE_ABI_MAJOR_OF_THE_EVALUATE_ENTRY_SIGNATURE = 1

# The largest integer magnitude an f64 holds exactly.
_LARGEST_INTEGER_AN_F64_HOLDS_EXACTLY = 2**53


@dataclasses.dataclass(frozen=True)
class RustFunctionNodeOutputPort:
    """An output port, and the value it carries."""

    name: str
    value_type: str


@dataclasses.dataclass(frozen=True)
class RustFunctionNodeRecordField:
    """A config or state field, with the value its record's `Default` gives it."""

    name: str
    value_type: str
    default: float


@dataclasses.dataclass(frozen=True)
class RustFunctionNodeInputPin:
    """An input pin, and its value when nothing is wired to it."""

    port: str
    value_type: str
    pin_default: Optional[float]


@dataclasses.dataclass(frozen=True)
class RustFunctionNodeDeclaration:
    """What `#[function_node]` declares, read from the dict its module's native half hands over."""

    short_name: str
    description: str
    input_pins: "tuple[RustFunctionNodeInputPin, ...]"
    output_ports: "tuple[RustFunctionNodeOutputPort, ...]"
    config_fields: "tuple[RustFunctionNodeRecordField, ...]"
    state_fields: "tuple[RustFunctionNodeRecordField, ...]"
    stream_time_parameter: Optional[str]
    state_parameter: Optional[str]
    config_parameter: Optional[str]
    evaluate_entry_address: int
    module_host_table_abi_major: int
    module_host_table_abi_minor: int


class RustFunctionNode(FunctionNode[..., Any]):
    """A `#[function_node]` Rust function: called through its evaluate entry, added to a stream as a node.

    Called with its input pins positionally or by port, its stream time,
    state and config by its Rust parameters' names. It answers its output (a
    dict by port when it has several), and when stateful the output and the
    state its next call takes, as a dict.
    """

    rust_function_node_declaration: RustFunctionNodeDeclaration

    def __init__(self, declaration: RustFunctionNodeDeclaration, module_name: str) -> None:
        self.rust_function_node_declaration = declaration
        self.__name__ = declaration.short_name
        self.__qualname__ = declaration.short_name
        self.__module__ = module_name
        self.__doc__ = declaration.description

    def __call__(self, *input_values: Any, **values_by_name: Any) -> Any:
        declaration = self.rust_function_node_declaration
        if declaration.module_host_table_abi_major != _MODULE_HOST_TABLE_ABI_MAJOR_OF_THE_EVALUATE_ENTRY_SIGNATURE:
            raise RuntimeError(
                f"{self.__name__} was built against module host table "
                f"{declaration.module_host_table_abi_major}.{declaration.module_host_table_abi_minor}, "
                f"whose evaluate entry this `tatolab-stream` cannot call; it calls major "
                f"{_MODULE_HOST_TABLE_ABI_MAJOR_OF_THE_EVALUATE_ENTRY_SIGNATURE}"
            )
        arguments_by_name = dict(values_by_name)
        input_pins = declaration.input_pins
        if len(input_values) > len(input_pins):
            raise TypeError(
                f"{self.__name__} takes {len(input_pins)} input pins, and was handed "
                f"{len(input_values)} positionally"
            )
        for input_pin, input_value in zip(input_pins, input_values):
            if input_pin.port in arguments_by_name:
                raise TypeError(f"{self.__name__} was handed `{input_pin.port}` twice")
            arguments_by_name[input_pin.port] = input_value

        input_f64s = [
            _the_input_value(self.__name__, input_pin, arguments_by_name.pop(input_pin.port, None))
            for input_pin in input_pins
        ]
        config_f64s = _record_values(
            self.__name__,
            declaration.config_fields,
            _pop_the_parameter(arguments_by_name, declaration.config_parameter),
        )
        state_f64s = _record_values(
            self.__name__,
            declaration.state_fields,
            _pop_the_parameter(arguments_by_name, declaration.state_parameter),
        )
        now_ns = _pop_the_parameter(arguments_by_name, declaration.stream_time_parameter)
        if declaration.stream_time_parameter is not None and now_ns is None:
            raise TypeError(
                f"{self.__name__} takes the stream time as `{declaration.stream_time_parameter}`, "
                f"and was handed none"
            )
        if arguments_by_name:
            raise TypeError(
                f"{self.__name__} takes no {', '.join(f'`{name}`' for name in sorted(arguments_by_name))}"
            )

        input_array = (ctypes.c_double * max(len(input_f64s), 1))(*input_f64s)
        config_array = (ctypes.c_double * max(len(config_f64s), 1))(*config_f64s)
        state_array = (ctypes.c_double * max(len(state_f64s), 1))(*state_f64s)
        output_array = (ctypes.c_double * len(declaration.output_ports))()
        evaluate_entry = _EVALUATE_ENTRY_C_SIGNATURE(declaration.evaluate_entry_address)
        status = evaluate_entry(input_array, config_array, state_array, output_array, int(now_ns or 0))
        if status != _MODULE_HOST_TABLE_STATUS_OK:
            raise RuntimeError(f"{self.__name__}'s evaluate entry refused the call (status {status})")

        outputs = {
            output_port.name: _from_f64(output_array[index], output_port.value_type)
            for index, output_port in enumerate(declaration.output_ports)
        }
        output: Any = outputs["result"] if list(outputs) == ["result"] else outputs
        if declaration.state_parameter is None:
            return output
        next_state = {
            state_field.name: _from_f64(state_array[index], state_field.value_type)
            for index, state_field in enumerate(declaration.state_fields)
        }
        return output, next_state

    def __repr__(self) -> str:
        return f"<rust function node {self.__module__}:{self.__qualname__}>"


def declare_rust_function_node(
    native_declaration: Mapping[str, Any],
    *,
    module_name: str,
    declaration_stamps: "dict[str, str]",
) -> RustFunctionNode:
    """Read the declaration a module's native half built into a function node, refusing by name what is not one."""
    declaration = read_rust_function_node_declaration(native_declaration)
    function_node = RustFunctionNode(declaration, module_name)
    input_ports: "list[dict[str, Any]]" = []
    signature_inputs: "list[dict[str, Any]]" = []
    for input_pin in declaration.input_pins:
        input_ports.append(
            {
                "name": input_pin.port,
                "description": "",
                "delivery_profile": "newest",
                "family": {"family": "channels"},
            }
        )
        signature_input: "dict[str, Any]" = {
            "parameter": input_pin.port,
            "port": input_pin.port,
            "value_type": input_pin.value_type,
            "optional": False,
        }
        if input_pin.pin_default is not None:
            signature_input["default"] = _from_f64(input_pin.pin_default, input_pin.value_type)
        signature_inputs.append(signature_input)
    several_outputs = [output_port.name for output_port in declaration.output_ports] != ["result"]
    stamps: "dict[str, Any]" = {
        declaration_stamps["declared"]: True,
        declaration_stamps["kind"]: FUNCTION_NODE_KIND,
        NODE_DECLARATION_LANGUAGE_STAMP: RUST_FUNCTION_NODE_LANGUAGE,
        declaration_stamps["config_class"]: None,
        declaration_stamps["config_schema"]: _config_schema(declaration.config_fields),
        declaration_stamps["description"]: declaration.description,
        declaration_stamps["input_ports"]: input_ports,
        declaration_stamps["output_ports"]: [
            {
                "name": output_port.name,
                "description": "",
                "family": {"family": "channels", "names": {"stated": [output_port.name]}, "dtype": "f64"},
            }
            for output_port in declaration.output_ports
        ],
        NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP: {
            "inputs": signature_inputs,
            "outputs": [
                {
                    "port": output_port.name,
                    "field": output_port.name if several_outputs else None,
                    "value_type": output_port.value_type,
                }
                for output_port in declaration.output_ports
            ],
            "time_parameter": declaration.stream_time_parameter,
            "state_parameter": declaration.state_parameter,
            "state_record_class": None,
            "config_parameter": declaration.config_parameter,
        },
        NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP: [
            state_field.name for state_field in declaration.state_fields
        ],
    }
    for stamp, value in stamps.items():
        setattr(function_node, stamp, value)
    return function_node


def read_rust_function_node_declaration(native_declaration: Mapping[str, Any]) -> RustFunctionNodeDeclaration:
    """The declaration `native_declaration` carries, refused by name where it is not one `tatolab-module-sdk` built."""
    if not isinstance(native_declaration, Mapping):
        raise TypeError(
            f"node.rust_function takes the declaration a `#[function_node]` function's "
            f"`<function>_function_node_declaration().to_python_declaration(py)` builds, got "
            f"{native_declaration!r}"
        )
    reading = _NativeDeclarationReading(native_declaration)
    short_name = reading.text("short_name")
    if not short_name.isidentifier():
        raise ValueError(f"a Rust function node's short name {short_name!r} is not a Python identifier")
    ports_by_cast_name: "dict[str, str]" = {}
    input_pins = tuple(
        RustFunctionNodeInputPin(
            port=_a_port_name(short_name, input_pin.text("port"), ports_by_cast_name),
            value_type=input_pin.value_type(),
            pin_default=input_pin.optional_number("pin_default"),
        )
        for input_pin in reading.entries("input_pins")
    )
    ports_by_cast_name = {}
    output_ports = tuple(
        RustFunctionNodeOutputPort(
            name=_a_port_name(short_name, output_port.text("name"), ports_by_cast_name),
            value_type=output_port.value_type(),
        )
        for output_port in reading.entries("output_ports")
    )
    if not output_ports:
        raise ValueError(f"Rust function node {short_name} declares no output")
    return RustFunctionNodeDeclaration(
        short_name=short_name,
        description=reading.text("description"),
        input_pins=input_pins,
        output_ports=output_ports,
        config_fields=reading.record_fields("config_fields"),
        state_fields=reading.record_fields("state_fields"),
        stream_time_parameter=reading.optional_text("stream_time_parameter"),
        state_parameter=reading.optional_text("state_parameter"),
        config_parameter=reading.optional_text("config_parameter"),
        evaluate_entry_address=reading.whole_number("evaluate_entry_address"),
        module_host_table_abi_major=reading.whole_number("module_host_table_abi_major"),
        module_host_table_abi_minor=reading.whole_number("module_host_table_abi_minor"),
    )


class _NativeDeclarationReading:
    def __init__(self, entries_by_key: Mapping[str, Any]) -> None:
        self._entries_by_key = entries_by_key

    def _entry(self, key: str) -> Any:
        if key not in self._entries_by_key:
            raise ValueError(
                f"a Rust function node's declaration carries no `{key}`; it is built by "
                f"`tatolab-module-sdk`'s `#[function_node]`, at a version this `tatolab-stream` reads"
            )
        return self._entries_by_key[key]

    def text(self, key: str) -> str:
        value = self._entry(key)
        if not isinstance(value, str):
            raise ValueError(f"a Rust function node's `{key}` is {value!r}, not text")
        return value

    def optional_text(self, key: str) -> Optional[str]:
        value = self._entry(key)
        if value is not None and not isinstance(value, str):
            raise ValueError(f"a Rust function node's `{key}` is {value!r}, not text or None")
        return value

    def whole_number(self, key: str) -> int:
        value = self._entry(key)
        if isinstance(value, bool) or not isinstance(value, int):
            raise ValueError(f"a Rust function node's `{key}` is {value!r}, not a whole number")
        return value

    def optional_number(self, key: str) -> Optional[float]:
        value = self._entry(key)
        if value is None:
            return None
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise ValueError(f"a Rust function node's `{key}` is {value!r}, not a number")
        return float(value)

    def value_type(self) -> str:
        value_type = self.text("value_type")
        if value_type not in _JSON_SCHEMA_TYPE_BY_VALUE_TYPE:
            raise ValueError(
                f"a Rust function node's value type {value_type!r} is none of "
                f"{', '.join(_JSON_SCHEMA_TYPE_BY_VALUE_TYPE)}"
            )
        return value_type

    def entries(self, key: str) -> "list[_NativeDeclarationReading]":
        value = self._entry(key)
        if not isinstance(value, (list, tuple)) or not all(isinstance(entry, Mapping) for entry in value):
            raise ValueError(f"a Rust function node's `{key}` is {value!r}, not a list of entries")
        return [_NativeDeclarationReading(entry) for entry in value]

    def record_fields(self, key: str) -> "tuple[RustFunctionNodeRecordField, ...]":
        record_fields: "list[RustFunctionNodeRecordField]" = []
        for record_field in self.entries(key):
            default = record_field.optional_number("default")
            if default is None:
                raise ValueError(f"a Rust function node's `{key}` entry carries no default")
            record_fields.append(
                RustFunctionNodeRecordField(
                    name=record_field.text("name"),
                    value_type=record_field.value_type(),
                    default=default,
                )
            )
        return tuple(record_fields)


def _a_port_name(short_name: str, spelling: str, ports_by_cast_name: "dict[str, str]") -> str:
    try:
        port_name = cast_exposed_name_to_url_safe(spelling)
    except ExposedNameCastsToNothingError as names_nothing:
        raise ValueError(f"{short_name} declares a port {spelling!r}: {names_nothing}") from names_nothing
    first_spelling = ports_by_cast_name.setdefault(port_name, spelling)
    if first_spelling != spelling:
        raise ValueError(
            f"{short_name} declares the ports {first_spelling!r} and {spelling!r}, which both "
            f"cast to {port_name!r}; two ports need names that stay apart once cast"
        )
    return port_name


def _config_schema(config_fields: "tuple[RustFunctionNodeRecordField, ...]") -> "dict[str, Any]":
    if not config_fields:
        return json_schema_for_a_node_declaring_no_config()
    return {
        "type": "object",
        "properties": {
            config_field.name: {
                "type": _JSON_SCHEMA_TYPE_BY_VALUE_TYPE[config_field.value_type],
                "default": _from_f64(config_field.default, config_field.value_type),
            }
            for config_field in config_fields
        },
        "additionalProperties": False,
    }


def _pop_the_parameter(arguments_by_name: "dict[str, Any]", parameter: Optional[str]) -> Any:
    if parameter is None:
        return None
    return arguments_by_name.pop(parameter, None)


def _the_input_value(short_name: str, input_pin: RustFunctionNodeInputPin, value: Any) -> float:
    if value is None:
        if input_pin.pin_default is None:
            raise TypeError(f"{short_name}'s input `{input_pin.port}` has no default and was handed none")
        return input_pin.pin_default
    return _exactly_as_f64(short_name, input_pin.port, value)


def _record_values(
    short_name: str,
    record_fields: "tuple[RustFunctionNodeRecordField, ...]",
    values_by_field: Optional[Mapping[str, Any]],
) -> "list[float]":
    values_by_field = dict(values_by_field or {})
    unknown_fields = sorted(set(values_by_field) - {field.name for field in record_fields})
    if unknown_fields:
        raise TypeError(f"{short_name} declares no field {', '.join(unknown_fields)}")
    return [
        _exactly_as_f64(short_name, record_field.name, values_by_field[record_field.name])
        if record_field.name in values_by_field
        else record_field.default
        for record_field in record_fields
    ]


def _exactly_as_f64(short_name: str, name: str, value: Any) -> float:
    if isinstance(value, int) and not isinstance(value, bool) and abs(value) > _LARGEST_INTEGER_AN_F64_HOLDS_EXACTLY:
        raise ValueError(
            f"{short_name}'s `{name}` is {value}, and an evaluate entry carries each value as an "
            f"f64, which holds an integer exactly only up to 2**53"
        )
    return float(value)


def _from_f64(value: float, value_type: str) -> Any:
    if value_type == "int":
        return int(value)
    if value_type == "bool":
        return value != 0.0
    return value

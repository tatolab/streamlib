# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`node.rust_function(...)` reads a `#[function_node]` declaration into the
same stamps `@node.function` writes, with `language` `rust`, and calls the node
through its evaluate entry."""

from __future__ import annotations

import ctypes
import math
from typing import Any

import pytest

from tatolab.stream import RustFunctionNode, StreamBuilder, compile_stream_to_graph, node, stream
from tatolab.stream._function_node_declaration import (
    NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP,
    NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP,
    NODE_DECLARATION_LANGUAGE_STAMP,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_CONFIG_SCHEMA_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_KIND_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

_EVALUATE_ENTRY_TYPE = ctypes.CFUNCTYPE(
    ctypes.c_int32,
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.c_int64,
)


@_EVALUATE_ENTRY_TYPE
def _ramp_evaluate_entry(
    input_values: Any, config_values: Any, state_values: Any, output_values: Any, now_ns: int
) -> int:
    level = state_values[0] + input_values[0] * config_values[0]
    output_values[0] = level
    output_values[1] = float(now_ns)
    output_values[2] = 1.0 if level > 1.0 else 0.0
    state_values[0] = level
    return 0


@_EVALUATE_ENTRY_TYPE
def _refusing_evaluate_entry(
    input_values: Any, config_values: Any, state_values: Any, output_values: Any, now_ns: int
) -> int:
    return -1


def a_ramp_declaration(**overrides: Any) -> "dict[str, Any]":
    declaration: "dict[str, Any]" = {
        "short_name": "Ramp",
        "description": "A level that climbs by `rate` times the config's `gain` each call.",
        "input_pins": [{"port": "rate", "value_type": "float", "pin_default": 0.5}],
        "output_ports": [
            {"name": "level", "value_type": "float"},
            {"name": "evaluated_at_ns", "value_type": "int"},
            {"name": "overflowed", "value_type": "bool"},
        ],
        "config_fields": [{"name": "gain", "value_type": "float", "default": 2.0}],
        "state_fields": [{"name": "level", "value_type": "float", "default": 0.0}],
        "stream_time_parameter": "now",
        "state_parameter": "previous",
        "config_parameter": "config",
        "evaluate_entry_address": ctypes.cast(_ramp_evaluate_entry, ctypes.c_void_p).value,
        "module_host_table_abi_major": 1,
        "module_host_table_abi_minor": 12,
    }
    declaration.update(overrides)
    return declaration


Ramp = node.rust_function(a_ramp_declaration())


def test_a_rust_function_node_is_a_function_node_named_by_its_module_and_short_name():
    assert isinstance(Ramp, RustFunctionNode)
    assert (Ramp.__module__, Ramp.__qualname__) == (__name__, "Ramp")
    assert getattr(Ramp, NODE_DECLARATION_KIND_STAMP) == "function"
    assert getattr(Ramp, NODE_DECLARATION_LANGUAGE_STAMP) == "rust"


def test_its_ports_config_and_state_are_stamped_as_a_function_nodes():
    assert getattr(Ramp, NODE_DECLARATION_INPUT_PORTS_STAMP) == [
        {
            "name": "rate",
            "description": "",
            "delivery_profile": "newest",
            "family": {"family": "channels"},
        }
    ]
    assert [port["name"] for port in getattr(Ramp, NODE_DECLARATION_OUTPUT_PORTS_STAMP)] == [
        "level",
        "evaluated_at_ns",
        "overflowed",
    ]
    assert getattr(Ramp, NODE_DECLARATION_CONFIG_SCHEMA_STAMP) == {
        "type": "object",
        "properties": {"gain": {"type": "number", "default": 2.0}},
        "additionalProperties": False,
    }
    assert getattr(Ramp, NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP) == ["level"]
    signature = getattr(Ramp, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    assert signature["inputs"] == [
        {"parameter": "rate", "port": "rate", "value_type": "float", "optional": False, "default": 0.5}
    ]
    assert (signature["time_parameter"], signature["state_parameter"]) == ("now", "previous")


def test_it_is_called_through_its_evaluate_entry_with_its_rust_parameter_names():
    outputs, next_state = Ramp(now=7, config={"gain": 3.0})
    assert outputs == {"level": 1.5, "evaluated_at_ns": 7, "overflowed": True}
    assert next_state == {"level": 1.5}
    outputs, _ = Ramp(0.25, now=9, previous=next_state)
    assert outputs == {"level": 2.0, "evaluated_at_ns": 9, "overflowed": True}


def test_a_call_its_evaluate_entry_refuses_raises():
    refusing = node.rust_function(
        a_ramp_declaration(
            evaluate_entry_address=ctypes.cast(_refusing_evaluate_entry, ctypes.c_void_p).value
        )
    )
    with pytest.raises(RuntimeError, match="refused"):
        refusing(now=0)


def test_a_call_naming_what_it_does_not_take_is_refused_by_name():
    with pytest.raises(TypeError, match="`velocity`"):
        Ramp(now=0, velocity=1.0)
    with pytest.raises(TypeError, match="stream time"):
        Ramp()


@pytest.mark.parametrize(
    ("override", "refusal"),
    [
        ({"short_name": "not an identifier"}, "identifier"),
        ({"output_ports": []}, "no output"),
        ({"input_pins": [{"port": "rate", "value_type": "complex", "pin_default": None}]}, "complex"),
        ({"evaluate_entry_address": "0x1"}, "whole number"),
    ],
)
def test_a_declaration_tatolab_module_sdk_did_not_build_is_refused_by_name(
    override: "dict[str, Any]", refusal: str
):
    with pytest.raises(ValueError, match=refusal):
        node.rust_function(a_ramp_declaration(**override))


def test_a_declaration_missing_a_key_names_it():
    declaration = a_ramp_declaration()
    del declaration["state_fields"]
    with pytest.raises(ValueError, match="`state_fields`"):
        node.rust_function(declaration)


@stream
def ramp_stream(stream_builder: StreamBuilder) -> None:
    stream_builder.add(Ramp, config={"gain": 1.0})


def test_a_stream_adds_it_by_its_import_path():
    graph = compile_stream_to_graph(ramp_stream)
    assert [graph_node["type"] for graph_node in graph["nodes"]] == [f"{__name__}:Ramp"]


def test_a_call_to_a_node_built_for_another_table_major_is_refused_before_its_entry_runs():
    built_for_another_major = node.rust_function(a_ramp_declaration(module_host_table_abi_major=2))
    with pytest.raises(RuntimeError, match="module host table 2.12"):
        built_for_another_major(now=0)


def test_an_integer_an_f64_cannot_hold_exactly_is_refused_by_name():
    with pytest.raises(ValueError, match="`rate`"):
        Ramp(2**53 + 1, now=0)
    with pytest.raises(ValueError, match="`gain`"):
        Ramp(now=0, config={"gain": 2**53 + 1})


def test_a_negative_zero_default_keeps_its_sign():
    negative_zero_gain = node.rust_function(
        a_ramp_declaration(
            config_fields=[{"name": "gain", "value_type": "float", "default": -0.0}]
        )
    )
    schema = getattr(negative_zero_gain, NODE_DECLARATION_CONFIG_SCHEMA_STAMP)
    assert math.copysign(1.0, schema["properties"]["gain"]["default"]) == -1.0

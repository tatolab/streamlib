# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`node.rust_function(...)` reads a `#[function_node]` declaration into the
same stamps `@node.function` writes, with `language` `rust`, and calls the node
through its evaluate entry."""

from __future__ import annotations

import ctypes
import math
from typing import Any

import pytest

from tatolab.stream import (
    RustFunctionNode,
    StreamBuilder,
    UnreadMessage,
    graph_generator,
    node,
    run_graph_generator,
)
from tatolab.stream._function_node_declaration import (
    NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP,
    NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP,
    NODE_DECLARATION_LANGUAGE_STAMP,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_CATEGORY_STAMP,
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
    assert getattr(Ramp, NODE_DECLARATION_CATEGORY_STAMP) is None
    assert getattr(Ramp, NODE_DECLARATION_KIND_STAMP) == "function"
    assert getattr(Ramp, NODE_DECLARATION_LANGUAGE_STAMP) == "rust"


def test_a_rust_function_node_carries_the_category_its_native_half_declares():
    categorized = node.rust_function(a_ramp_declaration(category="Animation/Ramps"))

    assert getattr(categorized, NODE_DECLARATION_CATEGORY_STAMP) == "Animation/Ramps"


def test_its_ports_config_and_state_are_stamped_as_a_function_nodes():
    assert getattr(Ramp, NODE_DECLARATION_INPUT_PORTS_STAMP) == [
        {
            "name": "rate",
            "description": "",
            "delivery_profile": "newest",
            "family": {"family": "channels"},
            "pin_default": 0.5,
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


@graph_generator
def ramp_stream(stream_builder: StreamBuilder) -> None:
    stream_builder.add(Ramp, config={"gain": 1.0})


def test_a_stream_adds_it_by_its_import_path():
    graph = run_graph_generator(ramp_stream)
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


@_EVALUATE_ENTRY_TYPE
def _gate_evaluate_entry(
    input_values: Any, config_values: Any, state_values: Any, output_values: Any, now_ns: int
) -> int:
    is_open = input_values[1] != 0.0
    output_values[0] = input_values[0] if is_open else 0.0
    output_values[1] = 1.0 if is_open else 0.0
    return 0


def a_gate_declaration(**overrides: Any) -> "dict[str, Any]":
    declaration: "dict[str, Any]" = {
        "short_name": "Gate",
        "description": "`value` handed on while `open`, and nothing while closed.",
        "input_pins": [
            {"port": "value", "value_type": "unread_message", "pin_default": None, "takes_none": False},
            {"port": "open", "value_type": "bool", "pin_default": 1.0, "takes_none": False},
        ],
        "output_ports": [{"name": "result", "value_type": "unread_message", "may_hand_out_nothing": True}],
        "config_fields": [],
        "state_fields": [],
        "stream_time_parameter": None,
        "state_parameter": None,
        "config_parameter": None,
        "evaluate_entry_address": ctypes.cast(_gate_evaluate_entry, ctypes.c_void_p).value,
        "module_host_table_abi_major": 1,
        "module_host_table_abi_minor": 12,
    }
    declaration.update(overrides)
    return declaration


RustGate = node.rust_function(a_gate_declaration())


@node.function
def gate(value: UnreadMessage, open: bool = True) -> "UnreadMessage | None":
    return value if open else None


def test_a_rust_gate_is_stamped_as_the_python_gate_of_the_same_signature_is():
    for stamp in (NODE_DECLARATION_INPUT_PORTS_STAMP, NODE_DECLARATION_OUTPUT_PORTS_STAMP):
        assert getattr(RustGate, stamp) == getattr(gate, stamp)
    rust_signature = getattr(RustGate, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    python_signature = getattr(gate, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    assert rust_signature["inputs"] == python_signature["inputs"]
    assert rust_signature["outputs"] == python_signature["outputs"]


def test_a_rust_gate_called_hands_on_the_object_it_was_handed_or_none():
    handed = object()
    assert RustGate(handed) is handed
    assert RustGate(handed, open=False) is None
    with pytest.raises(TypeError, match="`value` takes a message"):
        RustGate(None)


def test_an_optional_unread_message_pin_is_stamped_taking_none():
    takes_none = node.rust_function(
        a_gate_declaration(
            input_pins=[
                {"port": "value", "value_type": "unread_message", "pin_default": None, "takes_none": True},
                {"port": "open", "value_type": "bool", "pin_default": 1.0, "takes_none": False},
            ]
        )
    )
    (value_input, _) = getattr(takes_none, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)["inputs"]
    assert (value_input["optional"], value_input["default"]) == (True, None)
    assert getattr(takes_none, NODE_DECLARATION_INPUT_PORTS_STAMP)[0]["pin_default"] is None
    assert takes_none(None) is None


def test_a_number_that_may_be_none_answers_none_when_its_flag_says_so():
    @_EVALUATE_ENTRY_TYPE
    def reciprocal_evaluate_entry(
        input_values: Any, config_values: Any, state_values: Any, output_values: Any, now_ns: int
    ) -> int:
        output_values[0] = 1.0 / input_values[0] if input_values[0] != 0.0 else 0.0
        output_values[1] = 1.0 if input_values[0] != 0.0 else 0.0
        return 0

    reciprocal = node.rust_function(
        a_gate_declaration(
            short_name="Reciprocal",
            input_pins=[{"port": "x", "value_type": "float", "pin_default": None}],
            output_ports=[{"name": "result", "value_type": "float", "may_hand_out_nothing": True}],
            evaluate_entry_address=ctypes.cast(reciprocal_evaluate_entry, ctypes.c_void_p).value,
        )
    )
    assert reciprocal(4.0) == 0.25
    assert reciprocal(0.0) is None
    (result_output,) = getattr(reciprocal, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)["outputs"]
    assert result_output["may_hand_out_nothing"]


@pytest.mark.parametrize(
    ("override", "refusal"),
    [
        (
            {
                "input_pins": [
                    {"port": "open", "value_type": "bool", "pin_default": 1.0, "takes_none": False}
                ]
            },
            "takes 0 of them",
        ),
        (
            {
                "input_pins": [
                    {"port": "value", "value_type": "unread_message", "pin_default": 1.0},
                ]
            },
            "takes no pin default",
        ),
        (
            {"input_pins": [{"port": "x", "value_type": "float", "pin_default": None, "takes_none": True}]},
            "only an `UnreadMessage` input does",
        ),
        (
            {"state_fields": [{"name": "held", "value_type": "unread_message", "default": 0.0}]},
            "unread_message",
        ),
    ],
)
def test_a_gate_declaration_breaking_what_unread_messages_allow_is_refused_by_name(
    override: "dict[str, Any]", refusal: str
):
    with pytest.raises(ValueError, match=refusal):
        node.rust_function(a_gate_declaration(**override))

# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The function nodes and helpers more than one function node test file uses."""

from __future__ import annotations

import ctypes
import math
from dataclasses import dataclass
from typing import Any

from tatolab.runtime import _engine
from tatolab.runtime._function_node_compilation import (
    FunctionNodeRunAsPythonInItsReader,
    prepare_function_node_to_evaluate,
)
from tatolab.stream import State, Stateful, Time, node


# Kept on `@node.function`: its `x` holds no default, which bare `@node` refuses, and the
# island tests read what an input with no link, pin or default does.
@node.function
def remap(x: float, low: float = 0.0, high: float = 1.0) -> float:
    return low + x * (high - low)


@dataclass(frozen=True)
class Phase:
    cycles: float = 0.0
    last_now_ns: int = 0


@node
def lfo(rate_hz: float = 1.0, *, now: Time, previous: State[Phase]) -> Stateful[float, Phase]:
    elapsed_seconds = (now - previous.last_now_ns) * 1e-9 if previous.last_now_ns else 0.0
    cycles = (previous.cycles + rate_hz * elapsed_seconds) % 1.0
    return math.sin(2.0 * math.pi * cycles), Phase(cycles, now)


@dataclass(frozen=True)
class GainConfig:
    gain: float = 2.0
    inverted: bool = False


@node.function
def amplify(x: float, config: GainConfig) -> float:
    return -x * config.gain if config.inverted else x * config.gain


@node
def tally(x: float = 0.0, count: int = 3) -> int:
    return count + int(x)


_EVALUATE_ENTRY_TYPE = ctypes.CFUNCTYPE(
    ctypes.c_int32,
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.c_int64,
)


def an_island_of_one_node(
    input_port: str, import_path: str, config: Any, node_id: str = "only-node"
) -> "dict[str, Any]":
    return {
        "input_port": input_port,
        "link_id": f"L-{input_port}",
        "output": {"node": node_id, "port": "result"},
        "nodes": [
            {
                "node": node_id,
                "breadcrumb_number": 0,
                "import_path": import_path,
                "config": config,
                "links_into": [],
            }
        ],
    }


@_EVALUATE_ENTRY_TYPE
def _weighted_sum_counting_its_calls_evaluate_entry(
    input_values: Any, config_values: Any, state_values: Any, output_values: Any, now_ns: int
) -> int:
    output_values[0] = input_values[0] * config_values[0] + input_values[1]
    state_values[0] = state_values[0] + 1.0
    return 0


def a_rust_function_node_declaration(**overrides: Any) -> "dict[str, Any]":
    declaration: "dict[str, Any]" = {
        "short_name": "WeightedSumCountingItsCalls",
        "description": "`x` times the config's weight, plus `bias`.",
        "input_pins": [
            {"port": "x", "value_type": "float", "pin_default": None},
            {"port": "bias", "value_type": "float", "pin_default": 0.25},
        ],
        "output_ports": [{"name": "result", "value_type": "float"}],
        "config_fields": [{"name": "weight", "value_type": "float", "default": 2.0}],
        "state_fields": [{"name": "calls", "value_type": "int", "default": 10.0}],
        "stream_time_parameter": None,
        "state_parameter": "previous",
        "config_parameter": "config",
        "evaluate_entry_address": ctypes.cast(
            _weighted_sum_counting_its_calls_evaluate_entry, ctypes.c_void_p
        ).value,
        "module_host_table_abi_major": _engine.MODULE_HOST_TABLE_ABI_MAJOR,
        "module_host_table_abi_minor": _engine.MODULE_HOST_TABLE_ABI_MINOR,
    }
    declaration.update(overrides)
    return declaration


WeightedSumCountingItsCalls = node.rust_function(a_rust_function_node_declaration())


@_EVALUATE_ENTRY_TYPE
def _rust_gate_evaluate_entry(
    input_values: Any, config_values: Any, state_values: Any, output_values: Any, now_ns: int
) -> int:
    is_open = input_values[1] != 0.0
    output_values[0] = input_values[0] if is_open else 0.0
    output_values[1] = 1.0 if is_open else 0.0
    return 0


RustGate = node.rust_function(
    a_rust_function_node_declaration(
        short_name="RustGate",
        description="`value` handed on unread while `open`, and nothing while closed.",
        input_pins=[
            {"port": "value", "value_type": "unread_message", "pin_default": None, "takes_none": False},
            {"port": "open", "value_type": "bool", "pin_default": 1.0, "takes_none": False},
        ],
        output_ports=[{"name": "result", "value_type": "unread_message", "may_hand_out_nothing": True}],
        config_fields=[],
        state_fields=[],
        state_parameter=None,
        config_parameter=None,
        evaluate_entry_address=ctypes.cast(_rust_gate_evaluate_entry, ctypes.c_void_p).value,
    )
)


@node
def measure_its_own_spelling(x: float = 2.0) -> float:
    return float(len(f"{x:.1f}"))


@node.function
def label_the_level(level: float = 0.75) -> dict:
    return {"label": "high" if level > 0.5 else "low"}


@node.function
def count_the_label_letters(record: dict) -> int:
    return len(record["label"])


@dataclass(frozen=True)
class LabelsSeen:
    labels: str = ""


@node
def remember_each_call_by_a_letter(
    letter_count: float = 1.0, *, previous: State[LabelsSeen]
) -> Stateful[float, LabelsSeen]:
    labels = previous.labels + "x" * int(letter_count)
    return float(len(labels)), LabelsSeen(labels)


def run_as_python(function_node: Any) -> FunctionNodeRunAsPythonInItsReader:
    prepared = prepare_function_node_to_evaluate(
        function_node, f"{function_node.__module__}:{function_node.__qualname__}"
    )
    assert isinstance(prepared, FunctionNodeRunAsPythonInItsReader), prepared
    return prepared

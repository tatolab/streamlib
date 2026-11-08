# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A `@node.function` body compiled to its evaluate entry, and an island of them fed to a port.

Each node is called through its compiled entry the way the native evaluator
calls it, and an island is read off a helper's own data plane as the node it
feeds reads it: one f64 sample on one channel, evaluated at the read.
"""

from __future__ import annotations

import ctypes
import math
import struct
from dataclasses import dataclass
from typing import Any

import pytest

from tatolab.runtime import _engine
from tatolab.runtime._function_node_compilation import (
    FunctionNodeCompiledToItsEvaluateEntry,
    FunctionNodeDoesNotCompile,
    compile_function_node_to_its_evaluate_entry,
    feed_each_input_port_from_its_function_node_island,
)
from tatolab.stream import State, Stateful, Time, node

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


@node.function
def remap(x: float, low: float = 0.0, high: float = 1.0) -> float:
    return low + x * (high - low)


@dataclass(frozen=True)
class Phase:
    cycles: float = 0.0
    last_now_ns: int = 0


@node.function
def lfo(rate_hz: float = 1.0, *, now: Time, previous: State[Phase]) -> Stateful[float, Phase]:
    elapsed_seconds = (now - previous.last_now_ns) * 1e-9 if previous.last_now_ns else 0.0
    cycles = (previous.cycles + rate_hz * elapsed_seconds) % 1.0
    return math.sin(2.0 * math.pi * cycles), Phase(cycles, now)


@dataclass(frozen=True)
class Halves:
    lower: float
    upper: float


@node.function
def halve(x: float) -> Halves:
    return Halves(lower=x / 2.0, upper=x - x / 2.0)


@dataclass(frozen=True)
class GainConfig:
    gain: float = 2.0
    inverted: bool = False


@node.function
def amplify(x: float, config: GainConfig) -> float:
    return -x * config.gain if config.inverted else x * config.gain


@node.function
def tally(x: float, count: int = 3) -> int:
    return count + int(x)


@node.function
def is_positive(x: float) -> bool:
    return x > 0.0


@node.function
def build_an_object(x: float) -> float:
    return float(len(repr(object()))) + x


_EVALUATE_ENTRY_TYPE = ctypes.CFUNCTYPE(
    ctypes.c_int32,
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.c_int64,
)


def call_the_evaluate_entry(
    compiled: FunctionNodeCompiledToItsEvaluateEntry,
    inputs: "list[float]",
    config: "list[float]",
    state: "list[float]",
    now_ns: int = 0,
) -> "tuple[list[float], list[float]]":
    input_values = (ctypes.c_double * max(len(inputs), 1))(*inputs)
    config_values = (ctypes.c_double * max(len(config), 1))(*config)
    state_values = (ctypes.c_double * max(len(state), 1))(*state)
    output_values = (ctypes.c_double * len(compiled.output_ports))()
    status = _EVALUATE_ENTRY_TYPE(compiled.evaluate_entry_address)(
        input_values, config_values, state_values, output_values, now_ns
    )
    assert status == 0
    return list(output_values), list(state_values)[: len(state)]


def compiled(function_node: Any) -> FunctionNodeCompiledToItsEvaluateEntry:
    return compile_function_node_to_its_evaluate_entry(
        function_node, f"{__name__}:{function_node.__qualname__}"
    )


def test_a_stateless_body_runs_compiled_with_its_pins_in_signature_order():
    remap_compiled = compiled(remap)
    assert remap_compiled.input_ports == ("x", "low", "high")
    assert remap_compiled.pin_defaults_by_input_port == {"low": 0.0, "high": 1.0}
    assert call_the_evaluate_entry(remap_compiled, [0.5, 2.0, 4.0], [], [])[0] == [3.0]


def test_a_stateful_body_reads_time_and_its_state_and_writes_its_next_state():
    lfo_compiled = compiled(lfo)
    assert lfo_compiled.initial_state_values == (0.0, 0.0)
    outputs, state = call_the_evaluate_entry(lfo_compiled, [1.0], [], [0.0, 0.0], 1_000_000_000)
    assert outputs == [0.0] and state == [0.0, 1_000_000_000.0]
    outputs, state = call_the_evaluate_entry(lfo_compiled, [1.0], [], state, 1_250_000_000)
    assert outputs[0] == pytest.approx(1.0) and state == [0.25, 1_250_000_000.0]


def test_a_returned_dataclass_writes_one_output_per_field():
    halve_compiled = compiled(halve)
    assert halve_compiled.output_ports == ("lower", "upper")
    assert call_the_evaluate_entry(halve_compiled, [3.0], [], [])[0] == [1.5, 1.5]


def test_config_is_read_as_its_dataclass_with_a_bool_as_zero_or_one():
    amplify_compiled = compiled(amplify)
    assert amplify_compiled.config_fields == ("gain", "inverted")
    assert amplify_compiled.config_field_defaults == {"gain": 2.0, "inverted": 0.0}
    assert call_the_evaluate_entry(amplify_compiled, [3.0], [2.0, 0.0], [])[0] == [6.0]
    assert call_the_evaluate_entry(amplify_compiled, [3.0], [2.0, 1.0], [])[0] == [-6.0]


def test_an_int_input_and_output_travel_as_f64():
    assert call_the_evaluate_entry(compiled(tally), [2.0, 5.0], [], [])[0] == [7.0]


@pytest.mark.parametrize(
    ("function_node", "inputs", "config", "state", "now_ns"),
    [
        (remap, [0.25, -2.0, 6.0], [], [], 0),
        (lfo, [3.0], [], [0.1, 1_000_000_000.0], 1_100_000_000),
        (amplify, [1.5], [3.0, 1.0], [], 0),
        (tally, [4.0, 2.0], [], [], 0),
    ],
)
def test_a_compiled_body_matches_the_python_it_was_compiled_from(
    function_node: Any, inputs: "list[float]", config: "list[float]", state: "list[float]", now_ns: int
):
    outputs, new_state = call_the_evaluate_entry(compiled(function_node), inputs, config, state, now_ns)
    if function_node is remap:
        expected: Any = remap(*inputs)
    elif function_node is lfo:
        expected, expected_state = lfo(inputs[0], now=now_ns, previous=Phase(state[0], int(state[1])))
        assert new_state == pytest.approx([expected_state.cycles, expected_state.last_now_ns])
    elif function_node is amplify:
        expected = amplify(inputs[0], GainConfig(gain=config[0], inverted=bool(config[1])))
    else:
        expected = tally(inputs[0], int(inputs[1]))
    assert outputs == pytest.approx([float(expected)])


def test_a_second_compile_of_a_body_loads_it_from_numbas_cache():
    compiled(remap)
    assert compiled(remap).body_loaded_from_numbas_cache


def test_a_bool_output_travels_as_zero_or_one():
    is_positive_compiled = compiled(is_positive)
    assert call_the_evaluate_entry(is_positive_compiled, [2.0], [], [])[0] == [1.0]
    assert call_the_evaluate_entry(is_positive_compiled, [-2.0], [], [])[0] == [0.0]


def test_a_body_that_does_not_compile_is_refused_by_name():
    with pytest.raises(FunctionNodeDoesNotCompile, match="build_an_object's body does not compile"):
        compiled(build_an_object)


def test_what_is_not_a_function_node_is_refused():
    with pytest.raises(FunctionNodeDoesNotCompile, match="not a `@node.function` function"):
        compile_function_node_to_its_evaluate_entry(math.sin, "math:sin")


_FUNCTION_NODES_BY_IMPORT_PATH = {
    f"{__name__}:{function_node.__qualname__}": function_node
    for function_node in (remap, lfo, amplify)
}


def an_island_of_lfo_then_remap_feeding(input_port: str) -> "dict[str, Any]":
    return {
        "input_port": input_port,
        "link_id": "L-island",
        "output": {"node": "remap-1", "port": "result"},
        "nodes": [
            {
                "node": "lfo-1",
                "import_path": f"{__name__}:lfo",
                "config": None,
                "links_into": [],
            },
            {
                "node": "remap-1",
                "import_path": f"{__name__}:remap",
                "config": None,
                "links_into": [{"port": "x", "from": {"node": "lfo-1", "port": "result"}}],
            },
        ],
    }


def test_an_island_feeds_its_port_one_f64_sample_evaluated_at_the_read():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["brightness"], [])
    compiled_for_the_island = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_lfo_then_remap_feeding("brightness")],
        _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__,
    )
    assert len(compiled_for_the_island) == 2
    assert data_plane.input_port_has_data("brightness")

    payload = data_plane.read_from_input_port("brightness")
    assert payload is not None
    (brightness,) = struct.unpack("<d", bytes(payload))
    assert 0.0 <= brightness <= 1.0


def test_an_unlinked_input_with_no_default_is_refused_by_name():
    island = an_island_of_lfo_then_remap_feeding("brightness")
    island["nodes"][1]["links_into"] = []
    with pytest.raises(FunctionNodeDoesNotCompile, match="input `x` is linked to nothing"):
        feed_each_input_port_from_its_function_node_island(
            _engine.open_node_link_data_access_for_helper_process(),
            [island],
            _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__,
        )

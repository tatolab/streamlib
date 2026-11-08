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
import itertools
from dataclasses import dataclass, field
from typing import Any

import pytest

from tatolab.runtime import _engine
from tatolab.runtime._function_node_compilation import (
    FunctionNodeCompiledToItsEvaluateEntry,
    FunctionNodeDoesNotCompile,
    compile_function_node_to_its_evaluate_entry,
    feed_each_input_port_from_its_function_node_island,
)
from tatolab.runtime._function_node_run_as_its_helpers_processor import (
    the_processor_class_a_promoted_function_node_runs_as,
)
from tatolab.runtime._processor_hosting import construct_processor_instance
from tatolab.stream import State, Stateful, Time, node
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

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


@node.function
def square_root(x: float) -> float:
    if x < 0.0:
        raise ValueError("a negative number has no real square root")
    return math.sqrt(x)


@dataclass(frozen=True)
class Spread:
    centre: float
    width: float = 1.0


@node.function
def spread_around(x: float) -> Spread:
    return Spread(centre=x)


@dataclass(frozen=True)
class SpreadWithAFactoryWidth:
    centre: float = 0.0
    width: float = field(default_factory=lambda: 1.0)


@node.function
def spread_with_a_factory_width(x: float) -> SpreadWithAFactoryWidth:
    return SpreadWithAFactoryWidth(centre=x)


_SEEDS_HANDED_OUT = itertools.count(7)


@dataclass(frozen=True)
class SeededConfig:
    seed: int = field(default_factory=lambda: next(_SEEDS_HANDED_OUT))


@node.function
def offset_by_seed(x: float = 0.0, *, config: SeededConfig) -> float:
    return x + config.seed


@node.function
def count_from(start: int = 2**53 + 1) -> int:
    return start


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


def the_evaluate_entry_status(
    compiled: FunctionNodeCompiledToItsEvaluateEntry, inputs: "list[float]"
) -> int:
    input_values = (ctypes.c_double * max(len(inputs), 1))(*inputs)
    unused = (ctypes.c_double * 1)()
    output_values = (ctypes.c_double * len(compiled.output_ports))()
    return _EVALUATE_ENTRY_TYPE(compiled.evaluate_entry_address)(
        input_values, unused, unused, output_values, 0
    )


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


def test_a_body_that_raises_answers_the_refused_status():
    square_root_compiled = compiled(square_root)
    assert the_evaluate_entry_status(square_root_compiled, [4.0]) == 0
    assert the_evaluate_entry_status(square_root_compiled, [-1.0]) == -1


def test_a_body_may_omit_a_field_its_dataclass_defaults():
    assert call_the_evaluate_entry(compiled(spread_around), [3.0], [], [])[0] == [3.0, 1.0]


def test_a_factory_default_in_a_dataclass_the_body_builds_is_refused_by_name():
    with pytest.raises(FunctionNodeDoesNotCompile, match="`width` defaults through `default_factory`"):
        compiled(spread_with_a_factory_width)


def test_an_int_default_an_f64_cannot_hold_exactly_is_refused():
    with pytest.raises(FunctionNodeDoesNotCompile, match="only up to 2\\*\\*53"):
        compiled(count_from)


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
                "breadcrumb_number": 0,
                "import_path": f"{__name__}:lfo",
                "config": None,
                "links_into": [],
            },
            {
                "node": "remap-1",
                "breadcrumb_number": 1,
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


def test_a_config_field_its_class_does_not_declare_is_refused_by_name():
    import_path = f"{__name__}:offset_by_seed"
    with pytest.raises(FunctionNodeDoesNotCompile, match="config sets sede, which its config class"):
        feed_each_input_port_from_its_function_node_island(
            _engine.open_node_link_data_access_for_helper_process(),
            [an_island_of_one_node("level", import_path, {"sede": 3})],
            {import_path: offset_by_seed}.__getitem__,
        )


def test_a_config_field_default_factory_runs_for_each_node():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["first", "second"], [])
    import_path = f"{__name__}:offset_by_seed"
    feed_each_input_port_from_its_function_node_island(
        data_plane,
        [
            an_island_of_one_node("first", import_path, None, "seeded-1"),
            an_island_of_one_node("second", import_path, None, "seeded-2"),
        ],
        {import_path: offset_by_seed}.__getitem__,
    )
    seeds_read = []
    for port in ("first", "second"):
        payload = data_plane.read_from_input_port(port)
        assert payload is not None
        seeds_read.append(struct.unpack("<d", bytes(payload))[0])
    assert seeds_read[0] != seeds_read[1]


def test_a_stateful_node_feeding_two_inputs_of_one_reader_is_refused_by_name():
    with pytest.raises(FunctionNodeDoesNotCompile, match="stateful function node `lfo-1` feeds both"):
        feed_each_input_port_from_its_function_node_island(
            _engine.open_node_link_data_access_for_helper_process(),
            [
                an_island_of_lfo_then_remap_feeding("brightness"),
                an_island_of_lfo_then_remap_feeding("contrast"),
            ],
            _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__,
        )


class _InputsHandingOverControlValues:
    """`ctx.inputs` handing each named port the control value set for it, once."""

    def __init__(self) -> None:
        self.control_value_by_port: "dict[str, float]" = {}

    def read(self, port_name: str) -> "bytes | None":
        if port_name not in self.control_value_by_port:
            return None
        return struct.pack("<d", self.control_value_by_port.pop(port_name))


class _OutputsRecordingEachControlValueWritten:
    """`ctx.outputs` recording each Channels block written."""

    def __init__(self) -> None:
        self.blocks_written: "list[tuple[str, object, int, int]]" = []

    def write_channels_block(
        self, port_name: str, samples: Any, *, start_sample: int, sample_rate: int
    ) -> None:
        self.blocks_written.append((port_name, memoryview(samples).tolist(), start_sample, sample_rate))


class _ContextOfOneProcess:
    def __init__(self) -> None:
        self.inputs = _InputsHandingOverControlValues()
        self.outputs = _OutputsRecordingEachControlValueWritten()


def test_a_promoted_function_node_runs_as_a_process_writing_each_output_as_a_control_value():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    runner_class = the_processor_class_a_promoted_function_node_runs_as(
        amplify, f"{__name__}:amplify", "amplify-1", data_plane
    )
    assert [port["name"] for port in getattr(runner_class, NODE_DECLARATION_INPUT_PORTS_STAMP)] == ["x"]
    assert [port["name"] for port in getattr(runner_class, NODE_DECLARATION_OUTPUT_PORTS_STAMP)] == [
        "result"
    ]
    runner = construct_processor_instance(runner_class, {"gain": 3.0}, data_plane)
    ctx = _ContextOfOneProcess()

    runner.process(ctx)
    assert ctx.outputs.blocks_written == [], "an input with no pin value waits for its first value"

    ctx.inputs.control_value_by_port["x"] = 2.0
    runner.process(ctx)
    runner.process(ctx)
    assert ctx.outputs.blocks_written == [
        ("result", [[6.0]], 0, 0),
        ("result", [[6.0]], 1, 0),
    ], "each run writes one f64 at rate 0, the input keeping its newest value between reads"


def test_a_promoted_function_node_starts_from_its_pin_values():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    runner_class = the_processor_class_a_promoted_function_node_runs_as(
        tally, f"{__name__}:tally", "tally-1", data_plane
    )
    runner = construct_processor_instance(runner_class, None, data_plane)
    ctx = _ContextOfOneProcess()
    ctx.inputs.control_value_by_port["x"] = 4.0

    runner.process(ctx)

    assert ctx.outputs.blocks_written == [("result", [[7.0]], 0, 0)]


@node.function
def passes_its_process_input_through(process: float) -> float:
    return process


def test_a_promoted_function_node_with_a_port_named_like_a_runner_member_is_refused_by_name():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    with pytest.raises(FunctionNodeDoesNotCompile, match="its port 'process' is named like a member"):
        the_processor_class_a_promoted_function_node_runs_as(
            passes_its_process_input_through,
            f"{__name__}:passes_its_process_input_through",
            "passes-1",
            data_plane,
        )


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


def test_a_rust_function_node_is_read_from_its_declaration_and_never_compiled():
    rust_compiled = compiled(WeightedSumCountingItsCalls)
    assert rust_compiled.evaluate_entry_address == (
        WeightedSumCountingItsCalls.rust_function_node_declaration.evaluate_entry_address
    )
    assert rust_compiled.input_ports == ("x", "bias")
    assert rust_compiled.pin_defaults_by_input_port == {"bias": 0.25}
    assert rust_compiled.output_ports == ("result",)
    assert rust_compiled.config_fields == ("weight",)
    assert rust_compiled.config_field_defaults == {"weight": 2.0}
    assert rust_compiled.state_fields == ("calls",)
    assert rust_compiled.initial_state_values == (10.0,)
    assert rust_compiled.is_stateful
    assert not rust_compiled.body_loaded_from_numbas_cache
    assert call_the_evaluate_entry(rust_compiled, [3.0, 1.0], [2.0], [10.0]) == ([7.0], [11.0])


@pytest.mark.parametrize(
    ("major_offset", "minor_offset", "refusal"),
    [(1, 0, "another major"), (0, 1, "newer than")],
)
def test_a_rust_function_node_built_for_a_table_this_runtime_does_not_serve_is_refused(
    major_offset: int, minor_offset: int, refusal: str
):
    built_for_another_table = node.rust_function(
        a_rust_function_node_declaration(
            module_host_table_abi_major=_engine.MODULE_HOST_TABLE_ABI_MAJOR + major_offset,
            module_host_table_abi_minor=_engine.MODULE_HOST_TABLE_ABI_MINOR + minor_offset,
        )
    )
    with pytest.raises(FunctionNodeDoesNotCompile, match=refusal):
        compile_function_node_to_its_evaluate_entry(
            built_for_another_table, f"{__name__}:WeightedSumCountingItsCalls"
        )


def test_an_island_mixing_a_compiled_python_and_a_rust_function_node_feeds_its_port():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["weighted"], [])
    island = {
        "input_port": "weighted",
        "link_id": "L-mixed-island",
        "output": {"node": "weighted-1", "port": "result"},
        "nodes": [
            {
                "node": "lfo-1",
                "breadcrumb_number": 0,
                "import_path": f"{__name__}:lfo",
                "config": None,
                "links_into": [],
            },
            {
                "node": "weighted-1",
                "breadcrumb_number": 1,
                "import_path": f"{__name__}:WeightedSumCountingItsCalls",
                "config": {"weight": 4.0},
                "links_into": [{"port": "x", "from": {"node": "lfo-1", "port": "result"}}],
            },
        ],
    }
    function_nodes_by_import_path = {
        f"{__name__}:lfo": lfo,
        f"{__name__}:WeightedSumCountingItsCalls": WeightedSumCountingItsCalls,
    }
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], function_nodes_by_import_path.__getitem__
    )

    payload = data_plane.read_from_input_port("weighted")
    assert payload is not None
    (weighted,) = struct.unpack("<d", bytes(payload))
    assert weighted == pytest.approx(0.25), (
        "the LFO starts at 0, which the Rust node weights to 0 and moves by its `bias` pin"
    )

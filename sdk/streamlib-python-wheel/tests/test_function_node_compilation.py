# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A `@node.function` body compiled to its evaluate entry, and an island of them fed to a port.

Each node is called through its compiled entry the way the native evaluator
calls it, and an island is read off a helper's own data plane as the node it
feeds reads it: one f64 sample on one channel, evaluated at the read.
"""

from __future__ import annotations

import array
import ctypes
import math
import os
import struct
import itertools
from dataclasses import dataclass, field
from typing import Any, Literal

import pytest

from tatolab.runtime import _engine
from tatolab.runtime import _processor_interpreter_bootstrap as processor_interpreter_bootstrap
from tatolab.runtime._function_node_compilation import (
    FunctionNodeBodyRunAsPython,
    FunctionNodeCompiledToItsEvaluateEntry,
    FunctionNodeDoesNotCompile,
    FunctionNodeRunAsPythonInItsReader,
    UNREAD_MESSAGE_VALUE_TYPE,
    compile_function_node_to_its_evaluate_entry,
    feed_each_input_port_from_its_function_node_island,
    prepare_function_node_to_evaluate,
)
from tatolab.runtime._function_node_run_as_its_helpers_processor import (
    ran_compiled_by_node_id_of_the_promoted_function_node_a_processor_class_runs,
    the_processor_class_a_promoted_function_node_runs_as,
)
from tatolab.runtime._processor_hosting import apply_configuration, construct_processor_instance
from tatolab.stream import State, Stateful, Time, UnreadMessage, node
from tatolab.stream import _function_node_declaration
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
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_lfo_then_remap_feeding("brightness")],
        _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__,
    )
    assert len(fed.function_node_types_kept_alive) == 2
    assert fed.ran_compiled_by_node_id == {"lfo-1": True, "remap-1": True}
    assert data_plane.input_port_has_data("brightness")

    payload = data_plane.read_from_input_port("brightness")
    assert payload is not None
    (brightness,) = struct.unpack("<d", bytes(payload))
    assert 0.0 <= brightness <= 1.0


def test_an_island_with_an_input_no_value_reaches_holds_until_a_later_feed_gives_it_one():
    island = an_island_of_lfo_then_remap_feeding("brightness")
    linked_x = island["nodes"][1]["links_into"]
    island["nodes"][1]["links_into"] = []
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["brightness"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane, [island], _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__
    )
    assert fed.ran_compiled_by_node_id == {}, "a held island runs nothing"
    assert data_plane.read_from_input_port("brightness") is None

    island["nodes"][1]["links_into"] = linked_x
    fed.feed_an_input_port_from_its_island(island)
    assert data_plane.read_from_input_port("brightness") is not None

    island["nodes"][1]["links_into"] = []
    fed.feed_an_input_port_from_its_island(island)
    assert data_plane.read_from_input_port("brightness") is None, (
        "an island fed again with an input no value reaches stops feeding its port"
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


def _constructed_with_its_ports_declared(processor_class: Any, config: Any, data_plane: Any) -> Any:
    """`processor_class` built on `data_plane` with its ports declared first, as a helper's bootstrap builds it."""
    data_plane.declare_ports(
        [port["name"] for port in getattr(processor_class, NODE_DECLARATION_INPUT_PORTS_STAMP)],
        [port["name"] for port in getattr(processor_class, NODE_DECLARATION_OUTPUT_PORTS_STAMP)],
    )
    return construct_processor_instance(processor_class, config, data_plane)


class _InputsHandingOverRecords:
    """`ctx.inputs` handing each Records port attribute the record set for it, once."""

    def __init__(self) -> None:
        self.record_by_port: "dict[str, dict[str, Any]]" = {}

    def read(self, port: Any) -> "dict[str, Any] | None":
        return self.record_by_port.pop(port.port_name, None)


_links_into_promoted_nodes_opened = itertools.count()


class _ChannelsBlocksWrittenIntoAPromotedNode:
    """A source wired by a real link into each named number input of a promoted node's data plane."""

    def __init__(self, data_plane: Any, input_ports: "list[str]") -> None:
        self._source = _engine.open_node_link_data_access_for_helper_process()
        self._blocks_written = 0
        link_id_by_input_port = {
            input_port: f"L-into-{input_port}-{next(_links_into_promoted_nodes_opened)}"
            for input_port in input_ports
        }
        processor_interpreter_bootstrap.wire_link_data_access(
            data_plane,
            {
                "inputs": [
                    _wiring_of_one_link_end("input", input_port, link_id)
                    for input_port, link_id in link_id_by_input_port.items()
                ]
            },
        )
        processor_interpreter_bootstrap.wire_link_data_access(
            self._source,
            {
                "outputs": [
                    _wiring_of_one_link_end("output", f"{input_port}_to_the_node", link_id)
                    for input_port, link_id in link_id_by_input_port.items()
                ]
            },
        )
        self._source_outputs = _engine.open_runtime_context_full_access_for_helper_process(
            {}, self._source, "runtime-under-test", "source-under-test"
        ).outputs

    def write_control_value(self, input_port: str, control_value: float) -> None:
        """Write `control_value` as one f64 sample at rate 0, as an island or a pin hands it."""
        self.write_block(input_port, "d", [[control_value]], sample_rate=0)

    def write_block(
        self,
        input_port: str,
        sample_format: 'Literal["f", "d"]',
        samples: "list[list[float]]",
        *,
        sample_rate: int,
    ) -> None:
        """Write `samples`, `[channel][sample]` in `sample_format` (`"f"` or `"d"`), as one Channels block."""
        flat = array.array(sample_format, [sample for channel in samples for sample in channel])
        self._source_outputs.write_channels_block(
            f"{input_port}_to_the_node",
            memoryview(flat).cast("B").cast(sample_format, [len(samples), len(samples[0])]),
            start_sample=self._blocks_written,
            sample_rate=sample_rate,
        )
        self._blocks_written += 1

    def write_bytes(self, input_port: str, payload: bytes) -> None:
        """Write `payload` as a message of plain bytes, which carries no number."""
        self._source_outputs.write(f"{input_port}_to_the_node", payload)


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
        self.inputs = _InputsHandingOverRecords()
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
    runner = _constructed_with_its_ports_declared(runner_class, {"gain": 3.0}, data_plane)
    ctx = _ContextOfOneProcess()

    runner.process(ctx)
    assert ctx.outputs.blocks_written == [], "an input with no pin value waits for its first value"

    control_values = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["x"])
    control_values.write_control_value("x", 2.0)
    runner.process(ctx)
    runner.process(ctx)
    assert ctx.outputs.blocks_written == [
        ("result", [[6.0]], 0, 0),
        ("result", [[6.0]], 1, 0),
    ], "each run writes one f64 at rate 0, the input keeping its newest value between reads"


def test_a_promoted_function_node_reads_a_number_as_an_island_does_off_an_f32_block_of_any_shape():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    runner = _constructed_with_its_ports_declared(
        the_processor_class_a_promoted_function_node_runs_as(
            amplify, f"{__name__}:amplify", "amplify-1", data_plane
        ),
        {"gain": 2.0},
        data_plane,
    )
    control_values = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["x"])
    ctx = _ContextOfOneProcess()

    control_values.write_block("x", "f", [[0.25]], sample_rate=0)
    runner.process(ctx)
    control_values.write_block("x", "f", [[0.75, 0.5, 0.0], [-1.0, -0.5, 0.25]], sample_rate=48_000)
    runner.process(ctx)
    control_values.write_block("x", "d", [[1.5, 3.0]], sample_rate=48_000)
    runner.process(ctx)

    assert [block[1] for block in ctx.outputs.blocks_written] == [[[0.5]], [[1.5]], [[3.0]]], (
        "a number reads the first sample of an f32 or f64 block, whatever its channels and samples"
    )


def test_a_promoted_function_node_refuses_a_number_read_off_a_message_that_is_no_channels_block():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    runner = _constructed_with_its_ports_declared(
        the_processor_class_a_promoted_function_node_runs_as(
            amplify, f"{__name__}:amplify", "amplify-1", data_plane
        ),
        {"gain": 2.0},
        data_plane,
    )
    control_values = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["x"])
    control_values.write_bytes("x", struct.pack("<d", 4.0))

    with pytest.raises(ValueError, match=r"function node `amplify-1` reads a number .* is no Channels block"):
        runner.process(_ContextOfOneProcess())


@node.function
def gate_each_message(value: UnreadMessage, open: bool = True) -> "UnreadMessage | None":
    return value if open else None


@node.function
def hand_each_message_on_once_it_has_built_an_object(value: UnreadMessage) -> UnreadMessage:
    repr(object())
    return value


def _wiring_of_one_link_end(direction: str, port_name: str, link_id: str) -> "dict[str, Any]":
    """One entry of the envelope the engine's wiring path hands a helper."""
    channel_service_name = f"promoted{os.getpid()}_{link_id}/channel"
    notify_service_name = f"promoted{os.getpid()}_{link_id}_dest/notify"
    shared = {
        "name": port_name,
        "link_id": link_id,
        "channel_service_name": channel_service_name,
        "channel_service_creation_depth": 16,
        "max_subscribers": 2,
        "notify_max_notifiers": 1,
    }
    if direction == "output":
        return {
            **shared,
            "dest_notify_service_name": notify_service_name,
            "expected_payload_bytes": 1024,
            "max_payload_bytes_per_channel": 1 << 20,
            "output_port_wiring_generation": 1,
        }
    return {
        **shared,
        "inbound_link_name": channel_service_name,
        "notify_service_name": notify_service_name,
        "read_mode": "read_next_in_order",
        "input_port_ring_depth": 16,
        "loss_count_slot": 0,
        "wiring_generation": 1,
    }


@pytest.mark.parametrize(
    ("function_node", "is_compiled"),
    [
        (gate_each_message, True),
        (hand_each_message_on_once_it_has_built_an_object, False),
        ("RustGate", True),
    ],
)
def test_a_promoted_node_hands_each_message_on_unread_as_it_arrived(
    function_node: Any, is_compiled: bool
):
    if function_node == "RustGate":
        function_node = RustGate
    import_path = f"{__name__}:{getattr(function_node, '__name__', 'RustGate')}"
    assert (
        isinstance(
            prepare_function_node_to_evaluate(function_node, import_path),
            FunctionNodeCompiledToItsEvaluateEntry,
        )
        is is_compiled
    )
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    runner_class = the_processor_class_a_promoted_function_node_runs_as(
        function_node, import_path, "gate-1", data_plane
    )
    input_families = {
        port["name"]: port["family"] for port in getattr(runner_class, NODE_DECLARATION_INPUT_PORTS_STAMP)
    }
    assert input_families["value"]["also_takes_families"] == ["array", "channels", "packets", "records"]
    runner = _constructed_with_its_ports_declared(runner_class, None, data_plane)
    destination = _engine.open_node_link_data_access_for_helper_process()
    processor_interpreter_bootstrap.wire_link_data_access(
        destination, {"inputs": [_wiring_of_one_link_end("input", "read", f"L-out-of-{import_path}")]}
    )
    processor_interpreter_bootstrap.wire_link_data_access(
        data_plane,
        {
            "inputs": [_wiring_of_one_link_end("input", "value", f"L-into-{import_path}")],
            "outputs": [_wiring_of_one_link_end("output", "result", f"L-out-of-{import_path}")],
        },
    )
    source = _engine.open_node_link_data_access_for_helper_process()
    processor_interpreter_bootstrap.wire_link_data_access(
        source, {"outputs": [_wiring_of_one_link_end("output", "frames", f"L-into-{import_path}")]}
    )
    ctx = _ContextOfOneProcess()

    runner.process(ctx)
    assert destination.read_from_input_port("read") is None, "no message arrived, so none is handed on"

    source.write_to_output_port("frames", b"first", timestamp_ns=11)
    runner.process(ctx)
    payload, timestamp_ns = destination.read_from_input_port_with_timestamp("read")
    assert payload is not None
    assert (bytes(payload), timestamp_ns) == (b"first", 11)
    runner.process(ctx)
    assert destination.read_from_input_port("read") is None, "each message is handed on once"
    if function_node is not gate_each_message:
        return

    control_values = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["open"])
    control_values.write_control_value("open", 0.0)
    source.write_to_output_port("frames", b"second", timestamp_ns=12)
    runner.process(ctx)
    assert destination.read_from_input_port("read") is None, "a closed gate hands out nothing"


def test_the_pool_hands_a_message_its_island_hands_on_as_it_arrived_in_its_family():
    from tatolab.runtime._function_node_pool import the_processor_class_the_function_node_pool_runs_as

    data_plane = _engine.open_node_link_data_access_for_helper_process()
    pool_class = the_processor_class_the_function_node_pool_runs_as(
        {
            "carries": [
                {
                    "input_port": "island_0",
                    "output_port": "island_0_output",
                    "family": "message",
                    "message_family": "image",
                }
            ]
        },
        data_plane,
    )
    assert [port["family"]["family"] for port in getattr(pool_class, NODE_DECLARATION_OUTPUT_PORTS_STAMP)] == [
        "image"
    ]
    pool = _constructed_with_its_ports_declared(pool_class, None, data_plane)
    destination = _engine.open_node_link_data_access_for_helper_process()
    processor_interpreter_bootstrap.wire_link_data_access(
        destination, {"inputs": [_wiring_of_one_link_end("input", "read", "L-out-of-the-pool")]}
    )
    processor_interpreter_bootstrap.wire_link_data_access(
        data_plane,
        {
            "inputs": [_wiring_of_one_link_end("input", "island_0", "L-into-the-pool")],
            "outputs": [_wiring_of_one_link_end("output", "island_0_output", "L-out-of-the-pool")],
        },
    )
    source = _engine.open_node_link_data_access_for_helper_process()
    processor_interpreter_bootstrap.wire_link_data_access(
        source, {"outputs": [_wiring_of_one_link_end("output", "frames", "L-into-the-pool")]}
    )

    source.write_to_output_port("frames", b"a frame's descriptor", timestamp_ns=21)
    pool.process(_ContextOfOneProcess())

    payload, timestamp_ns = destination.read_from_input_port_with_timestamp("read")
    assert payload is not None
    assert (bytes(payload), timestamp_ns) == (b"a frame's descriptor", 21)


def test_a_promoted_function_node_starts_from_its_pin_values():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    runner_class = the_processor_class_a_promoted_function_node_runs_as(
        tally, f"{__name__}:tally", "tally-1", data_plane
    )
    runner = _constructed_with_its_ports_declared(runner_class, None, data_plane)
    ctx = _ContextOfOneProcess()
    control_values = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["x"])
    control_values.write_control_value("x", 4.0)

    runner.process(ctx)

    assert ctx.outputs.blocks_written == [("result", [[7.0]], 0, 0)]


def test_a_promoted_function_node_reads_a_live_config_update_at_its_next_call():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    assert isinstance(
        prepare_function_node_to_evaluate(amplify, f"{__name__}:amplify"),
        FunctionNodeCompiledToItsEvaluateEntry,
    )
    runner_class = the_processor_class_a_promoted_function_node_runs_as(
        amplify, f"{__name__}:amplify", "amplify-1", data_plane
    )
    runner = _constructed_with_its_ports_declared(runner_class, {"gain": 3.0}, data_plane)
    ctx = _ContextOfOneProcess()
    control_values = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["x"])
    control_values.write_control_value("x", 2.0)
    runner.process(ctx)

    apply_configuration(runner, {"gain": 5.0, "inverted": True})
    runner.process(ctx)

    assert ctx.outputs.blocks_written == [
        ("result", [[6.0]], 0, 0),
        ("result", [[-10.0]], 1, 0),
    ], "a compiled node's float and bool config fields reach the call after the update"


@dataclass(frozen=True)
class LabelledGainConfig:
    gain: float = 2.0
    label: str = "gain"


@node.function
def amplify_while_labelled(x: float, config: LabelledGainConfig) -> float:
    return x * config.gain if config.label else 0.0


def test_a_promoted_function_node_run_as_python_reads_a_live_config_update_at_its_next_call():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    import_path = f"{__name__}:amplify_while_labelled"
    assert isinstance(
        prepare_function_node_to_evaluate(amplify_while_labelled, import_path),
        FunctionNodeRunAsPythonInItsReader,
    ), "a `str` config field does not compile"
    runner_class = the_processor_class_a_promoted_function_node_runs_as(
        amplify_while_labelled, import_path, "amplify-1", data_plane
    )
    runner = _constructed_with_its_ports_declared(runner_class, {"gain": 3.0}, data_plane)
    ctx = _ContextOfOneProcess()
    control_values = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["x"])
    control_values.write_control_value("x", 2.0)
    runner.process(ctx)

    apply_configuration(runner, {"gain": 0.5, "label": "half"})
    runner.process(ctx)
    apply_configuration(runner, {"gain": 0.5, "label": ""})
    runner.process(ctx)

    assert ctx.outputs.blocks_written == [
        ("result", [[6.0]], 0, 0),
        ("result", [[1.0]], 1, 0),
        ("result", [[0.0]], 2, 0),
    ], "a body run as Python reads each field of the config it was last handed"


@dataclass(frozen=True)
class CallStepConfig:
    step: float = 1.0


@dataclass(frozen=True)
class CallsCountedByTheirStep:
    total: float = 0.0


@node.function
def counts_its_calls_by_its_configured_step(
    x: float, *, config: CallStepConfig, previous: State[CallsCountedByTheirStep]
) -> Stateful[float, CallsCountedByTheirStep]:
    total = previous.total + config.step
    return total, CallsCountedByTheirStep(total)


def test_a_promoted_stateful_function_node_keeps_its_state_through_a_live_config_update():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    runner_class = the_processor_class_a_promoted_function_node_runs_as(
        counts_its_calls_by_its_configured_step,
        f"{__name__}:counts_its_calls_by_its_configured_step",
        "counter-1",
        data_plane,
    )
    runner = _constructed_with_its_ports_declared(runner_class, None, data_plane)
    ctx = _ContextOfOneProcess()
    control_values = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["x"])
    control_values.write_control_value("x", 0.0)
    runner.process(ctx)
    runner.process(ctx)

    apply_configuration(runner, {"step": 10.0})
    runner.process(ctx)

    assert [block[1] for block in ctx.outputs.blocks_written] == [[[1.0]], [[2.0]], [[12.0]]], (
        "the count carries on from its state, stepping by the step it was handed"
    )


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
                "node": "tally-1",
                "breadcrumb_number": 1,
                "import_path": f"{__name__}:tally",
                "config": None,
                "links_into": [{"port": "x", "from": {"node": "lfo-1", "port": "result"}}],
            },
            {
                "node": "weighted-1",
                "breadcrumb_number": 2,
                "import_path": f"{__name__}:WeightedSumCountingItsCalls",
                "config": {"weight": 4.0},
                "links_into": [{"port": "x", "from": {"node": "tally-1", "port": "result"}}],
            },
        ],
    }
    function_nodes_by_import_path = {
        f"{__name__}:lfo": lfo,
        f"{__name__}:tally": tally,
        f"{__name__}:WeightedSumCountingItsCalls": WeightedSumCountingItsCalls,
    }
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], function_nodes_by_import_path.__getitem__
    )

    payload = data_plane.read_from_input_port("weighted")
    assert payload is not None
    (weighted,) = struct.unpack("<d", bytes(payload))
    assert weighted == pytest.approx(3 * 4.0 + 0.25), (
        "`tally` hands the Rust node 3, which it weights by its config's 4 and moves by its "
        "`bias` pin's 0.25"
    )


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


@node.function
def python_gate(value: UnreadMessage, open: bool = True) -> "UnreadMessage | None":
    return value if open else None


def test_a_rust_gate_carries_its_message_and_its_handed_out_flag_as_a_compiled_python_gate_does():
    rust_compiled = compiled(RustGate)
    python_compiled = compiled(python_gate)
    assert rust_compiled.input_value_types == python_compiled.input_value_types == (
        "unread_message",
        "bool",
    )
    assert rust_compiled.how_its_values_are_carried == python_compiled.how_its_values_are_carried
    assert rust_compiled.how_its_values_are_carried == {
        "inputs_that_take_none": [],
        "inputs_carrying_an_unread_message": [0],
        "outputs_carrying_an_unread_message": [0],
        "outputs_that_may_hand_out_nothing": [0],
    }


@_EVALUATE_ENTRY_TYPE
def _rust_reciprocal_evaluate_entry(
    input_values: Any, config_values: Any, state_values: Any, output_values: Any, now_ns: int
) -> int:
    hands_one_out = input_values[0] != 0.0
    output_values[0] = 1.0 / input_values[0] if hands_one_out else 0.0
    output_values[1] = 1.0 if hands_one_out else 0.0
    return 0


RustReciprocal = node.rust_function(
    a_rust_function_node_declaration(
        short_name="RustReciprocal",
        description="The reciprocal of `x`, or nothing at zero.",
        input_pins=[{"port": "x", "value_type": "float", "pin_default": 0.0, "takes_none": False}],
        output_ports=[{"name": "result", "value_type": "float", "may_hand_out_nothing": True}],
        config_fields=[],
        state_fields=[],
        state_parameter=None,
        config_parameter=None,
        evaluate_entry_address=ctypes.cast(_rust_reciprocal_evaluate_entry, ctypes.c_void_p).value,
    )
)


@pytest.mark.parametrize(("x", "read"), [(4.0, 0.25), (0.0, None)])
def test_an_island_output_a_rust_node_hands_out_nothing_on_reads_nothing(x: float, read: Any):
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["reciprocal"], [])
    island = {
        "input_port": "reciprocal",
        "link_id": f"L-rust-reciprocal-of-{x}",
        "output": {"node": "reciprocal-1", "port": "result"},
        "nodes": [
            {
                "node": "reciprocal-1",
                "breadcrumb_number": 0,
                "import_path": f"{__name__}:RustReciprocal",
                "config": None,
                "links_into": [],
                "pins": {"x": x},
            },
        ],
    }
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], {f"{__name__}:RustReciprocal": RustReciprocal}.__getitem__
    )

    payload = data_plane.read_from_input_port("reciprocal")
    if read is None:
        assert payload is None, "an output that hands out nothing yields no message"
    else:
        assert payload is not None
        assert struct.unpack("<d", bytes(payload)) == (pytest.approx(read),)


@node.function
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


@node.function
def remember_each_call_by_a_letter(
    letter_count: float = 1.0, *, previous: State[LabelsSeen]
) -> Stateful[float, LabelsSeen]:
    labels = previous.labels + "x" * int(letter_count)
    return float(len(labels)), LabelsSeen(labels)


@dataclass(frozen=True)
class CallsSoFar:
    calls: int = 0


@node.function
def count_calls_by_spelling_them(step: float = 1.0, *, previous: State[CallsSoFar]) -> Stateful[int, CallsSoFar]:
    calls = previous.calls + int(step) + len(repr(object())) * 0
    return calls, CallsSoFar(calls)


@node.function
def refuse_a_negative_spelling(x: float) -> float:
    if x < 0.0:
        raise ValueError(f"{x!r} is negative")
    return float(len(repr(x)))


@node.function
def take_an_optional_level(level: "float | None" = None) -> float:
    return 0.0 if level is None else level


def run_as_python(function_node: Any) -> FunctionNodeRunAsPythonInItsReader:
    prepared = prepare_function_node_to_evaluate(function_node, f"{__name__}:{function_node.__qualname__}")
    assert isinstance(prepared, FunctionNodeRunAsPythonInItsReader), prepared
    return prepared


_FUNCTION_NODES_RUN_AS_PYTHON_BY_IMPORT_PATH = {
    f"{__name__}:{function_node.__qualname__}": function_node
    for function_node in (measure_its_own_spelling, label_the_level, count_the_label_letters, remap)
}


def test_a_body_that_does_not_compile_runs_as_python_saying_why():
    prepared = run_as_python(build_an_object)
    assert "body does not compile" in prepared.why_it_does_not_compile
    assert prepared.input_ports == ("x",)
    assert prepared.ports_carrying_records == frozenset()


def test_a_body_taking_or_giving_a_dict_runs_as_python():
    assert run_as_python(label_the_level).ports_carrying_records == frozenset({"result"})
    assert run_as_python(count_the_label_letters).ports_carrying_records == frozenset({"record"})


def test_a_signature_neither_compiled_nor_python_carries_is_refused_by_name():
    with pytest.raises(FunctionNodeDoesNotCompile, match="`level` is optional"):
        prepare_function_node_to_evaluate(take_an_optional_level, f"{__name__}:take_an_optional_level")


def test_a_body_run_as_python_answers_each_output_as_a_float_or_a_dict():
    assert FunctionNodeBodyRunAsPython(run_as_python(measure_its_own_spelling), "m", None).evaluate(
        [12.0], 0
    ) == [4.0]
    assert FunctionNodeBodyRunAsPython(run_as_python(label_the_level), "l", None).evaluate([0.25], 0) == [
        {"label": "low"}
    ]
    letters = FunctionNodeBodyRunAsPython(run_as_python(count_the_label_letters), "c", None).evaluate(
        [{"label": "high"}], 0
    )
    assert letters == [4.0] and isinstance(letters[0], float)


def test_the_runtime_spells_the_unread_message_value_type_as_tatolab_stream_does():
    assert UNREAD_MESSAGE_VALUE_TYPE == _function_node_declaration.UNREAD_MESSAGE_VALUE_TYPE


@node.function
def hand_on_once_it_has_built_an_object(value: UnreadMessage) -> UnreadMessage:
    repr(object())
    return value


def test_a_body_run_as_python_hands_on_unread_a_number_or_a_dict_another_node_of_its_island_gave():
    handing_on = FunctionNodeBodyRunAsPython(run_as_python(hand_on_once_it_has_built_an_object), "h", None)
    level = 0.25
    record = {"label": "low"}
    assert handing_on.evaluate([level], 0)[0] is level
    assert handing_on.evaluate([record], 1)[0] is record


@dataclass(frozen=True)
class FirstMessageHeld:
    message: UnreadMessage | None = None


@node.function
def hold_the_first_message_handed(
    value: UnreadMessage | None = None, *, previous: State[FirstMessageHeld]
) -> Stateful[UnreadMessage | None, FirstMessageHeld]:
    if previous.message is None and value is not None:
        return value, FirstMessageHeld(value)
    return previous.message, previous


def test_a_body_run_as_python_hands_on_a_number_or_a_dict_its_state_held_from_an_earlier_call():
    holding = FunctionNodeBodyRunAsPython(run_as_python(hold_the_first_message_handed), "h", None)
    record = {"label": "low"}
    assert holding.evaluate([record], 0)[0] is record
    assert holding.evaluate([None], 1)[0] == record
    assert holding.evaluate([{"label": "high"}], 2)[0] == record


def test_a_body_run_as_python_keeps_a_state_of_any_record_between_calls():
    remembering = FunctionNodeBodyRunAsPython(run_as_python(remember_each_call_by_a_letter), "r", None)
    assert remembering.evaluate([1.0], 0) == [1.0]
    assert remembering.evaluate([2.0], 1) == [3.0]
    assert run_as_python(remember_each_call_by_a_letter).initial_state_values == (), (
        "a state record holding a string is not carried as f64s on the board"
    )


def test_a_body_run_as_python_with_a_state_of_numbers_carries_it_as_f64s_and_resumes_from_them():
    prepared = run_as_python(count_calls_by_spelling_them)
    assert prepared.initial_state_values == (0.0,)
    counting = FunctionNodeBodyRunAsPython(prepared, "counter", None)
    counting.resume_from_state_values([41.0])
    assert counting.evaluate([1.0], 0) == [42.0]
    assert counting.state_values() == [42.0]


def test_a_body_run_as_python_that_raises_keeps_its_state():
    refusing = FunctionNodeBodyRunAsPython(run_as_python(refuse_a_negative_spelling), "refuses", None)
    with pytest.raises(ValueError, match="is negative"):
        refusing.evaluate([-1.0], 0)
    assert refusing.evaluate([1.5], 0) == [3.0]


@node.function
def spell_a_positional_level(level: float = 0.75, /) -> float:
    return level + len(repr(object())) * 0


@node.function
def take_the_label_out(record: dict) -> int:
    return len(record.pop("label"))


@dataclass
class LabelsKept:
    labels: list = field(default_factory=list)


@node.function
def keep_a_label_unless_negative(x: float = 1.0, *, previous: State[LabelsKept]) -> Stateful[int, LabelsKept]:
    previous.labels.append(repr(x))
    if x < 0.0:
        raise ValueError(f"{x!r} is negative")
    return len(previous.labels), previous


@node.function
def jump_the_count_past_an_f64(step: float = 1.0, *, previous: State[CallsSoFar]) -> Stateful[float, CallsSoFar]:
    return step + len(repr(object())) * 0, CallsSoFar(previous.calls + 2**53 + 1)


@dataclass(frozen=True)
class LabelPolicy:
    prefix: str = ""


@dataclass(frozen=True)
class LabelConfig:
    policy: LabelPolicy = field(default_factory=LabelPolicy)


@node.function
def label_with_a_prefix(level: float = 0.5, *, config: LabelConfig) -> dict:
    return {"label": f"{config.policy.prefix}{level}"}


def test_a_body_run_as_python_takes_a_positional_only_input_positionally():
    assert FunctionNodeBodyRunAsPython(run_as_python(spell_a_positional_level), "p", None).evaluate(
        [0.25], 0
    ) == [0.25]


def test_a_body_run_as_python_that_changes_its_dict_input_leaves_the_one_it_was_handed():
    record = {"label": "high"}
    taking = FunctionNodeBodyRunAsPython(run_as_python(take_the_label_out), "t", None)
    assert taking.evaluate([record], 0) == [4.0]
    assert record == {"label": "high"}, "a sibling reading the same dict still finds its label"


def test_a_body_run_as_python_that_changes_its_state_and_raises_keeps_the_state_it_had():
    keeping = FunctionNodeBodyRunAsPython(run_as_python(keep_a_label_unless_negative), "k", None)
    assert keeping.evaluate([1.0], 0) == [1.0]
    with pytest.raises(ValueError, match="is negative"):
        keeping.evaluate([-1.0], 1)
    assert keeping.evaluate([2.0], 2) == [2.0], "the call that raised appended nothing it kept"


def test_a_body_run_as_python_keeps_no_state_its_stream_cannot_carry():
    jumping = FunctionNodeBodyRunAsPython(run_as_python(jump_the_count_past_an_f64), "counter", None)
    with pytest.raises(FunctionNodeDoesNotCompile, match="`counter`'s state `calls`"):
        jumping.evaluate([1.0], 0)
    assert jumping.state_values() == [0.0]


def test_a_body_run_as_python_that_takes_no_config_refuses_one_naming_its_fields():
    with pytest.raises(FunctionNodeDoesNotCompile, match="`m`'s config sets rate, and it takes no config"):
        FunctionNodeBodyRunAsPython(run_as_python(measure_its_own_spelling), "m", {"rate": 1})


def test_a_body_run_as_python_reads_a_nested_dataclass_of_its_config():
    labelling = FunctionNodeBodyRunAsPython(
        run_as_python(label_with_a_prefix), "l", {"policy": {"prefix": "at "}}
    )
    assert labelling.evaluate([0.5], 0) == [{"label": "at 0.5"}]


def an_island_of_label_then_count_feeding(input_port: str, output_node: str) -> "dict[str, Any]":
    return {
        "input_port": input_port,
        "link_id": f"L-{input_port}",
        "output": {"node": output_node, "port": "result"},
        "nodes": [
            {
                "node": "label-1",
                "breadcrumb_number": 0,
                "import_path": f"{__name__}:label_the_level",
                "config": None,
                "links_into": [],
            },
            {
                "node": "count-1",
                "breadcrumb_number": 1,
                "import_path": f"{__name__}:count_the_label_letters",
                "config": None,
                "links_into": [{"port": "record", "from": {"node": "label-1", "port": "result"}}],
            },
        ],
    }


def test_an_island_runs_a_body_that_does_not_compile_as_python_inside_the_read():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["spelled"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_one_node("spelled", f"{__name__}:measure_its_own_spelling", None, "spelling-1")],
        _FUNCTION_NODES_RUN_AS_PYTHON_BY_IMPORT_PATH.__getitem__,
    )
    assert fed.ran_compiled_by_node_id == {"spelling-1": False}
    payload = data_plane.read_from_input_port("spelled")
    assert payload is not None
    assert struct.unpack("<d", bytes(payload)) == (3.0,)


def test_a_dict_passes_between_nodes_run_as_python_and_reaches_its_port_as_a_records_message():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["letters", "label"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [
            an_island_of_label_then_count_feeding("letters", "count-1"),
            an_island_of_label_then_count_feeding("label", "label-1"),
        ],
        _FUNCTION_NODES_RUN_AS_PYTHON_BY_IMPORT_PATH.__getitem__,
    )
    assert fed.ran_compiled_by_node_id == {"label-1": False, "count-1": False}

    letters = data_plane.read_from_input_port("letters")
    assert letters is not None
    assert struct.unpack("<d", bytes(letters)) == (4.0,)
    label = data_plane.read_from_input_port("label")
    assert label is not None
    assert bytes(label) == b"\x81\xa5label\xa4high", "one msgpack map, as a Records message carries it"


class _OutputsRecordingEachWrite(_OutputsRecordingEachControlValueWritten):
    """`ctx.outputs` recording each Channels block, and each Records write by its port."""

    def __init__(self) -> None:
        super().__init__()
        self.records_written: "list[tuple[str, object]]" = []

    def write(self, port: Any, payload: Any) -> None:
        self.records_written.append((port.port_name, payload))


def test_a_promoted_function_node_reports_whether_its_own_helper_runs_it_compiled():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    compiled = the_processor_class_a_promoted_function_node_runs_as(
        amplify, f"{__name__}:amplify", "amplify-1", data_plane
    )
    run_as_python = the_processor_class_a_promoted_function_node_runs_as(
        label_the_level, f"{__name__}:label_the_level", "label-1", data_plane
    )

    assert ran_compiled_by_node_id_of_the_promoted_function_node_a_processor_class_runs(compiled) == {
        "amplify-1": True
    }
    assert ran_compiled_by_node_id_of_the_promoted_function_node_a_processor_class_runs(
        run_as_python
    ) == {"label-1": False}
    assert ran_compiled_by_node_id_of_the_promoted_function_node_a_processor_class_runs(Phase) == {}, (
        "a class that runs no promoted function node reports none"
    )


def test_a_promoted_function_node_run_as_python_reads_and_writes_records():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    labelling = _constructed_with_its_ports_declared(
        the_processor_class_a_promoted_function_node_runs_as(
            label_the_level, f"{__name__}:label_the_level", "label-1", data_plane
        ),
        None,
        data_plane,
    )
    ctx = _ContextOfOneProcess()
    ctx.outputs = _OutputsRecordingEachWrite()
    labelling.process(ctx)
    assert ctx.outputs.records_written == [("result", {"label": "high"})], (
        "the input keeps its pin value until one arrives"
    )

    counting = _constructed_with_its_ports_declared(
        the_processor_class_a_promoted_function_node_runs_as(
            count_the_label_letters, f"{__name__}:count_the_label_letters", "count-1", data_plane
        ),
        None,
        data_plane,
    )
    ctx = _ContextOfOneProcess()
    counting.process(ctx)
    assert ctx.outputs.blocks_written == [], "an input with no pin value waits for its first record"
    ctx.inputs.record_by_port["record"] = {"label": "low"}
    counting.process(ctx)
    assert ctx.outputs.blocks_written == [("result", [[3.0]], 0, 0)]


@dataclass(frozen=True)
class EvaluationsSoFar:
    evaluations: float = 0.0


@node.function
def count_each_evaluation(
    step: float = 1.0, *, previous: State[EvaluationsSoFar]
) -> Stateful[float, EvaluationsSoFar]:
    evaluations = previous.evaluations + step
    return evaluations, EvaluationsSoFar(evaluations)


_FUNCTION_NODES_FED_AGAIN_BY_IMPORT_PATH = {
    f"{__name__}:{function_node.__qualname__}": function_node
    for function_node in (count_each_evaluation, remember_each_call_by_a_letter, remap)
}


def an_island_of_the_counter_feeding(input_port: str, *, then_remap_it: bool) -> "dict[str, Any]":
    counter = {
        "node": "counter-1",
        "breadcrumb_number": 0,
        "import_path": f"{__name__}:count_each_evaluation",
        "config": None,
        "links_into": [],
    }
    if not then_remap_it:
        return {
            "input_port": input_port,
            "link_id": f"L-{input_port}",
            "output": {"node": "counter-1", "port": "result"},
            "nodes": [counter],
        }
    return {
        "input_port": input_port,
        "link_id": f"L-{input_port}",
        "output": {"node": "remap-1", "port": "result"},
        "nodes": [
            counter,
            {
                "node": "remap-1",
                "breadcrumb_number": 1,
                "import_path": f"{__name__}:remap",
                "config": None,
                "links_into": [{"port": "x", "from": {"node": "counter-1", "port": "result"}}],
            },
        ],
    }


def the_value_read_from(data_plane: Any, input_port: str) -> float:
    payload = data_plane.read_from_input_port(input_port)
    assert payload is not None
    return struct.unpack("<d", bytes(payload))[0]


def test_an_island_fed_again_to_its_port_carries_each_compiled_stateful_nodes_state_on():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["count"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_the_counter_feeding("count", then_remap_it=False)],
        _FUNCTION_NODES_FED_AGAIN_BY_IMPORT_PATH.__getitem__,
    )
    assert fed.ran_compiled_by_node_id == {"counter-1": True}
    assert the_value_read_from(data_plane, "count") == 1.0
    assert the_value_read_from(data_plane, "count") == 2.0

    fed.feed_an_input_port_from_its_island(an_island_of_the_counter_feeding("count", then_remap_it=True))

    assert fed.ran_compiled_by_node_id == {"counter-1": True, "remap-1": True}
    assert the_value_read_from(data_plane, "count") == 3.0, "the counter resumes, read through remap"


def test_an_island_fed_again_keeps_the_body_of_each_node_run_as_python_and_its_state():
    import_path = f"{__name__}:remember_each_call_by_a_letter"
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["letters"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_one_node("letters", import_path, None, "letters-1")],
        _FUNCTION_NODES_FED_AGAIN_BY_IMPORT_PATH.__getitem__,
    )
    assert fed.ran_compiled_by_node_id == {"letters-1": False}
    assert the_value_read_from(data_plane, "letters") == 1.0

    fed.feed_an_input_port_from_its_island(an_island_of_one_node("letters", import_path, None, "letters-1"))

    assert the_value_read_from(data_plane, "letters") == 2.0, "a state of any record carries on"


def test_a_port_an_island_stops_feeding_reads_nothing_and_names_no_node():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["count"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_the_counter_feeding("count", then_remap_it=False)],
        _FUNCTION_NODES_FED_AGAIN_BY_IMPORT_PATH.__getitem__,
    )

    assert fed.stop_feeding_an_input_port("count") is True

    assert fed.ran_compiled_by_node_id == {}
    assert data_plane.read_from_input_port("count") is None
    assert fed.stop_feeding_an_input_port("count") is False


def test_a_live_feed_of_a_stateful_node_another_port_reads_is_refused_and_leaves_both_ports_as_they_were():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["count", "also_count"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_the_counter_feeding("count", then_remap_it=False)],
        _FUNCTION_NODES_FED_AGAIN_BY_IMPORT_PATH.__getitem__,
    )

    with pytest.raises(FunctionNodeDoesNotCompile, match="stateful function node `counter-1` feeds both"):
        fed.feed_an_input_port_from_its_island(
            an_island_of_the_counter_feeding("also_count", then_remap_it=False)
        )

    assert fed.ran_compiled_by_node_id == {"counter-1": True}
    assert the_value_read_from(data_plane, "count") == 1.0
    assert data_plane.read_from_input_port("also_count") is None


def test_an_unlinked_input_takes_its_pin_value_before_its_default():
    import_path = f"{__name__}:remap"
    island = an_island_of_one_node("level", import_path, None, "remap-1")
    island["nodes"][0]["pins"] = {"x": 0.5, "high": 4.0}
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["level"], [])
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], {import_path: remap}.__getitem__
    )
    assert the_value_read_from(data_plane, "level") == 2.0, "x pinned to 0.5 over 0..4"


def test_an_island_node_input_with_no_link_pin_or_default_holds_its_port():
    import_path = f"{__name__}:remap"
    island = an_island_of_one_node("level", import_path, None, "remap-1")
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["level"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane, [island], {import_path: remap}.__getitem__
    )
    assert data_plane.read_from_input_port("level") is None
    island["nodes"][0]["pins"] = {"x": 0.5, "high": 4.0}
    fed.feed_an_input_port_from_its_island(island)
    assert the_value_read_from(data_plane, "level") == 2.0


def test_a_pin_value_that_is_not_a_number_is_refused_by_name():
    island = an_island_of_one_node("level", f"{__name__}:remap", None, "remap-1")
    island["nodes"][0]["pins"] = {"x": "half"}
    with pytest.raises(FunctionNodeDoesNotCompile, match="pin on input `x` is 'half'"):
        feed_each_input_port_from_its_function_node_island(
            _engine.open_node_link_data_access_for_helper_process(),
            [island],
            _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__,
        )


@node.function
def pass_the_value_on(value: float = 0.0) -> float:
    return value


@node.function
def add_a_step(x: float, step: float = 1.0) -> float:
    return x + step


def test_a_node_reading_a_later_node_takes_its_output_from_the_last_evaluation():
    """`pass_the_value_on -> add_a_step -> pass_the_value_on`, a loop whose link back the
    island lists first: that node reads what `add_a_step` gave last time, so each read
    counts one further."""
    pass_on_path = f"{__name__}:pass_the_value_on"
    add_path = f"{__name__}:add_a_step"
    island = {
        "input_port": "count",
        "link_id": "L-count",
        "output": {"node": "add-1", "port": "result"},
        "nodes": [
            {
                "node": "pass-on-1",
                "breadcrumb_number": 0,
                "import_path": pass_on_path,
                "config": None,
                "links_into": [{"port": "value", "from": {"node": "add-1", "port": "result"}}],
            },
            {
                "node": "add-1",
                "breadcrumb_number": 1,
                "import_path": add_path,
                "config": None,
                "links_into": [{"port": "x", "from": {"node": "pass-on-1", "port": "result"}}],
            },
        ],
    }
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["count"], [])
    feed_each_input_port_from_its_function_node_island(
        data_plane,
        [island],
        {pass_on_path: pass_the_value_on, add_path: add_a_step}.__getitem__,
    )
    assert [the_value_read_from(data_plane, "count") for _ in range(3)] == [1.0, 2.0, 3.0]


def test_a_node_linked_into_itself_reads_its_own_output_from_the_last_evaluation():
    add_path = f"{__name__}:add_a_step"
    island = {
        "input_port": "count",
        "link_id": "L-count",
        "output": {"node": "add-1", "port": "result"},
        "nodes": [
            {
                "node": "add-1",
                "breadcrumb_number": 0,
                "import_path": add_path,
                "config": None,
                "links_into": [{"port": "x", "from": {"node": "add-1", "port": "result"}}],
            },
        ],
    }
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["count"], [])
    feed_each_input_port_from_its_function_node_island(
        data_plane,
        [island],
        {add_path: add_a_step}.__getitem__,
    )
    assert [the_value_read_from(data_plane, "count") for _ in range(3)] == [1.0, 2.0, 3.0]


@dataclass(frozen=True)
class SpelledGainConfig:
    gain: float = 2.0


@node.function
def spell_the_scaled_value(x: float = 4.0, *, config: SpelledGainConfig) -> float:
    return float(len(f"{x * config.gain:.1f}"))


def an_island_whose_gain_a_tally_drives(
    input_port: str, scaled_import_path: str, scaled_pins: "dict[str, float]"
) -> "dict[str, Any]":
    return {
        "input_port": input_port,
        "link_id": f"L-{input_port}",
        "output": {"node": "scaled-1", "port": "result"},
        "nodes": [
            {
                "node": "tally-1",
                "breadcrumb_number": 0,
                "import_path": f"{__name__}:tally",
                "config": None,
                "pins": {"x": 0.0},
                "links_into": [],
            },
            {
                "node": "scaled-1",
                "breadcrumb_number": 1,
                "import_path": scaled_import_path,
                "config": {"gain": 10.0},
                "pins": scaled_pins,
                "exposed_config_fields": ["gain"],
                "links_into": [{"port": "gain", "from": {"node": "tally-1", "port": "result"}}],
            },
        ],
    }


def test_a_linked_exposed_config_field_drives_a_compiled_node_in_place_of_its_config():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["amplified"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_whose_gain_a_tally_drives("amplified", f"{__name__}:amplify", {"x": 2.0})],
        {f"{__name__}:tally": tally, f"{__name__}:amplify": amplify}.__getitem__,
    )
    assert fed.ran_compiled_by_node_id == {"tally-1": True, "scaled-1": True}
    assert the_value_read_from(data_plane, "amplified") == 2.0 * 3.0, (
        "the tally's 3 is the gain, not the config's 10"
    )


def test_a_linked_exposed_config_field_drives_a_node_run_as_python_in_place_of_its_config():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["spelled"], [])
    scaled_import_path = f"{__name__}:spell_the_scaled_value"
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_whose_gain_a_tally_drives("spelled", scaled_import_path, {})],
        {f"{__name__}:tally": tally, scaled_import_path: spell_the_scaled_value}.__getitem__,
    )
    assert fed.ran_compiled_by_node_id == {"tally-1": True, "scaled-1": False}
    assert the_value_read_from(data_plane, "spelled") == float(len("12.0")), (
        "4 scaled by the tally's 3 spells 12.0, not 40.0"
    )


def test_an_exposed_config_field_no_link_drives_reads_the_nodes_config():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["amplified"], [])
    island = an_island_whose_gain_a_tally_drives("amplified", f"{__name__}:amplify", {"x": 2.0})
    island["nodes"][1]["links_into"] = []
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], {f"{__name__}:tally": tally, f"{__name__}:amplify": amplify}.__getitem__
    )
    assert the_value_read_from(data_plane, "amplified") == 2.0 * 10.0


@node.function
def one_without_a_frame_and_two_with_one(frame: "UnreadMessage | None" = None) -> float:
    return 1.0 if frame is None else 2.0


def test_an_unlinked_optional_message_input_takes_none_and_its_island_feeds_its_port():
    import_path = f"{__name__}:one_without_a_frame_and_two_with_one"
    island = an_island_of_one_node("level", import_path, None, "frame-counter-1")
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["level"], [])
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], {import_path: one_without_a_frame_and_two_with_one}.__getitem__
    )
    assert the_value_read_from(data_plane, "level") == 1.0

# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A `@node.function` node promoted to run as its helper's node.

The promoted node reads its inputs and writes each output as a control value,
compiled or run as Python.
"""

from __future__ import annotations

import array
import os
import struct
import itertools
from dataclasses import dataclass
from typing import Any, Literal

import pytest

from tatolab.runtime import _engine
from tatolab.runtime import _node_interpreter_bootstrap as node_interpreter_bootstrap
from tatolab.runtime._function_node_compilation import (
    FunctionNodeCompiledToItsEvaluateEntry,
    FunctionNodeDoesNotCompile,
    FunctionNodeRunAsPythonInItsReader,
    prepare_function_node_to_evaluate,
)
from tatolab.runtime._function_node_run_as_a_standalone_node import (
    ran_compiled_by_node_id_of_the_function_node_a_standalone_node_runs,
    the_node_class_a_promoted_function_node_runs_as,
)
from tatolab.runtime._node_hosting import apply_configuration, construct_node_instance
from tatolab.stream import State, Stateful, UnreadMessage, node
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

from function_node_test_nodes import (
    Phase,
    RustGate,
    amplify,
    count_the_label_letters,
    label_the_level,
    tally,
)

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


def _constructed_with_its_ports_declared(node_class: Any, config: Any, data_plane: Any) -> Any:
    """`node_class` built on `data_plane` with its ports declared first, as a helper's bootstrap builds it."""
    data_plane.declare_ports(
        [port["name"] for port in getattr(node_class, NODE_DECLARATION_INPUT_PORTS_STAMP)],
        [port["name"] for port in getattr(node_class, NODE_DECLARATION_OUTPUT_PORTS_STAMP)],
    )
    return construct_node_instance(node_class, config, data_plane)


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
        node_interpreter_bootstrap.wire_link_data_access(
            data_plane,
            {
                "inputs": [
                    _wiring_of_one_link_end("input", input_port, link_id)
                    for input_port, link_id in link_id_by_input_port.items()
                ]
            },
        )
        node_interpreter_bootstrap.wire_link_data_access(
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
    runner_class = the_node_class_a_promoted_function_node_runs_as(
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
        the_node_class_a_promoted_function_node_runs_as(
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
        the_node_class_a_promoted_function_node_runs_as(
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
    runner_class = the_node_class_a_promoted_function_node_runs_as(
        function_node, import_path, "gate-1", data_plane
    )
    input_families = {
        port["name"]: port["family"] for port in getattr(runner_class, NODE_DECLARATION_INPUT_PORTS_STAMP)
    }
    assert input_families["value"]["also_takes_families"] == ["array", "channels", "packets", "records"]
    runner = _constructed_with_its_ports_declared(runner_class, None, data_plane)
    destination = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        destination, {"inputs": [_wiring_of_one_link_end("input", "read", f"L-out-of-{import_path}")]}
    )
    node_interpreter_bootstrap.wire_link_data_access(
        data_plane,
        {
            "inputs": [_wiring_of_one_link_end("input", "value", f"L-into-{import_path}")],
            "outputs": [_wiring_of_one_link_end("output", "result", f"L-out-of-{import_path}")],
        },
    )
    source = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
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
    from tatolab.runtime._function_node_pool import the_node_class_the_function_node_pool_runs_as

    data_plane = _engine.open_node_link_data_access_for_helper_process()
    pool_class = the_node_class_the_function_node_pool_runs_as(
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
    node_interpreter_bootstrap.wire_link_data_access(
        destination, {"inputs": [_wiring_of_one_link_end("input", "read", "L-out-of-the-pool")]}
    )
    node_interpreter_bootstrap.wire_link_data_access(
        data_plane,
        {
            "inputs": [_wiring_of_one_link_end("input", "island_0", "L-into-the-pool")],
            "outputs": [_wiring_of_one_link_end("output", "island_0_output", "L-out-of-the-pool")],
        },
    )
    source = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        source, {"outputs": [_wiring_of_one_link_end("output", "frames", "L-into-the-pool")]}
    )

    source.write_to_output_port("frames", b"a frame's descriptor", timestamp_ns=21)
    pool.process(_ContextOfOneProcess())

    payload, timestamp_ns = destination.read_from_input_port_with_timestamp("read")
    assert payload is not None
    assert (bytes(payload), timestamp_ns) == (b"a frame's descriptor", 21)


def test_the_pool_hands_on_every_part_of_a_control_message_its_island_answers():
    from tatolab.runtime._function_node_pool import the_node_class_the_function_node_pool_runs_as

    data_plane = _engine.open_node_link_data_access_for_helper_process()
    pool_class = the_node_class_the_function_node_pool_runs_as(
        {"carries": [{"input_port": "island_0", "output_port": "island_0_output", "family": "channels"}]},
        data_plane,
    )
    pool = _constructed_with_its_ports_declared(pool_class, None, data_plane)
    destination = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        destination, {"inputs": [_wiring_of_one_link_end("input", "read", "L-out-of-the-pool-of-parts")]}
    )
    node_interpreter_bootstrap.wire_link_data_access(
        data_plane,
        {"outputs": [_wiring_of_one_link_end("output", "island_0_output", "L-out-of-the-pool-of-parts")]},
    )
    island = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["island_0"])
    color_parts = [0.5, 0.25, 1.0, 1.0]

    island.write_block("island_0", "d", [[part] for part in color_parts], sample_rate=0)
    pool.process(_ContextOfOneProcess())

    payload = destination.read_from_input_port("read")
    assert payload is not None
    assert bytes(payload) == struct.pack("<4d", *color_parts), "each part reaches the reader, in order"
    pool.process(_ContextOfOneProcess())
    assert destination.read_from_input_port("read") is None, "each message is handed on once"


def test_the_pool_writes_an_island_carried_into_a_setting_as_it_sets_up_and_no_other():
    """The engine builds a reader whose setting an island feeds once the pool has set up, so the
    pool writes that island's value at its setup; an island into any other input waits for the
    pool's first `process`.

    Fail-without-fix: the pool writes nothing at its setup, and the reader is built with the
    setting's own value.
    """
    from tatolab.runtime._function_node_pool import the_node_class_the_function_node_pool_runs_as

    data_plane = _engine.open_node_link_data_access_for_helper_process()
    pool_class = the_node_class_the_function_node_pool_runs_as(
        {
            "carries": [
                {
                    "input_port": "island_0",
                    "output_port": "island_0_output",
                    "family": "channels",
                    "written_at_setup": True,
                },
                {"input_port": "island_1", "output_port": "island_1_output", "family": "channels"},
            ]
        },
        data_plane,
    )
    pool = _constructed_with_its_ports_declared(pool_class, None, data_plane)
    destinations = {}
    for carry_number in (0, 1):
        destination = _engine.open_node_link_data_access_for_helper_process()
        link_id = f"L-out-of-the-pool-at-setup-{carry_number}"
        node_interpreter_bootstrap.wire_link_data_access(
            destination, {"inputs": [_wiring_of_one_link_end("input", "read", link_id)]}
        )
        node_interpreter_bootstrap.wire_link_data_access(
            data_plane,
            {"outputs": [_wiring_of_one_link_end("output", f"island_{carry_number}_output", link_id)]},
        )
        destinations[carry_number] = destination
    island = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["island_0", "island_1"])
    island.write_block("island_0", "d", [[0.5]], sample_rate=0)
    island.write_block("island_1", "d", [[0.25]], sample_rate=0)

    pool.setup(_ContextOfOneProcess())

    payload = destinations[0].read_from_input_port("read")
    assert payload is not None, "the island into a setting is written at setup"
    assert bytes(payload) == struct.pack("<d", 0.5)
    assert destinations[1].read_from_input_port("read") is None, "the other waits for `process`"
    pool.process(_ContextOfOneProcess())
    payload = destinations[1].read_from_input_port("read")
    assert payload is not None
    assert bytes(payload) == struct.pack("<d", 0.25)


def test_a_running_pool_takes_a_new_carry_and_hands_its_island_on_without_a_restart():
    from tatolab.runtime._function_node_pool import (
        take_the_carries_a_running_pool_is_handed,
        the_node_class_the_function_node_pool_runs_as,
    )

    first_carry = {"input_port": "island_0", "output_port": "island_0_output", "family": "channels"}
    late_carry = {"input_port": "island_1", "output_port": "island_1_output", "family": "channels"}
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    pool_class = the_node_class_the_function_node_pool_runs_as({"carries": [first_carry]}, data_plane)
    pool = _constructed_with_its_ports_declared(pool_class, None, data_plane)
    pool.process(_ContextOfOneProcess())

    take_the_carries_a_running_pool_is_handed(
        pool_class, {"carries": [first_carry, late_carry]}, data_plane
    )
    take_the_carries_a_running_pool_is_handed(
        pool_class, {"carries": [first_carry, late_carry]}, data_plane
    )
    assert [carry[0] for carry in pool_class._carries] == ["island_0", "island_1"], (
        "a carry the pool holds is taken once"
    )
    destination = _engine.open_node_link_data_access_for_helper_process()
    node_interpreter_bootstrap.wire_link_data_access(
        destination, {"inputs": [_wiring_of_one_link_end("input", "read", "L-out-of-the-late-carry")]}
    )
    node_interpreter_bootstrap.wire_link_data_access(
        data_plane,
        {"outputs": [_wiring_of_one_link_end("output", "island_1_output", "L-out-of-the-late-carry")]},
    )
    island = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["island_1"])

    island.write_block("island_1", "d", [[0.75]], sample_rate=0)
    pool.process(_ContextOfOneProcess())

    payload = destination.read_from_input_port("read")
    assert payload is not None, "the same pool instance hands the late island on"
    assert bytes(payload) == struct.pack("<d", 0.75)


def test_a_promoted_function_node_starts_from_its_pin_values():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    runner_class = the_node_class_a_promoted_function_node_runs_as(
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
    runner_class = the_node_class_a_promoted_function_node_runs_as(
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
    runner_class = the_node_class_a_promoted_function_node_runs_as(
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
    runner_class = the_node_class_a_promoted_function_node_runs_as(
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
        the_node_class_a_promoted_function_node_runs_as(
            passes_its_process_input_through,
            f"{__name__}:passes_its_process_input_through",
            "passes-1",
            data_plane,
        )


class _OutputsRecordingEachWrite(_OutputsRecordingEachControlValueWritten):
    """`ctx.outputs` recording each Channels block, and each Records write by its port."""

    def __init__(self) -> None:
        super().__init__()
        self.records_written: "list[tuple[str, object]]" = []

    def write(self, port: Any, payload: Any) -> None:
        self.records_written.append((port.port_name, payload))


def test_a_promoted_function_node_reports_whether_its_own_helper_runs_it_compiled():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    compiled = the_node_class_a_promoted_function_node_runs_as(
        amplify, f"{__name__}:amplify", "amplify-1", data_plane
    )
    run_as_python = the_node_class_a_promoted_function_node_runs_as(
        label_the_level, f"{__name__}:label_the_level", "label-1", data_plane
    )

    assert ran_compiled_by_node_id_of_the_function_node_a_standalone_node_runs(compiled) == {
        "amplify-1": True
    }
    assert ran_compiled_by_node_id_of_the_function_node_a_standalone_node_runs(
        run_as_python
    ) == {"label-1": False}
    assert ran_compiled_by_node_id_of_the_function_node_a_standalone_node_runs(Phase) == {}, (
        "a class that runs no promoted function node reports none"
    )


def test_a_promoted_function_node_run_as_python_reads_and_writes_records():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    labelling = _constructed_with_its_ports_declared(
        the_node_class_a_promoted_function_node_runs_as(
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
        the_node_class_a_promoted_function_node_runs_as(
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

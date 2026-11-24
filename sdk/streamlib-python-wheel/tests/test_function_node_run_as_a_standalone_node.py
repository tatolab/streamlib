# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A function node promoted to run as its helper's node.

The promoted node reads its inputs and writes each output as a control value,
compiled or run as Python.
"""

from __future__ import annotations

import array
import os
import struct
import itertools
from typing import Any, Literal

import pytest

from tatolab.runtime._node_the_wheel_hosts import link_output_data_writer_of
from tatolab.runtime import _engine
from tatolab.runtime import _node_interpreter_bootstrap as node_interpreter_bootstrap
from tatolab.runtime._function_node_compilation import (
    FunctionNodeCompiledToItsEvaluateEntry,
    prepare_function_node_to_evaluate,
)
from tatolab.runtime._function_node_run_as_a_standalone_node import (
    ran_compiled_by_node_id_of_the_function_node_a_standalone_node_runs,
    the_node_class_a_promoted_function_node_runs_as,
)
from tatolab.runtime._node_hosting import construct_node_instance
from tatolab.runtime._function_node_compilation import FunctionNodeDoesNotCompile
from tatolab.stream import node
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

from function_node_test_nodes import (
    Phase,
    RustGate,
    WeightedSumCountingItsCalls,
    amplify,
    measure_its_own_spelling,
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
    return construct_node_instance(node_class, config)


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
        self._source_outputs = link_output_data_writer_of(
            _engine.open_runtime_context_full_access_for_helper_process(
                self._source, "runtime-under-test", "source-under-test"
            )
        )

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
    """The output writer recording each Channels block written."""

    def __init__(self) -> None:
        self.blocks_written: "list[tuple[str, object, int, int]]" = []

    def write_channels_block(
        self, port_name: str, samples: Any, *, start_sample: int, sample_rate: int
    ) -> None:
        self.blocks_written.append((port_name, memoryview(samples).tolist(), start_sample, sample_rate))


class _ContextOfOneProcess:
    def __init__(self) -> None:
        self._link_output_data_writer = _OutputsRecordingEachControlValueWritten()


def test_a_promoted_function_node_runs_as_a_process_writing_each_output_as_a_control_value():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    runner_class = the_node_class_a_promoted_function_node_runs_as(
        amplify, f"{__name__}:amplify", "amplify-1", data_plane
    )
    assert [port["name"] for port in getattr(runner_class, NODE_DECLARATION_OUTPUT_PORTS_STAMP)] == [
        "result"
    ]
    assert [port["name"] for port in getattr(runner_class, NODE_DECLARATION_INPUT_PORTS_STAMP)] == [
        "x",
        "gain",
        "inverted",
    ]
    runner = _constructed_with_its_ports_declared(runner_class, None, data_plane)
    ctx = _ContextOfOneProcess()

    runner.process(ctx)
    control_values = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["x", "gain"])
    control_values.write_control_value("x", 2.0)
    control_values.write_control_value("gain", 3.0)
    runner.process(ctx)
    runner.process(ctx)
    assert ctx._link_output_data_writer.blocks_written == [
        ("result", [[0.0]], 0, 0),
        ("result", [[6.0]], 1, 0),
        ("result", [[6.0]], 2, 0),
    ], "each run writes one f64 at rate 0, an input reading its default until a value arrives"


def test_a_promoted_function_node_reads_a_number_as_an_island_does_off_an_f32_block_of_any_shape():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    runner = _constructed_with_its_ports_declared(
        the_node_class_a_promoted_function_node_runs_as(
            amplify, f"{__name__}:amplify", "amplify-1", data_plane
        ),
        None,
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

    assert [block[1] for block in ctx._link_output_data_writer.blocks_written] == [[[0.5]], [[1.5]], [[3.0]]], (
        "a number reads the first sample of an f32 or f64 block, whatever its channels and samples"
    )


def test_a_promoted_function_node_refuses_a_number_read_off_a_message_that_is_no_channels_block():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    runner = _constructed_with_its_ports_declared(
        the_node_class_a_promoted_function_node_runs_as(
            amplify, f"{__name__}:amplify", "amplify-1", data_plane
        ),
        None,
        data_plane,
    )
    control_values = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["x"])
    control_values.write_bytes("x", struct.pack("<d", 4.0))

    with pytest.raises(ValueError, match=r"function node `amplify-1` reads a number .* is no Channels block"):
        runner.process(_ContextOfOneProcess())


@node
def gate_each_message[PinValue](value: PinValue, open: bool = True) -> PinValue | None:
    return value if open else None


@node
def hand_each_message_on_once_it_has_built_an_object[PinValue](value: PinValue) -> PinValue:
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
    assert "value" in [port["name"] for port in getattr(runner_class, NODE_DECLARATION_INPUT_PORTS_STAMP)]
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
    assert [port["name"] for port in getattr(pool_class, NODE_DECLARATION_OUTPUT_PORTS_STAMP)] == [
        "island_0_output"
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


def _a_pool_with_its_carries_wired_to_readers(carries: "list[dict[str, Any]]") -> "tuple[Any, Any, dict[int, Any]]":
    """A pool built from `carries`, each output wired to a reader of its own: the pool, its data plane and each reader by carry number."""
    from tatolab.runtime._function_node_pool import the_node_class_the_function_node_pool_runs_as

    data_plane = _engine.open_node_link_data_access_for_helper_process()
    pool_class = the_node_class_the_function_node_pool_runs_as({"carries": carries}, data_plane)
    pool = _constructed_with_its_ports_declared(pool_class, None, data_plane)
    readers = {}
    for carry_number in range(len(carries)):
        reader = _engine.open_node_link_data_access_for_helper_process()
        link_id = f"L-out-of-the-pool-{carry_number}-{next(_links_into_promoted_nodes_opened)}"
        node_interpreter_bootstrap.wire_link_data_access(
            reader, {"inputs": [_wiring_of_one_link_end("input", "read", link_id)]}
        )
        node_interpreter_bootstrap.wire_link_data_access(
            data_plane,
            {"outputs": [_wiring_of_one_link_end("output", f"island_{carry_number}_output", link_id)]},
        )
        readers[carry_number] = reader
    return pool, data_plane, readers


def _a_carry(carry_number: int, *, written_at_setup: bool) -> "dict[str, Any]":
    carry: "dict[str, Any]" = {
        "input_port": f"island_{carry_number}",
        "output_port": f"island_{carry_number}_output",
        "family": "channels",
    }
    if written_at_setup:
        carry["written_at_setup"] = True
    return carry


def test_an_island_raising_as_the_pool_sets_up_leaves_the_other_carries_written(monkeypatch):
    """An island carried into a setting that raises as the pool sets up is logged, and the pool
    still sets up: another island into a setting is written then, and every carry from the
    pool's first `process` on.

    Fail-without-fix: the island's failure fails the pool's setup, and no carry is written.
    """
    from tatolab.runtime import _function_node_pool

    pool, data_plane, readers = _a_pool_with_its_carries_wired_to_readers(
        [_a_carry(0, written_at_setup=True), _a_carry(1, written_at_setup=True), _a_carry(2, written_at_setup=False)]
    )
    island = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["island_0", "island_1", "island_2"])
    for carry_number, value in ((0, 0.5), (1, 0.25), (2, 0.125)):
        island.write_block(f"island_{carry_number}", "d", [[value]], sample_rate=0)
    read_the_island_as_the_engine_does = _function_node_pool.read_the_message_at_an_input_port_unread

    def an_island_that_raises_at_island_0(*, link_data_access: Any, input_port: str) -> Any:
        if input_port == "island_0":
            raise RuntimeError("the island raised")
        return read_the_island_as_the_engine_does(link_data_access=link_data_access, input_port=input_port)

    monkeypatch.setattr(
        _function_node_pool, "read_the_message_at_an_input_port_unread", an_island_that_raises_at_island_0
    )
    pool.setup(_ContextOfOneProcess())

    assert readers[0].read_from_input_port("read") is None, "the island that raised wrote nothing"
    payload = readers[1].read_from_input_port("read")
    assert payload is not None and bytes(payload) == struct.pack("<d", 0.25)
    monkeypatch.setattr(
        _function_node_pool, "read_the_message_at_an_input_port_unread", read_the_island_as_the_engine_does
    )
    pool.process(_ContextOfOneProcess())
    for carry_number, value in ((0, 0.5), (2, 0.125)):
        payload = readers[carry_number].read_from_input_port("read")
        assert payload is not None, f"carry {carry_number} is written at the pool's first `process`"
        assert bytes(payload) == struct.pack("<d", value)


def test_the_pool_writes_an_island_carried_into_a_setting_again_only_once_its_value_changes():
    """Each value the engine reads into a setting may rebuild its node, so the pool writes an
    island into a setting only when what it carries changed; an island into any other input is
    written at each evaluation.

    Fail-without-fix: the unchanged value is written again, and the reader's wire wakes.
    """
    pool, data_plane, readers = _a_pool_with_its_carries_wired_to_readers(
        [_a_carry(0, written_at_setup=True), _a_carry(1, written_at_setup=False)]
    )
    island = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["island_0", "island_1"])

    def the_values_each_reader_reads_after_one_process(values: "tuple[float, float]") -> "list[Any]":
        for carry_number, value in enumerate(values):
            island.write_block(f"island_{carry_number}", "d", [[value]], sample_rate=0)
        pool.process(_ContextOfOneProcess())
        read = [readers[carry_number].read_from_input_port("read") for carry_number in (0, 1)]
        return [None if payload is None else struct.unpack("<d", bytes(payload))[0] for payload in read]

    assert the_values_each_reader_reads_after_one_process((0.5, 0.5)) == [0.5, 0.5]
    assert the_values_each_reader_reads_after_one_process((0.5, 0.5)) == [None, 0.5], (
        "the setting's island is not written again with what it carried"
    )
    assert the_values_each_reader_reads_after_one_process((0.75, 0.5)) == [0.75, 0.5]


def test_a_promoted_function_node_feeding_a_setting_is_called_as_it_sets_up():
    """A promoted function node whose output feeds a setting read at setup is called once at
    its setup, so the setting's node, set up after it, is built with its value; one feeding no
    setting is first called at its first `process`.

    Fail-without-fix: the node is not called at its setup, and the setting's node is built with
    the setting's own value.
    """
    from tatolab.runtime._function_node_run_as_a_standalone_node import (
        evaluate_a_promoted_function_node_at_its_setup,
    )

    def a_runner(evaluated_at_setup: bool) -> "tuple[Any, Any, Any]":
        data_plane = _engine.open_node_link_data_access_for_helper_process()
        runner_class = the_node_class_a_promoted_function_node_runs_as(
            tally, f"{__name__}:tally", "tally-1", data_plane
        )
        if evaluated_at_setup:
            evaluate_a_promoted_function_node_at_its_setup(runner_class)
        runner = _constructed_with_its_ports_declared(runner_class, None, data_plane)
        control_values = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["x"])
        control_values.write_control_value("x", 4.0)
        return runner, _ContextOfOneProcess(), control_values

    feeding_a_setting, ctx, _control_values = a_runner(evaluated_at_setup=True)
    feeding_a_setting.setup(ctx)
    assert ctx._link_output_data_writer.blocks_written == [("result", [[7.0]], 0, 0)]

    feeding_no_setting, ctx, _control_values = a_runner(evaluated_at_setup=False)
    feeding_no_setting.setup(ctx)
    assert ctx._link_output_data_writer.blocks_written == []


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

    assert ctx._link_output_data_writer.blocks_written == [("result", [[7.0]], 0, 0)]


@node
def passes_its_process_and_setup_inputs_on(process: float = 0.0, setup: float = 0.0) -> float:
    return process + setup


def test_a_promoted_function_node_with_ports_named_like_runner_members_runs():
    """Its ports are stamped by name, never set on the class, so a port named `process` or
    `setup` leaves the runner's own members as they are."""
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    runner = _constructed_with_its_ports_declared(
        the_node_class_a_promoted_function_node_runs_as(
            passes_its_process_and_setup_inputs_on,
            f"{__name__}:passes_its_process_and_setup_inputs_on",
            "passes-1",
            data_plane,
        ),
        None,
        data_plane,
    )
    control_values = _ChannelsBlocksWrittenIntoAPromotedNode(data_plane, ["process", "setup"])
    ctx = _ContextOfOneProcess()

    control_values.write_control_value("process", 2.0)
    control_values.write_control_value("setup", 0.5)
    runner.process(ctx)

    assert ctx._link_output_data_writer.blocks_written == [("result", [[2.5]], 0, 0)]


def test_a_promoted_function_node_reports_whether_its_own_helper_runs_it_compiled():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    compiled = the_node_class_a_promoted_function_node_runs_as(
        amplify, f"{__name__}:amplify", "amplify-1", data_plane
    )
    run_as_python = the_node_class_a_promoted_function_node_runs_as(
        measure_its_own_spelling, f"{__name__}:measure_its_own_spelling", "spelling-1", data_plane
    )

    assert ran_compiled_by_node_id_of_the_function_node_a_standalone_node_runs(compiled) == {
        "amplify-1": True
    }
    assert ran_compiled_by_node_id_of_the_function_node_a_standalone_node_runs(
        run_as_python
    ) == {"spelling-1": False}
    assert ran_compiled_by_node_id_of_the_function_node_a_standalone_node_runs(Phase) == {}, (
        "a class that runs no promoted function node reports none"
    )


@pytest.mark.parametrize("function_node", [amplify, measure_its_own_spelling])
def test_a_promoted_python_function_node_refuses_a_config_naming_the_inputs_that_replace_it(
    function_node: Any,
):
    """Mental-revert: reading the config again takes `gain` as the node's setting."""
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    runner_class = the_node_class_a_promoted_function_node_runs_as(
        function_node, f"{__name__}:{function_node.__name__}", "python-1", data_plane
    )
    with pytest.raises(FunctionNodeDoesNotCompile, match="config sets gain, and it takes no config"):
        _constructed_with_its_ports_declared(runner_class, {"gain": 3.0}, data_plane)
    _constructed_with_its_ports_declared(runner_class, None, data_plane)

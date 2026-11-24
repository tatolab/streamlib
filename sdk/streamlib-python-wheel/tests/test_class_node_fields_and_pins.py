# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A class node under bare `@node` reads and writes through its pins, and each field holds its input's current value.

The node is built on a real data plane, as its helper builds it: every field holding its set
value and every pin bound before `__init__()`. A field a wire feeds holds the newest value the
wire gave, and its set value again once the wire is gone; a value set live reaches the next
`process()` of a running helper, and a setting keeps the value it was built with.
"""

from __future__ import annotations

import array
import itertools
import os
import time
from typing import Any

import pytest

from tatolab.runtime._node_the_wheel_hosts import link_input_data_reader_of, link_output_data_writer_of
from tatolab.runtime import _engine
from tatolab.runtime import _node_interpreter_bootstrap as node_interpreter_bootstrap
from tatolab.runtime._class_node_fields import ClassNodeFields
from tatolab.runtime._node_hosting import (
    class_node_members_of,
    construct_class_node_from_its_fields_and_pins,
)
from tatolab.runtime._node_interpreter_bootstrap import ParentProcessBridge, load_node_class
from tatolab.stream import Audio, Color, node
from tatolab.stream._class_node_pins import InputPin, OutputPin
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)
from tatolab.stream._pin_types import Trigger

from helper_process_test_support import PROBE_MODULE, drive_lifecycle_on_a_thread

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


@node(execution="continuous", interval_ms=5)
class LevelTinter:
    """Writes its tint scaled by the level it reads and by its gain."""

    gain: float = 1.0
    tint: Color = Color(1.0, 0.5, 0.25)
    level: InputPin[float]
    sound: InputPin[Audio]
    reset: InputPin[Trigger]
    tinted: OutputPin[Color]

    def process(self, ctx: Any) -> None:
        level = self.level.read()
        if level is not None:
            scale = level * self.gain
            self.tinted.write(
                Color(self.tint.red * scale, self.tint.green * scale, self.tint.blue * scale)
            )


_links_opened = itertools.count()


def _wiring_of_one_link_end(direction: str, port_name: str, link_id: str) -> "dict[str, Any]":
    """One entry of the envelope the engine's wiring path hands a helper."""
    channel_service_name = f"classnode{os.getpid()}_{link_id}/channel"
    notify_service_name = f"classnode{os.getpid()}_{link_id}_dest/notify"
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


class LevelTinterOnARealDataPlane:
    """A `LevelTinter` built as its helper builds it, a source wired into each named input and a reader wired from `tinted`."""

    def __init__(self, stored_input_values: "dict[str, Any]") -> None:
        self.data_plane = _engine.open_node_link_data_access_for_helper_process()
        self.data_plane.declare_ports(
            [port["name"] for port in getattr(LevelTinter, NODE_DECLARATION_INPUT_PORTS_STAMP)],
            [port["name"] for port in getattr(LevelTinter, NODE_DECLARATION_OUTPUT_PORTS_STAMP)],
        )
        self.ctx = _engine.open_runtime_context_full_access_for_helper_process(
            self.data_plane, "runtime-under-test", "tinter-1"
        )
        self.reader = _engine.open_node_link_data_access_for_helper_process()
        self.reader.declare_ports(["tinted_read"], [])
        out_of_the_node = f"L-out-{next(_links_opened)}"
        node_interpreter_bootstrap.wire_link_data_access(
            self.reader, {"inputs": [_wiring_of_one_link_end("input", "tinted_read", out_of_the_node)]}
        )
        node_interpreter_bootstrap.wire_link_data_access(
            self.data_plane,
            {"outputs": [_wiring_of_one_link_end("output", "tinted", out_of_the_node)]},
        )
        self.source = _engine.open_node_link_data_access_for_helper_process()
        self.source_outputs = link_output_data_writer_of(
            _engine.open_runtime_context_full_access_for_helper_process(
                self.source, "runtime-under-test", "source-1"
            )
        )
        self.link_id_by_input_port: "dict[str, str]" = {}
        self._blocks_written = 0
        members = class_node_members_of(LevelTinter)
        assert members is not None
        self.fields = ClassNodeFields(
            members,
            link_data_access=self.data_plane,
            inputs=link_input_data_reader_of(self.ctx),
            node_id="tinter-1",
            stored_input_values=stored_input_values,
        )
        self.node: Any = construct_class_node_from_its_fields_and_pins(
            LevelTinter,
            members,
            self.fields,
            link_data_access=self.data_plane,
            inputs=link_input_data_reader_of(self.ctx),
            outputs=link_output_data_writer_of(self.ctx),
            node_id="tinter-1",
        )

    def wire_a_source_into(self, input_port: str) -> None:
        link_id = f"L-into-{input_port}-{next(_links_opened)}"
        self.link_id_by_input_port[input_port] = link_id
        node_interpreter_bootstrap.wire_link_data_access(
            self.data_plane, {"inputs": [_wiring_of_one_link_end("input", input_port, link_id)]}
        )
        node_interpreter_bootstrap.wire_link_data_access(
            self.source,
            {"outputs": [_wiring_of_one_link_end("output", f"{input_port}_to_the_node", link_id)]},
        )

    def unwire_the_source_of(self, input_port: str) -> None:
        self.data_plane.unwire_input_link(self.link_id_by_input_port.pop(input_port))

    def write_samples(self, input_port: str, samples: "list[list[float]]", *, sample_rate: int) -> None:
        flat = array.array("d", [sample for channel in samples for sample in channel])
        self.source_outputs.write_channels_block(
            f"{input_port}_to_the_node",
            memoryview(flat).cast("B").cast("d", [len(samples), len(samples[0])]),
            start_sample=self._blocks_written,
            sample_rate=sample_rate,
        )
        self._blocks_written += 1

    def write_control_value(self, input_port: str, value: float) -> None:
        self.write_samples(input_port, [[value]], sample_rate=0)

    def the_colour_read_off_tinted(self) -> "tuple[float, ...] | None":
        return _engine.read_the_parts_at_an_input_port(
            link_data_access=self.reader, node_id="reader-1", input_port="tinted_read", value_type="color"
        )


def test_a_class_node_reads_through_its_input_pin_and_writes_through_its_output_pin():
    tinter = LevelTinterOnARealDataPlane(
        {"gain": 2.0, "tint": {"red": 1.0, "green": 0.5, "blue": 0.25, "alpha": 1.0}}
    )
    tinter.wire_a_source_into("level")

    tinter.node.process(tinter.ctx)
    assert tinter.the_colour_read_off_tinted() is None, "a pin with nothing waiting reads None"

    tinter.write_control_value("level", 0.25)
    tinter.node.process(tinter.ctx)

    assert tinter.the_colour_read_off_tinted() == (0.5, 0.25, 0.125, 1.0)


def test_each_read_type_names_its_pin_type_and_a_fire_reads_as_true():
    tinter = LevelTinterOnARealDataPlane({})
    tinter.wire_a_source_into("sound")
    tinter.write_samples("sound", [[0.0, 0.5], [0.25, -0.25]], sample_rate=48_000)

    block = tinter.node.sound.read()

    assert block is not None
    assert block.pin_type == "audio"
    assert (block.channel_count, block.samples_per_channel, block.sample_rate) == (2, 2, 48_000)
    assert tinter.node.reset.read() is None
    _engine.queue_a_fire_of_each_trigger_input(link_data_access=tinter.data_plane, fired=["reset"])
    assert tinter.node.reset.read() is True


def test_a_field_holds_its_wires_newest_value_and_its_set_value_again_once_the_wire_is_gone():
    tinter = LevelTinterOnARealDataPlane({"gain": 2.0})
    assert tinter.node.gain == 2.0

    tinter.wire_a_source_into("gain")
    tinter.fields.take_each_new_value_a_wire_gives(tinter.node)
    assert tinter.node.gain == 2.0, "a wire that gave nothing yet leaves the set value"

    tinter.write_control_value("gain", 3.0)
    tinter.fields.take_each_new_value_a_wire_gives(tinter.node)
    assert tinter.node.gain == 3.0

    tinter.fields.take_set_values(tinter.node, {"gain": 5.0})
    assert tinter.node.gain == 3.0, "a value set while a wire feeds the field waits for the wire to go"

    tinter.unwire_the_source_of("gain")
    tinter.fields.hand_back_the_set_value_of(tinter.node, "gain")
    assert tinter.node.gain == 5.0


def test_a_field_an_island_of_function_nodes_feeds_holds_the_value_the_island_evaluates():
    """An island is a wire with no link: drop the island's arm from `_a_wire_feeds` and the gain
    keeps its set value, as Oscillator's frequency did under a Constant on the rig."""
    from tatolab.runtime._function_node_compilation import (
        feed_each_input_port_from_its_function_node_island,
    )

    from function_node_test_nodes import an_island_of_one_node, measure_its_own_spelling

    tinter = LevelTinterOnARealDataPlane({"gain": 2.0})
    import_path = f"{measure_its_own_spelling.__module__}:{measure_its_own_spelling.__qualname__}"
    islands = feed_each_input_port_from_its_function_node_island(
        tinter.data_plane,
        [an_island_of_one_node("gain", import_path, None)],
        {import_path: measure_its_own_spelling}.__getitem__,
    )
    assert islands.input_ports_fed == frozenset({"gain"})

    tinter.fields.take_each_new_value_a_wire_gives(
        tinter.node, fed_by_an_island=islands.input_ports_fed
    )
    assert tinter.node.gain == 3.0, "`2.0` is three letters long"

    tinter.fields.take_set_values(tinter.node, {"gain": 5.0})
    tinter.fields.hand_back_the_set_value_of(
        tinter.node, "gain", fed_by_an_island=islands.input_ports_fed
    )
    assert tinter.node.gain == 3.0, "the island still feeds the field"

    islands.stop_feeding_an_input_port("gain")
    tinter.fields.hand_back_the_set_value_of(
        tinter.node, "gain", fed_by_an_island=islands.input_ports_fed
    )
    assert tinter.node.gain == 5.0


def test_a_field_whose_value_was_taken_off_holds_its_default_again():
    """The engine hands every value the node's unlinked inputs read, so a field left out had its
    value taken off: keep the set value instead and an Image File whose asset was cleared keeps
    showing it, as it did on the rig."""
    tinter = LevelTinterOnARealDataPlane({"gain": 2.0})

    tinter.fields.take_set_values(tinter.node, {})

    assert tinter.node.gain == 1.0, "`gain`'s default"
    assert tinter.fields.values_to_build_with() == {}


def test_a_field_set_live_holds_its_value_in_its_python_form():
    tinter = LevelTinterOnARealDataPlane({})
    assert tinter.node.tint == Color(1.0, 0.5, 0.25), "a field with no set value holds its default"

    tinter.fields.take_set_values(
        tinter.node, {"tint": {"red": 0.0, "green": 1.0, "blue": 0.0, "alpha": 0.5}}
    )

    assert tinter.node.tint == Color(0.0, 1.0, 0.0, 0.5)
    with pytest.raises(ValueError, match="`tinter-1`'s field `gain` is 'loud'"):
        tinter.fields.take_set_values(tinter.node, {"gain": "loud"})


def wait_until_the_fields_probe_saw(seen: "tuple[str, float, Any, int]", timeout_s: float = 10.0) -> None:
    from helper_process_probes import WHAT_THE_FIELDS_PROBE_SAW

    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if WHAT_THE_FIELDS_PROBE_SAW[-1:] == [seen]:
            return
        time.sleep(0.01)
    raise AssertionError(f"the probe never saw {seen!r}; it saw: {WHAT_THE_FIELDS_PROBE_SAW[-5:]}")


def test_a_running_helpers_class_node_is_built_from_its_setup_values_and_takes_each_one_set_live(
    stand_in_parent: Any,
):
    """Fail-without-fix: build the node with `node_class()` and its `__init__` sees each
    field's class default; drop the fields' arm from `_set_input_values` and the gain set live
    never reaches `process()`."""
    from helper_process_probes import WHAT_THE_FIELDS_PROBE_SAW

    WHAT_THE_FIELDS_PROBE_SAW.clear()
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:FieldsProbe")
    )
    red = {"red": 1.0, "green": 0.0, "blue": 0.0, "alpha": 1.0}
    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "config": {},
            "ports": {"input_values": {"gain": 0.5, "tint": red, "width": 8}},
        }
    )
    assert stand_in_parent.receive() == {"rpc": "ready"}
    assert WHAT_THE_FIELDS_PROBE_SAW[0] == ("init", 0.5, Color(1.0, 0.0, 0.0), 8)

    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 5})
    wait_until_the_fields_probe_saw(("process", 0.5, Color(1.0, 0.0, 0.0), 8))

    blue = {"red": 0.0, "green": 0.0, "blue": 1.0, "alpha": 1.0}
    stand_in_parent.send(
        {"cmd": "set_input_values", "input_values": {"gain": 2.0, "tint": blue, "width": 16}}
    )
    wait_until_the_fields_probe_saw(("process", 2.0, Color(0.0, 0.0, 1.0), 8))

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()

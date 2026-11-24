# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A class node's `PinGroup` is built with the slots its `setup` command reaches, each slot's pins bound and each slot's values set.

A group that follows its wires is built with one slot past the last a link or an island
reaches, and a group whose count is set with as many as its input named after it holds. Each
slot's pins read through the slot's ports, `layers.0.level`, and a value set live on a slot's
port reaches that slot alone.
"""

from __future__ import annotations

import array
from dataclasses import dataclass
from typing import Annotated, Any

import pytest

from tatolab.runtime._node_the_wheel_hosts import link_input_data_reader_of, link_output_data_writer_of
from tatolab.runtime import _engine
from tatolab.runtime import _node_interpreter_bootstrap as node_interpreter_bootstrap
from tatolab.runtime._class_node_fields import ClassNodeFields
from tatolab.runtime._class_node_pin_group_slots import the_slot_count_of_each_pin_group
from tatolab.runtime._node_hosting import (
    class_node_members_of,
    class_node_pin_groups_of,
    construct_class_node_from_its_fields_and_pins,
    the_members_of_each_slot_built,
    the_slots_its_pin_groups_are_built_with,
)
from tatolab.stream import PinGroup, SettingOnly, node
from tatolab.stream._class_node_pins import InputPin, OutputPin
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

from test_class_node_fields_and_pins import _wiring_of_one_link_end


@dataclass(frozen=True)
class Layer:
    level: InputPin[float]
    gain: float = 1.0


@dataclass(frozen=True)
class Band:
    banded: OutputPin[float]
    width: Annotated[float, SettingOnly()] = 1.0


@node(execution="continuous", interval_ms=5)
class LayerMixer:
    """Sums the level each layer reads scaled by its gain."""

    layers: PinGroup[Layer]
    bands: PinGroup[Band]
    mixed: OutputPin[float]

    def process(self, ctx: Any) -> None:
        pass


def test_a_group_following_its_wires_holds_a_slot_past_the_last_a_link_or_an_island_reaches():
    """Fail-without-fix: count the links alone and a gap before `layers.2` builds two slots,
    leaving the wired `layers.2.level` with no slot."""
    pin_groups = class_node_pin_groups_of(LayerMixer)

    assert the_slot_count_of_each_pin_group(pin_groups, {}) == {"layers": 0, "bands": 1}
    assert the_slot_count_of_each_pin_group(
        pin_groups,
        {
            "inputs": [{"name": "layers.2.level"}, {"name": "level"}],
            "islands": [{"input_port": "layers.0.gain"}],
            "input_values": {"bands": 3, "layers.7.nothing": 1.0},
        },
    ) == {"layers": 3, "bands": 3}


def test_the_slot_ports_of_a_group_are_declared_slot_by_slot():
    counts, inputs, outputs = the_slots_its_pin_groups_are_built_with(
        LayerMixer,
        {"inputs": [{"name": "layers.1.level"}], "input_values": {"bands": 2}},
    )

    assert counts == {"layers": 2, "bands": 2}
    assert inputs == [
        "layers.0.level",
        "layers.0.gain",
        "layers.1.level",
        "layers.1.gain",
        "bands.0.width",
        "bands.1.width",
    ]
    assert outputs == ["bands.0.banded", "bands.1.banded"]


@pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)
def test_each_slot_reads_through_its_own_pin_and_holds_its_own_value_set_live():
    """Fail-without-fix: bind each slot's pins under the slot's own port names, `level`, and
    every slot reads the one port; route a live value by its port alone and `layers.1.gain`
    never reaches the slot."""
    port_wiring = {"inputs": [{"name": "layers.1.level"}], "input_values": {"bands": 1}}
    slot_counts, slot_inputs, slot_outputs = the_slots_its_pin_groups_are_built_with(
        LayerMixer, port_wiring
    )
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(
        [
            *(port["name"] for port in getattr(LayerMixer, NODE_DECLARATION_INPUT_PORTS_STAMP)),
            *slot_inputs,
        ],
        [
            *(port["name"] for port in getattr(LayerMixer, NODE_DECLARATION_OUTPUT_PORTS_STAMP)),
            *slot_outputs,
        ],
    )
    ctx = _engine.open_runtime_context_full_access_for_helper_process(
        {}, data_plane, "runtime-under-test", "mixer-1"
    )
    source = _engine.open_node_link_data_access_for_helper_process()
    source_outputs = link_output_data_writer_of(
        _engine.open_runtime_context_full_access_for_helper_process(
            {}, source, "runtime-under-test", "source-1"
        )
    )
    node_interpreter_bootstrap.wire_link_data_access(
        data_plane, {"inputs": [_wiring_of_one_link_end("input", "layers.1.level", "L-layer-1")]}
    )
    node_interpreter_bootstrap.wire_link_data_access(
        source, {"outputs": [_wiring_of_one_link_end("output", "level_out", "L-layer-1")]}
    )
    members = class_node_members_of(LayerMixer)
    assert members is not None
    members_of_each_slot = the_members_of_each_slot_built(
        class_node_pin_groups_of(LayerMixer), slot_counts
    )
    fields = ClassNodeFields(
        (
            *members,
            *(
                member
                for slots in members_of_each_slot.values()
                for slot_members in slots
                for member in slot_members
            ),
        ),
        link_data_access=data_plane,
        inputs=link_input_data_reader_of(ctx),
        node_id="mixer-1",
        stored_input_values={"layers.0.gain": 2.0, "bands.0.width": 0.5},
    )
    mixer: Any = construct_class_node_from_its_fields_and_pins(
        LayerMixer,
        members,
        fields,
        link_data_access=data_plane,
        inputs=link_input_data_reader_of(ctx),
        outputs=link_output_data_writer_of(ctx),
        node_id="mixer-1",
        members_of_each_slot=members_of_each_slot,
    )

    assert [layer.gain for layer in mixer.layers] == [2.0, 1.0]
    assert mixer.bands[0].width == 0.5

    samples = array.array("d", [0.75])
    source_outputs.write_channels_block(
        "level_out",
        memoryview(samples).cast("B").cast("d", [1, 1]),
        start_sample=0,
        sample_rate=0,
    )
    assert mixer.layers[0].level.read() is None
    assert mixer.layers[1].level.read() == 0.75

    fields.take_set_values(mixer, {"layers.0.gain": 2.0, "layers.1.gain": 4.0, "bands.0.width": 0.5})
    assert [layer.gain for layer in mixer.layers] == [2.0, 4.0]

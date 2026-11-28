# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node function's `PinGroup` is built with the slots its `setup` command reaches, each slot's
ports declared slot by slot.

A group that follows its wires is built with one slot past the last a link or an island
reaches, and a group whose count is set with as many as its input named after it holds. How each
slot reads through its own ports and takes a value set live is held by the node function's own
helper tests.
"""

from __future__ import annotations

from dataclasses import dataclass

from tatolab.runtime._class_node_pin_group_slots import the_slot_count_of_each_pin_group
from tatolab.runtime._node_hosting import (
    class_node_pin_groups_of,
    the_slots_its_pin_groups_are_built_with,
)
from tatolab.stream import PinGroup, node
from tatolab.stream._class_node_pins import InputPin, OutputPin


@dataclass(frozen=True)
class Layer:
    level: InputPin[float]
    gain: float = 1.0


@dataclass(frozen=True)
class Band:
    banded: OutputPin[float]


@node(execution="continuous", interval_ms=5)
def LayerMixer(
    *,
    layers: PinGroup[Layer],
    bands: PinGroup[Band],
    mixed: OutputPin[float],
) -> None:
    """Sums the level each layer reads scaled by its gain."""


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
    ]
    assert outputs == ["bands.0.banded", "bands.1.banded"]

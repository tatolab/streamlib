# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The math, logic and animation nodes are declared in the typed form: every pin states its pin
type, Compare hands out its three outputs from a named tuple, and Gate and Hold Frame hand on
the type linked into their input, Gate of any type and Hold Frame only a Texture."""

from __future__ import annotations

import importlib
from typing import Any

import pytest
from core_nodes.logic import Compare, Comparison, Gate, HeldFrame, HoldFrame

TYPED_MODULES = ("math_nodes", "logic", "animation")


def declared_in(module_name: str) -> "dict[str, Any]":
    module = importlib.import_module(f"core_nodes.{module_name}")
    return {
        name: member
        for name, member in vars(module).items()
        if getattr(member, "__tatolab_node_declared__", False) and member.__module__ == module.__name__
    }


TYPED_NODES = {
    f"{module_name}:{name}": declared
    for module_name in TYPED_MODULES
    for name, declared in declared_in(module_name).items()
}


def ports_of(declared: Any) -> "dict[str, dict[str, Any]]":
    return {
        port["name"]: port
        for port in (*declared.__tatolab_node_input_ports__, *declared.__tatolab_node_output_ports__)
    }


def test_the_thirty_one_math_logic_and_animation_nodes_are_all_typed():
    assert len(TYPED_NODES) == 31


@pytest.mark.parametrize("node_type", sorted(TYPED_NODES))
def test_each_pin_states_its_pin_type(node_type: str):
    for port_name, port in ports_of(TYPED_NODES[node_type]).items():
        assert "pin_declaration" in port, f"{node_type}'s pin `{port_name}` states no pin type"


def test_compare_hands_out_each_field_of_its_named_tuple():
    outputs = [port["name"] for port in Compare.__tatolab_node_output_ports__]
    assert outputs == ["less", "equal", "greater"]
    assert {port["pin_declaration"]["pin_type"]["pin_type"]["type"] for port in Compare.__tatolab_node_output_ports__} == {"boolean"}
    assert Compare(1.0, 2.0) == Comparison(less=True, equal=False, greater=False)
    assert Compare(2.0, 2.05, 0.1) == Comparison(less=False, equal=True, greater=False)


@pytest.mark.parametrize(("declared", "input_port"), [(Gate, "value"), (HoldFrame, "frame")])
def test_gate_and_hold_frame_hand_on_the_type_linked_into_their_input(declared: Any, input_port: str):
    ports = ports_of(declared)
    assert ports["result"]["pin_declaration"]["pin_type"] == {
        "kind": "same_as_input",
        "input_port_name": input_port,
    }


def test_gate_takes_any_type_and_hold_frame_takes_only_pictures():
    assert ports_of(Gate)["value"]["pin_declaration"]["pin_type"] == {"kind": "any"}
    frame = ports_of(HoldFrame)["frame"]
    assert frame["pin_declaration"]["pin_type"] == {"kind": "is", "pin_type": {"type": "texture"}}
    assert frame["family"] == {"family": "image"}, "a number's link, on Channels, finds no family here"


def test_hold_frame_holds_the_first_frame_until_refresh_takes_a_newer_one():
    first, held = HoldFrame("first", previous=HeldFrame())
    assert first == "first"
    assert HoldFrame("second", previous=held) == ("first", held)
    assert HoldFrame("second", refresh=True, previous=held)[0] == "second"


@pytest.mark.parametrize(
    ("node_type", "port_name", "annotations"),
    [
        ("animation:Lfo", "rate_hz", {"unit": "Hz"}),
        ("animation:Pulse", "width", {"range": {"minimum": 0.0, "maximum": 1.0}}),
        ("animation:Lag", "seconds", {"range": {"minimum": 0.0}, "unit": "s"}),
        ("animation:Envelope", "release_seconds", {"range": {"minimum": 0.0}, "unit": "s"}),
        ("logic:Select", "index", {"range": {"minimum": 0.0, "maximum": 3.0}}),
    ],
)
def test_a_number_with_a_range_or_a_unit_states_it(node_type: str, port_name: str, annotations: "dict[str, Any]"):
    assert ports_of(TYPED_NODES[node_type])[port_name]["pin_declaration"]["annotations"] == annotations

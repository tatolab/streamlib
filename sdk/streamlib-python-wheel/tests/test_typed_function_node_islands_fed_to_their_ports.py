# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""An island of bare `@node` function nodes fed to a port: a node typed by a type parameter
hands on the value linked into it, each field of a returned `NamedTuple` is an output, and a
value of parts handed between its nodes is read with the free conversion a link allows."""

from __future__ import annotations

import struct
from typing import Any, NamedTuple

import pytest

from tatolab.runtime import _engine
from tatolab.runtime._function_node_compilation import feed_each_input_port_from_its_function_node_island
from tatolab.stream import Color, Vector2, Vector4, node

from function_node_test_nodes import an_island_of_one_node

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


@node
def place_a_tint(tint: Color = Color(1.0, 0.5, 0.25)) -> Color:
    return tint


@node
def hand_on[PinValue](value: PinValue, open: bool = True) -> PinValue | None:  # noqa: A002
    return value if open else None


@node
def dim_a_tint(tint: Color = Color(0.0, 0.0, 0.0)) -> Color:
    return Color(tint.red / 2, tint.green / 2, tint.blue / 2, tint.alpha)


class LowAndHigh(NamedTuple):
    low: float
    high: float


@node
def split_the_level(x: float = 0.25, spread: float = 0.5) -> LowAndHigh:
    return LowAndHigh(low=x - spread, high=x + spread)


@node
def a_vector_of_four(x: float = 0.0, y: float = 0.0, z: float = 0.0, w: float = 0.0) -> Vector4:
    return Vector4(x, y, z, w)


@node
def a_spot_as_one_number(spot: Vector2 = Vector2(0.0, 0.0)) -> float:
    return spot.x + 10 * spot.y


@node
def a_level(level: float = 0.5) -> float:
    return level


_FUNCTION_NODES_BY_IMPORT_PATH = {
    f"{__name__}:{function_node.__qualname__}": function_node
    for function_node in (
        place_a_tint,
        hand_on,
        dim_a_tint,
        split_the_level,
        a_vector_of_four,
        a_spot_as_one_number,
        a_level,
    )
}


def the_numbers_read_from(data_plane: Any, input_port: str, count: int) -> "tuple[float, ...]":
    payload = data_plane.read_from_input_port(input_port)
    assert payload is not None
    return struct.unpack(f"<{count}d", bytes(payload))


def a_node_of_an_island(node_id: str, breadcrumb_number: int, function_name: str, **links_into: str) -> "dict[str, Any]":
    return {
        "node": node_id,
        "breadcrumb_number": breadcrumb_number,
        "import_path": f"{__name__}:{function_name}",
        "config": None,
        "links_into": [
            {"port": port, "from": {"node": from_node, "port": "result"}}
            for port, from_node in links_into.items()
        ],
    }


def test_a_type_parameter_passes_the_colour_linked_into_it_on_to_the_next_node():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["dimmed"], [])
    island = {
        "input_port": "dimmed",
        "link_id": "L-island",
        "output": {"node": "dim-1", "port": "result"},
        "nodes": [
            a_node_of_an_island("place-1", 0, "place_a_tint"),
            a_node_of_an_island("hand-on-1", 1, "hand_on", value="place-1"),
            a_node_of_an_island("dim-1", 2, "dim_a_tint", tint="hand-on-1"),
        ],
    }
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__
    )
    assert the_numbers_read_from(data_plane, "dimmed", 4) == (0.5, 0.25, 0.125, 1.0)


@pytest.mark.parametrize(("output_port", "level"), [("low", -0.25), ("high", 0.75)])
def test_each_field_of_a_named_tuple_feeds_the_port_its_output_is_linked_to(output_port: str, level: float):
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["level"], [])
    island = an_island_of_one_node("level", f"{__name__}:split_the_level", None, "split-1")
    island["output"]["port"] = output_port
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane, [island], _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__
    )
    assert fed.ran_compiled_by_node_id == {"split-1": True}, "a named tuple of numbers compiles"
    assert the_numbers_read_from(data_plane, "level", 1) == (level,)


def an_island_of_two_nodes_feeding(input_port: str, first: str, second: str, port_linked: str) -> "dict[str, Any]":
    return {
        "input_port": input_port,
        "link_id": f"L-{input_port}",
        "output": {"node": "second-1", "port": "result"},
        "nodes": [
            a_node_of_an_island("first-1", 0, first),
            a_node_of_an_island("second-1", 1, second, **{port_linked: "first-1"}),
        ],
    }


def test_a_vector_of_four_into_a_vector_of_two_keeps_its_first_two_components():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["spot"], [])
    island = an_island_of_two_nodes_feeding("spot", "a_vector_of_four", "a_spot_as_one_number", "spot")
    island["nodes"][0]["pins"] = {"x": 0.25, "y": 0.5, "z": 9.0, "w": 9.0}
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__
    )
    assert the_numbers_read_from(data_plane, "spot", 1) == (5.25,)


def test_one_number_into_a_colour_is_an_opaque_grey():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["dimmed"], [])
    island = an_island_of_two_nodes_feeding("dimmed", "a_level", "dim_a_tint", "tint")
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__
    )
    assert the_numbers_read_from(data_plane, "dimmed", 4) == (0.25, 0.25, 0.25, 1.0)

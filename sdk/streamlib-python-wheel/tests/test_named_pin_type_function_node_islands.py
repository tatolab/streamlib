# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A module's named type between bare `@node` function nodes: a node writes a value of it, the
next reads an equal one, and the port the island feeds reads the bytes its class packs."""

from __future__ import annotations

import struct
from collections.abc import Buffer
from dataclasses import dataclass
from typing import Any, Self

import pytest

from tatolab.runtime import _engine
from tatolab.runtime._function_node_carried_values import FunctionNodeDoesNotCompile
from tatolab.runtime._function_node_compilation import feed_each_input_port_from_its_function_node_island
from tatolab.stream import named_pin_type, node

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


@named_pin_type(description="A level read off a sensor, and how sure the sensor is of it.")
@dataclass(frozen=True)
class SensorReading:
    level: float
    confidence: float

    @classmethod
    def from_bytes(cls, payload: Buffer) -> Self:
        return cls(*struct.unpack("<2d", bytes(payload)))

    def to_bytes(self) -> bytes:
        return struct.pack("<2d", self.level, self.confidence)


@node
def read_the_sensor(level: float = 0.25) -> SensorReading:
    return SensorReading(level, 0.5)


@node
def double_the_reading(reading: SensorReading) -> SensorReading:
    return SensorReading(reading.level * 2, reading.confidence)


@node
def the_surest_level(reading: SensorReading, second: SensorReading | None = None) -> float:
    if second is None or reading.confidence >= second.confidence:
        return reading.level
    return second.level


@node
def a_reading_of_the_wrong_class(level: float = 0.25) -> SensorReading:
    return level  # type: ignore[return-value]


_FUNCTION_NODES_BY_IMPORT_PATH = {
    f"{__name__}:{function_node.__qualname__}": function_node
    for function_node in (
        read_the_sensor,
        double_the_reading,
        the_surest_level,
        a_reading_of_the_wrong_class,
    )
}


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


def an_island_feeding(input_port: str, *nodes: "dict[str, Any]") -> "dict[str, Any]":
    return {
        "input_port": input_port,
        "link_id": f"L-{input_port}",
        "output": {"node": nodes[-1]["node"], "port": "result"},
        "nodes": list(nodes),
    }


def fed(data_plane: Any, island: "dict[str, Any]") -> Any:
    return feed_each_input_port_from_its_function_node_island(
        data_plane, [island], _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__
    )


def test_a_node_writes_a_named_value_and_the_next_reads_an_equal_one():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["reading"], [])
    island = an_island_feeding(
        "reading",
        a_node_of_an_island("sensor-1", 0, "read_the_sensor"),
        a_node_of_an_island("double-1", 1, "double_the_reading", reading="sensor-1"),
    )

    assert fed(data_plane, island).ran_compiled_by_node_id == {"sensor-1": False, "double-1": False}
    assert data_plane.read_from_input_port("reading", into=SensorReading) == SensorReading(0.5, 0.5)


def test_an_optional_named_input_linked_to_nothing_is_none():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["level"], [])
    island = an_island_feeding(
        "level",
        a_node_of_an_island("sensor-1", 0, "read_the_sensor"),
        a_node_of_an_island("surest-1", 1, "the_surest_level", reading="sensor-1"),
    )
    island["nodes"][0]["pins"] = {"level": 0.75}

    fed(data_plane, island)
    payload = data_plane.read_from_input_port("level")
    assert payload is not None
    assert struct.unpack("<d", bytes(payload)) == (0.75,)


def test_a_pin_set_on_a_named_input_is_refused_as_it_holds_no_value():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["level"], [])
    island = an_island_feeding(
        "level",
        a_node_of_an_island("sensor-1", 0, "read_the_sensor"),
        a_node_of_an_island("surest-1", 1, "the_surest_level", reading="sensor-1"),
    )
    island["nodes"][1]["pins"] = {"second": {"level": 1.0}}

    with pytest.raises(FunctionNodeDoesNotCompile, match=r"`second`.*holds no value.*link the input"):
        fed(data_plane, island)


def test_a_body_handing_out_another_class_on_a_named_output_is_refused_naming_the_type():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["reading"], [])
    island = an_island_feeding("reading", a_node_of_an_island("wrong-1", 0, "a_reading_of_the_wrong_class"))

    fed(data_plane, island)
    assert data_plane.read_from_input_port("reading") is None
    raises = _engine.why_each_function_node_its_islands_call_is_raising(link_data_access=data_plane)
    assert f"`{__name__}:SensorReading`" in raises["wrong-1"], raises

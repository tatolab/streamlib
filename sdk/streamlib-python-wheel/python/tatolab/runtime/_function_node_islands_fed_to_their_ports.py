# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Feed each input port from the island of function nodes that computes it."""

from __future__ import annotations

from typing import Any, Callable, Optional

from tatolab.stream import NodeLinkDataAccess, log

from tatolab.runtime._engine import (
    feed_an_input_port_from_a_function_node_island,
    stop_feeding_an_input_port_from_a_function_node_island,
)
from tatolab.runtime._function_node_carried_values import (
    _exactly_as_f64,
    FunctionNodeCompiledToItsEvaluateEntry,
    FunctionNodeDoesNotCompile,
    FunctionNodeReadyToEvaluate,
    NAMED_VALUE_TYPE,
    PART_VALUE_TYPES,
    PinValueCarried,
    pin_value_carried,
)
from tatolab.runtime._function_node_preparation import (
    how_one_function_node_is_called,
    prepare_function_node_to_evaluate,
)
from tatolab.runtime._function_node_stored_values import STORED_VALUE_TYPES


class FunctionNodeIslandsFedToTheirPorts:
    """The islands a helper's input ports are fed from, kept for as long as the ports are read, and how each node runs.

    The native evaluator calls into what each feed prepared, so it is kept
    here. A port fed again is fed the new island in place of the old one. An
    island holds only stateless nodes: the runtime runs each stateful function
    node in a helper of its own.
    """

    def __init__(
        self, link_data_access: NodeLinkDataAccess, load_function_node: Callable[[str], Any]
    ) -> None:
        self._link_data_access = link_data_access
        self._load_function_node = load_function_node
        self._prepared_by_import_path: "dict[str, FunctionNodeReadyToEvaluate]" = {}
        self._island_by_input_port: "dict[str, dict[str, Any]]" = {}

    @property
    def function_node_types_kept_alive(self) -> "list[FunctionNodeReadyToEvaluate]":
        """Every function node type a feed prepared."""
        return list(self._prepared_by_import_path.values())

    @property
    def input_ports_fed(self) -> "frozenset[str]":
        """Each input port an island feeds."""
        return frozenset(self._island_by_input_port)

    @property
    def ran_compiled_by_node_id(self) -> "dict[str, bool]":
        """Whether each node of a fed island runs compiled (`True`) or as Python (`False`)."""
        return {
            island_node["node"]: isinstance(
                self._prepared_by_import_path[island_node["import_path"]],
                FunctionNodeCompiledToItsEvaluateEntry,
            )
            for island in self._island_by_input_port.values()
            for island_node in island["nodes"]
        }

    def feed_an_input_port_from_its_island(self, island: "dict[str, Any]") -> None:
        """Prepare every node of `island` and feed it to its input port, in place of any island feeding it.

        An island with a node input that no value reaches holds instead: its
        port is fed nothing until a later feed gives each input a value.
        """
        input_port = island["input_port"]
        prepared_by_node_id = {
            island_node["node"]: self._prepared(island_node["import_path"])
            for island_node in island["nodes"]
        }
        inputs_waiting_for_a_value = _the_island_node_inputs_no_value_reaches(
            island["nodes"], prepared_by_node_id
        )
        if inputs_waiting_for_a_value:
            self.stop_feeding_an_input_port(input_port)
            log.info(
                "the island feeding this input port holds until each of its function node "
                "inputs has a value",
                input_port=input_port,
                inputs_waiting_for_a_value=inputs_waiting_for_a_value,
            )
            return
        node_index_by_node_id = {
            island_node["node"]: node_index for node_index, island_node in enumerate(island["nodes"])
        }
        output_ports_by_node_id = {
            node_id: prepared.output_ports for node_id, prepared in prepared_by_node_id.items()
        }
        function_nodes_handed_over: "list[Any]" = []
        how_each_node_carries_its_values: "list[dict[str, Any]]" = []
        for island_node in island["nodes"]:
            node_id = island_node["node"]
            prepared = prepared_by_node_id[node_id]
            called_through, config_values, config_value_indices = how_one_function_node_is_called(
                prepared, node_id, island_node.get("config")
            )
            how_each_node_carries_its_values.append(
                {
                    **prepared.how_its_values_are_carried,
                    "inputs_read_from_links_from_outside_the_island": (
                        _the_inputs_read_from_links_from_outside_the_island(
                            prepared, island_node["links_into"]
                        )
                    ),
                }
            )
            function_nodes_handed_over.append(
                (
                    node_id,
                    island_node["breadcrumb_number"],
                    called_through,
                    _where_each_input_takes_its_value(
                        prepared,
                        node_id,
                        island_node["links_into"],
                        island_node.get("pins") or {},
                        node_index_by_node_id,
                        output_ports_by_node_id,
                    ),
                    config_values,
                    list(prepared.initial_state_values),
                    len(prepared.output_ports),
                    config_value_indices,
                )
            )
        island_output = island["output"]
        feed_an_input_port_from_a_function_node_island(
            link_data_access=self._link_data_access,
            input_port=input_port,
            function_nodes=function_nodes_handed_over,
            island_output=(
                node_index_by_node_id[island_output["node"]],
                _output_index(output_ports_by_node_id, island_output["node"], island_output["port"]),
            ),
            how_each_node_carries_its_values=how_each_node_carries_its_values,
        )
        self._island_by_input_port[input_port] = island

    def stop_feeding_an_input_port(self, input_port: str) -> bool:
        """Stop feeding `input_port` from its island; whether an island fed it."""
        self._island_by_input_port.pop(input_port, None)
        return stop_feeding_an_input_port_from_a_function_node_island(
            link_data_access=self._link_data_access, input_port=input_port
        )

    def _prepared(self, import_path: str) -> FunctionNodeReadyToEvaluate:
        if import_path not in self._prepared_by_import_path:
            self._prepared_by_import_path[import_path] = prepare_function_node_to_evaluate(
                self._load_function_node(import_path), import_path
            )
        return self._prepared_by_import_path[import_path]


def _is_read_through_the_reader(linked_from: "dict[str, Any]") -> bool:
    """Whether a link into an island node comes from a node outside the island, whose channel its reader reads."""
    return "reader_input_port" in linked_from


def _the_inputs_read_from_links_from_outside_the_island(
    prepared: FunctionNodeReadyToEvaluate,
    links_into: "list[dict[str, Any]]",
) -> "list[tuple[int, str, str]]":
    """Each input a link from outside the island feeds: its index, the reader input port it is read at, and its value type."""
    linked_from_by_input_port = {link["port"]: link["from"] for link in links_into}
    value_type_by_input_port = dict(zip(prepared.input_ports, prepared.input_value_types))
    inputs_read: "list[tuple[int, str, str]]" = []
    for input_index, input_port in enumerate(prepared.input_ports):
        linked_from = linked_from_by_input_port.get(input_port)
        if linked_from is not None and _is_read_through_the_reader(linked_from):
            inputs_read.append(
                (
                    input_index,
                    linked_from["reader_input_port"],
                    value_type_by_input_port.get(input_port, "float"),
                )
            )
    return inputs_read


def _the_island_node_inputs_no_value_reaches(
    island_nodes: "list[dict[str, Any]]",
    prepared_by_node_id: "dict[str, FunctionNodeReadyToEvaluate]",
) -> "list[str]":
    """Each `node.input` of `island_nodes` linked to nothing, with no pin value and declaring no default."""
    inputs_no_value_reaches: "list[str]" = []
    for island_node in island_nodes:
        node_id = island_node["node"]
        prepared = prepared_by_node_id[node_id]
        linked_input_ports = {link["port"] for link in island_node["links_into"]}
        pinned_input_ports = set(island_node.get("pins") or {})
        input_ports_that_take_none = _the_input_ports_that_take_none(prepared)
        inputs_no_value_reaches.extend(
            f"{island_node.get('name') or node_id}.{input_port}"
            for input_port in prepared.input_ports
            if input_port not in linked_input_ports
            and input_port not in pinned_input_ports
            and input_port not in prepared.pin_defaults_by_input_port
            and input_port not in input_ports_that_take_none
        )
    return inputs_no_value_reaches


def _where_each_input_takes_its_value(
    prepared: FunctionNodeReadyToEvaluate,
    node_id: str,
    links_into: "list[dict[str, Any]]",
    pin_values_by_input_port: "dict[str, Any]",
    node_index_by_node_id: "dict[str, int]",
    output_ports_by_node_id: "dict[str, tuple[str, ...]]",
) -> "list[tuple[Optional[tuple[int, int]], Optional[PinValueCarried]]]":
    """Each input's earlier node output, or else its pin value, or else its declared default.

    A named type's input linked to nothing holds no value: `None`.
    """
    linked_from_by_input_port = {link["port"]: link["from"] for link in links_into}
    value_type_by_input_port = dict(zip(prepared.input_ports, prepared.input_value_types))
    pin_type_by_input_port = dict(zip(prepared.input_ports, prepared.input_pin_types))
    where_each_input_takes_its_value: "list[tuple[Optional[tuple[int, int]], Optional[PinValueCarried]]]" = []
    for input_port in prepared.input_ports:
        linked_from = linked_from_by_input_port.get(input_port)
        if linked_from is not None and _is_read_through_the_reader(linked_from):
            where_each_input_takes_its_value.append((None, 0.0))
        elif linked_from is not None:
            source_node_id = linked_from["node"]
            where_each_input_takes_its_value.append(
                (
                    (
                        node_index_by_node_id[source_node_id],
                        _output_index(output_ports_by_node_id, source_node_id, linked_from["port"]),
                    ),
                    0.0,
                )
            )
        elif input_port in pin_values_by_input_port:
            where_each_input_takes_its_value.append(
                (
                    None,
                    _a_pin_value_as_the_island_carries_it(
                        node_id,
                        input_port,
                        value_type_by_input_port[input_port],
                        pin_type_by_input_port.get(input_port),
                        pin_values_by_input_port[input_port],
                    ),
                )
            )
        elif input_port in prepared.pin_defaults_by_input_port:
            where_each_input_takes_its_value.append(
                (None, prepared.pin_defaults_by_input_port[input_port])
            )
        elif value_type_by_input_port.get(input_port) == NAMED_VALUE_TYPE:
            where_each_input_takes_its_value.append((None, None))
        else:
            where_each_input_takes_its_value.append((None, _NOTHING_AS_AN_UNREAD_MESSAGE_TOKEN))
    return where_each_input_takes_its_value


#: The token an `UnreadMessage` input holding no message carries: inputs are numbered from 1.
_NOTHING_AS_AN_UNREAD_MESSAGE_TOKEN = 0.0


def _the_input_ports_that_take_none(prepared: FunctionNodeReadyToEvaluate) -> "frozenset[str]":
    return frozenset(
        prepared.input_ports[input_index]
        for input_index in prepared.how_its_values_are_carried.get("inputs_that_take_none", [])
    )


def _output_index(
    output_ports_by_node_id: "dict[str, tuple[str, ...]]", node_id: str, output_port: str
) -> int:
    output_ports = output_ports_by_node_id[node_id]
    if output_port not in output_ports:
        raise FunctionNodeDoesNotCompile(
            f"function node `{node_id}` has no output `{output_port}`; its outputs are "
            f"{', '.join(output_ports)}"
        )
    return output_ports.index(output_port)


def _a_pin_value_as_the_island_carries_it(
    node_id: str,
    input_port: str,
    value_type: str,
    pin_type: "Optional[dict[str, Any]]",
    pin_value: Any,
) -> PinValueCarried:
    what_it_is = f"function node `{node_id}`'s pin on input `{input_port}`"
    if value_type == NAMED_VALUE_TYPE:
        raise FunctionNodeDoesNotCompile(
            f"{what_it_is} is set to {pin_value!r}, and the input is a named type, which holds no "
            f"value: it reads only what a link brings. To fix: clear the pin, and link the input."
        )
    if value_type in PART_VALUE_TYPES or value_type in STORED_VALUE_TYPES:
        return pin_value_carried(value_type, pin_value, what_it_is, pin_type)
    if not isinstance(pin_value, (bool, int, float)):
        raise FunctionNodeDoesNotCompile(
            f"{what_it_is} is {pin_value!r}, and an island input takes a number or a bool. "
            f"To fix: set the pin to a number, or link the input."
        )
    return _exactly_as_f64(pin_value, what_it_is)

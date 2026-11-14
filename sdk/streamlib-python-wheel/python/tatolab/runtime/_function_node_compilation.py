# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Compile `@node.function` bodies to their evaluate entry and feed each island to its port.

A body is compiled by Numba in nopython mode. Its dataclasses (its state
record, its config, a several-output return) are lowered as named tuples of the
same fields, so the body constructs and reads them unchanged. A wrapper with the
evaluate entry's C signature unpacks the f64 values the island hands it into the
body's parameters and packs its outputs and new state back. The body is cached
on disk by Numba; the wrapper is compiled at each load.

A body that does not compile, or that takes or gives a `dict`, runs as Python in
its reader's interpreter, called at the read with the interpreter's lock held.

An `UnreadMessage` reaches a compiled body as a token, the index of the input
that holds it plus one, and an output the body may hand out nothing on carries,
past every output, a slot saying whether it handed one out.
"""

from __future__ import annotations

import collections
import copy
import dataclasses
import inspect
import types
import typing
from typing import Any, Callable, Optional

from tatolab.stream import NodeLinkDataAccess, log
from tatolab.stream._function_node_declaration import (
    NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP,
    FunctionNode,
)
from tatolab.stream._node_declaration import NODE_DECLARATION_CONFIG_CLASS_STAMP
from tatolab.stream._rust_function_node_declaration import (
    RustFunctionNode,
    RustFunctionNodeDeclaration,
)

from tatolab.runtime._engine import (
    MODULE_HOST_TABLE_ABI_MAJOR,
    MODULE_HOST_TABLE_ABI_MINOR,
    feed_an_input_port_from_a_function_node_island,
    stop_feeding_an_input_port_from_a_function_node_island,
)

#: The value type a function node signature gives an `UnreadMessage` input or output. Spelled
#: here, not imported, because the `tatolab-stream` in a project's venv may predate it, and the
#: string is the wire contract both sides hold.
UNREAD_MESSAGE_VALUE_TYPE = "unread_message"

# The scalar value types an evaluate entry carries, each as an f64.
_SCALAR_VALUE_TYPES_AN_EVALUATE_ENTRY_CARRIES = {float: "float", int: "int", bool: "bool"}

# The value types a body run as Python takes and gives: the scalars, a `dict` as Records, and a
# message it hands on unread.
_VALUE_TYPES_A_BODY_RUN_AS_PYTHON_CARRIES = (
    "float",
    "int",
    "bool",
    "records",
    UNREAD_MESSAGE_VALUE_TYPE,
)

# The largest integer magnitude an f64 holds exactly.
_LARGEST_INTEGER_AN_F64_HOLDS_EXACTLY = 2**53

# The status the evaluate entry answers when the body raises; `MODULE_HOST_TABLE_STATUS_REFUSED`.
_EVALUATE_ENTRY_STATUS_REFUSED = -1


class FunctionNodeDoesNotCompile(Exception):
    """A function node whose body or signature the evaluate entry cannot carry."""


# What a node whose every value is a number, a bool or a dict carries beyond numbers: nothing.
_NO_VALUES_CARRIED_BEYOND_NUMBERS: "dict[str, list[int]]" = {
    "inputs_that_take_none": [],
    "inputs_carrying_an_unread_message": [],
    "outputs_carrying_an_unread_message": [],
    "outputs_that_may_hand_out_nothing": [],
}


def _how_the_values_of_a_signature_are_carried(
    function_signature: "dict[str, Any]",
) -> "dict[str, list[int]]":
    """Which inputs and outputs of `function_signature` take `None`, carry an `UnreadMessage`, or may hand out nothing."""
    inputs = function_signature["inputs"]
    outputs = function_signature["outputs"]
    return {
        "inputs_that_take_none": [
            index for index, each_input in enumerate(inputs) if each_input["optional"]
        ],
        "inputs_carrying_an_unread_message": [
            index
            for index, each_input in enumerate(inputs)
            if each_input["value_type"] == UNREAD_MESSAGE_VALUE_TYPE
        ],
        "outputs_carrying_an_unread_message": [
            index
            for index, each_output in enumerate(outputs)
            if each_output["value_type"] == UNREAD_MESSAGE_VALUE_TYPE
        ],
        "outputs_that_may_hand_out_nothing": [
            index
            for index, each_output in enumerate(outputs)
            if each_output.get("may_hand_out_nothing")
        ],
    }


@dataclasses.dataclass(frozen=True)
class FunctionNodeCompiledToItsEvaluateEntry:
    """A function node type compiled to its evaluate entry, and the order its values travel in."""

    evaluate_entry_address: int
    input_ports: "tuple[str, ...]"
    pin_defaults_by_input_port: "dict[str, float]"
    output_ports: "tuple[str, ...]"
    config_fields: "tuple[str, ...]"
    config_field_defaults: "dict[str, float]"
    config_field_default_factories: "dict[str, Callable[[], Any]]"
    state_fields: "tuple[str, ...]"
    initial_state_values: "tuple[float, ...]"
    is_stateful: bool
    body_loaded_from_numbas_cache: bool
    compiled_wrapper_kept_alive: Any
    input_value_types: "tuple[str, ...]" = ()
    how_its_values_are_carried: "dict[str, list[int]]" = dataclasses.field(
        default_factory=lambda: dict(_NO_VALUES_CARRIED_BEYOND_NUMBERS)
    )


@dataclasses.dataclass(frozen=True)
class FunctionNodeRunAsPythonInItsReader:
    """A function node type whose body runs as Python in its reader's interpreter, and why it does not compile."""

    body: Callable[..., Any]
    function_signature: "dict[str, Any]"
    input_ports: "tuple[str, ...]"
    pin_defaults_by_input_port: "dict[str, float]"
    output_ports: "tuple[str, ...]"
    ports_carrying_records: "frozenset[str]"
    config_class: Optional[type]
    state_record_class: Optional[type]
    state_value_types: "dict[str, str]"
    initial_state_values: "tuple[float, ...]"
    is_stateful: bool
    why_it_does_not_compile: str
    input_value_types: "tuple[str, ...]" = ()
    how_its_values_are_carried: "dict[str, list[int]]" = dataclasses.field(
        default_factory=lambda: dict(_NO_VALUES_CARRIED_BEYOND_NUMBERS)
    )


FunctionNodeReadyToEvaluate = typing.Union[
    FunctionNodeCompiledToItsEvaluateEntry, FunctionNodeRunAsPythonInItsReader
]


class FunctionNodeIslandsFedToTheirPorts:
    """The islands a helper's input ports are fed from, kept for as long as the ports are read, and how each node runs.

    The native evaluator calls into what each feed prepared, so it is kept
    here. A port fed again is fed the new island in place of the old one: a
    node run as Python keeps its body object, so its state carries on, and the
    native evaluator carries on each other stateful node's state.
    """

    def __init__(
        self, link_data_access: NodeLinkDataAccess, load_function_node: Callable[[str], Any]
    ) -> None:
        self._link_data_access = link_data_access
        self._load_function_node = load_function_node
        self._prepared_by_import_path: "dict[str, FunctionNodeReadyToEvaluate]" = {}
        self._island_by_input_port: "dict[str, dict[str, Any]]" = {}
        self._body_run_as_python_by_input_port_and_node_id: (
            "dict[tuple[str, str], FunctionNodeBodyRunAsPython]"
        ) = {}

    @property
    def function_node_types_kept_alive(self) -> "list[FunctionNodeReadyToEvaluate]":
        """Every function node type a feed prepared."""
        return list(self._prepared_by_import_path.values())

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
        self._refuse_a_stateful_node_another_port_holds(island)
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
        bodies_run_as_python: "dict[tuple[str, str], FunctionNodeBodyRunAsPython]" = {}
        for island_node in island["nodes"]:
            node_id = island_node["node"]
            prepared = prepared_by_node_id[node_id]
            linked_exposed_config_fields = _the_exposed_config_fields_a_link_drives(island_node)
            called_through, config_values, config_value_indices = (
                self._how_one_function_node_is_called(
                    prepared,
                    input_port,
                    node_id,
                    island_node.get("config"),
                    linked_exposed_config_fields,
                )
            )
            if isinstance(called_through, FunctionNodeBodyRunAsPython):
                bodies_run_as_python[(input_port, node_id)] = called_through
            how_each_node_carries_its_values.append(
                {
                    **prepared.how_its_values_are_carried,
                    "inputs_read_from_links_from_outside_the_island": (
                        _the_inputs_read_from_links_from_outside_the_island(
                            prepared, island_node["links_into"], linked_exposed_config_fields
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
                    )
                    + _where_each_linked_exposed_config_field_takes_its_value(
                        island_node["links_into"],
                        linked_exposed_config_fields,
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
        self._forget_the_bodies_run_as_python_for(input_port)
        self._body_run_as_python_by_input_port_and_node_id.update(bodies_run_as_python)
        self._island_by_input_port[input_port] = island

    def stop_feeding_an_input_port(self, input_port: str) -> bool:
        """Stop feeding `input_port` from its island; whether an island fed it."""
        self._island_by_input_port.pop(input_port, None)
        self._forget_the_bodies_run_as_python_for(input_port)
        return stop_feeding_an_input_port_from_a_function_node_island(
            link_data_access=self._link_data_access, input_port=input_port
        )

    def _prepared(self, import_path: str) -> FunctionNodeReadyToEvaluate:
        if import_path not in self._prepared_by_import_path:
            self._prepared_by_import_path[import_path] = prepare_function_node_to_evaluate(
                self._load_function_node(import_path), import_path
            )
        return self._prepared_by_import_path[import_path]

    def _how_one_function_node_is_called(
        self,
        prepared: FunctionNodeReadyToEvaluate,
        input_port: str,
        node_id: str,
        config: Any,
        linked_exposed_config_fields: "tuple[str, ...]",
    ) -> "tuple[Any, list[float], list[int]]":
        body_run_as_python = self._body_run_as_python_by_input_port_and_node_id.get(
            (input_port, node_id)
        )
        if body_run_as_python is not None and body_run_as_python.runs(prepared):
            body_run_as_python.config_fields_set_by_the_inputs_past_its_own = (
                linked_exposed_config_fields
            )
            return body_run_as_python, [], []
        return how_one_function_node_is_called(
            prepared, node_id, config, linked_exposed_config_fields
        )

    def _refuse_a_stateful_node_another_port_holds(self, island: "dict[str, Any]") -> None:
        input_port_of_the_island_holding_each_stateful_node = {
            island_node["node"]: held_input_port
            for held_input_port, held_island in self._island_by_input_port.items()
            if held_input_port != island["input_port"]
            for island_node in held_island["nodes"]
            if self._prepared(island_node["import_path"]).is_stateful
        }
        for island_node in island["nodes"]:
            if self._prepared(island_node["import_path"]).is_stateful:
                _refuse_a_stateful_node_two_islands_hold(
                    island_node["node"],
                    island["input_port"],
                    input_port_of_the_island_holding_each_stateful_node,
                )

    def _forget_the_bodies_run_as_python_for(self, input_port: str) -> None:
        for input_port_and_node_id in [
            key for key in self._body_run_as_python_by_input_port_and_node_id if key[0] == input_port
        ]:
            del self._body_run_as_python_by_input_port_and_node_id[input_port_and_node_id]


def feed_each_input_port_from_its_function_node_island(
    link_data_access: NodeLinkDataAccess,
    islands: "list[dict[str, Any]]",
    load_function_node: Callable[[str], Any],
) -> FunctionNodeIslandsFedToTheirPorts:
    """Prepare every function node the setup command's `islands` carry and feed each to its port.

    A node is compiled when its body compiles, and run as Python otherwise.
    """
    fed = FunctionNodeIslandsFedToTheirPorts(link_data_access, load_function_node)
    for island in islands:
        fed.feed_an_input_port_from_its_island(island)
    return fed


def how_one_function_node_is_called(
    prepared: FunctionNodeReadyToEvaluate,
    node_id: str,
    config: Any,
    linked_exposed_config_fields: "tuple[str, ...]" = (),
) -> "tuple[Any, list[float], list[int]]":
    """What the native evaluator calls `node_id` through, the config values it hands the call,
    and the index among them each input past the node's own sets.

    A compiled node is called through its entry's address with its config as
    f64s; a node run as Python through an object holding its config and state.
    Each of `linked_exposed_config_fields` is an input past the node's own, in
    that order, setting the config field of its name at each evaluation.
    """
    if isinstance(prepared, FunctionNodeCompiledToItsEvaluateEntry):
        return (
            prepared.evaluate_entry_address,
            config_values_of_one_function_node(prepared, node_id, config),
            [
                _the_config_value_index_of(prepared, node_id, field)
                for field in linked_exposed_config_fields
            ],
        )
    body_run_as_python = FunctionNodeBodyRunAsPython(prepared, node_id, config)
    body_run_as_python.config_fields_set_by_the_inputs_past_its_own = linked_exposed_config_fields
    return body_run_as_python, [], []


def _the_config_value_index_of(
    compiled: FunctionNodeCompiledToItsEvaluateEntry, node_id: str, field: str
) -> int:
    if field not in compiled.config_fields:
        raise FunctionNodeDoesNotCompile(
            f"function node `{node_id}` exposes `{field}` as an input, which its config class "
            f"does not declare; its fields are {', '.join(compiled.config_fields) or 'none'}"
        )
    return compiled.config_fields.index(field)


def _the_exposed_config_fields_a_link_drives(island_node: "dict[str, Any]") -> "tuple[str, ...]":
    linked_input_ports = {link["port"] for link in island_node["links_into"]}
    return tuple(
        field
        for field in island_node.get("exposed_config_fields") or ()
        if field in linked_input_ports
    )


def _where_each_linked_exposed_config_field_takes_its_value(
    links_into: "list[dict[str, Any]]",
    linked_exposed_config_fields: "tuple[str, ...]",
    node_index_by_node_id: "dict[str, int]",
    output_ports_by_node_id: "dict[str, tuple[str, ...]]",
) -> "list[tuple[Optional[tuple[int, int]], float]]":
    linked_from_by_input_port = {link["port"]: link["from"] for link in links_into}
    return [
        (
            None
            if _is_read_through_the_reader(linked_from_by_input_port[field])
            else (
                node_index_by_node_id[linked_from_by_input_port[field]["node"]],
                _output_index(
                    output_ports_by_node_id,
                    linked_from_by_input_port[field]["node"],
                    linked_from_by_input_port[field]["port"],
                ),
            ),
            0.0,
        )
        for field in linked_exposed_config_fields
    ]


def _is_read_through_the_reader(linked_from: "dict[str, Any]") -> bool:
    """Whether a link into an island node comes from a node outside the island, whose channel its reader reads."""
    return "reader_input_port" in linked_from


def _the_inputs_read_from_links_from_outside_the_island(
    prepared: FunctionNodeReadyToEvaluate,
    links_into: "list[dict[str, Any]]",
    linked_exposed_config_fields: "tuple[str, ...]",
) -> "list[tuple[int, str, str]]":
    """Each input a link from outside the island feeds: its index, the reader input port it is read at, and its value type.

    A linked exposed config field is an input past the node's own, read as a number.
    """
    linked_from_by_input_port = {link["port"]: link["from"] for link in links_into}
    value_type_by_input_port = dict(zip(prepared.input_ports, prepared.input_value_types))
    inputs_read: "list[tuple[int, str, str]]" = []
    for input_index, input_port in enumerate(
        (*prepared.input_ports, *linked_exposed_config_fields)
    ):
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


def prepare_function_node_to_evaluate(function_node: Any, import_path: str) -> FunctionNodeReadyToEvaluate:
    """`function_node` compiled to its evaluate entry, or ready to run as Python when its body does not compile.

    What neither path carries is refused by name.
    """
    if isinstance(function_node, RustFunctionNode):
        return _the_evaluate_entry_a_rust_function_node_exports(function_node, import_path)
    run_as_python = function_node_run_as_python_in_its_reader(function_node, import_path, "")
    try:
        return compile_function_node_to_its_evaluate_entry(function_node, import_path)
    except FunctionNodeDoesNotCompile as does_not_compile:
        log.info(
            f"{import_path} runs as Python in its reader's interpreter, since it does not "
            f"compile: {does_not_compile}"
        )
        return dataclasses.replace(run_as_python, why_it_does_not_compile=str(does_not_compile))


def function_node_run_as_python_in_its_reader(
    function_node: Any, import_path: str, why_it_does_not_compile: str
) -> FunctionNodeRunAsPythonInItsReader:
    """`function_node` ready to run as Python, refusing by name a signature a body run as Python cannot carry."""
    if not isinstance(function_node, FunctionNode):
        raise FunctionNodeDoesNotCompile(
            f"{import_path} names {function_node!r}, which is not a `@node.function` function"
        )
    function_signature: "dict[str, Any]" = getattr(
        function_node, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP
    )
    body: Callable[..., Any] = function_node.__wrapped__
    annotations = typing.get_type_hints(body)
    for each_port in (*function_signature["inputs"], *function_signature["outputs"]):
        if each_port["value_type"] not in _VALUE_TYPES_A_BODY_RUN_AS_PYTHON_CARRIES:
            raise FunctionNodeDoesNotCompile(
                f"{import_path}'s port `{each_port['port']}` is {each_port['value_type']}, and a "
                f"function node carries numbers, bools and dicts in this release. To fix: make "
                f"it `float`, `int`, `bool` or `dict`."
            )
    for each_input in function_signature["inputs"]:
        if each_input["optional"] and each_input["value_type"] != UNREAD_MESSAGE_VALUE_TYPE:
            raise FunctionNodeDoesNotCompile(
                f"{import_path}'s input `{each_input['parameter']}` is optional, and a function "
                f"node takes a value on every input but an `UnreadMessage` in this release. To "
                f"fix: give it a default."
            )
    state_parameter: Optional[str] = function_signature["state_parameter"]
    state_record_class: Optional[type] = (
        None if state_parameter is None else typing.get_args(annotations[state_parameter])[0]
    )
    state_value_types = _scalar_field_value_types_or_none(state_record_class) or {}
    return FunctionNodeRunAsPythonInItsReader(
        body=body,
        function_signature=function_signature,
        input_ports=tuple(each_input["port"] for each_input in function_signature["inputs"]),
        pin_defaults_by_input_port=_pin_defaults_by_input_port(import_path, function_signature),
        output_ports=tuple(each_output["port"] for each_output in function_signature["outputs"]),
        ports_carrying_records=frozenset(
            each_port["port"]
            for each_port in (*function_signature["inputs"], *function_signature["outputs"])
            if each_port["value_type"] == "records"
        ),
        config_class=getattr(function_node, NODE_DECLARATION_CONFIG_CLASS_STAMP, None),
        state_record_class=state_record_class,
        state_value_types=state_value_types,
        initial_state_values=(
            _initial_state_values(import_path, state_record_class, state_value_types)
            if state_value_types
            else ()
        ),
        is_stateful=state_record_class is not None,
        why_it_does_not_compile=why_it_does_not_compile,
        input_value_types=tuple(each_input["value_type"] for each_input in function_signature["inputs"]),
        how_its_values_are_carried=_how_the_values_of_a_signature_are_carried(function_signature),
    )


class FunctionNodeBodyRunAsPython:
    """One function node's body, run as Python with its config and state; the native evaluator calls `evaluate`."""

    def __init__(
        self, run_as_python: FunctionNodeRunAsPythonInItsReader, node_id: str, config: Any
    ) -> None:
        self._run_as_python = run_as_python
        self._node_id = node_id
        self._config = _config_record_of_one_function_node(run_as_python, node_id, config)
        self.config_fields_set_by_the_inputs_past_its_own: "tuple[str, ...]" = ()
        state_record_class = run_as_python.state_record_class
        self._state = state_record_class() if state_record_class is not None else None
        signature = run_as_python.function_signature
        self._value_type_by_input_parameter = {
            each_input["parameter"]: each_input["value_type"] for each_input in signature["inputs"]
        }
        self._input_parameters = list(self._value_type_by_input_parameter)
        self._positional_only_parameters = [
            parameter.name
            for parameter in inspect.signature(run_as_python.body).parameters.values()
            if parameter.kind is inspect.Parameter.POSITIONAL_ONLY
        ]

    def take_the_config(self, config: Any) -> None:
        """Call the body with `config` from the next evaluation on."""
        self._config = _config_record_of_one_function_node(self._run_as_python, self._node_id, config)

    def runs(self, run_as_python: FunctionNodeReadyToEvaluate) -> bool:
        """Whether this is the body of `run_as_python`, so an island fed again can call it with its state."""
        return self._run_as_python is run_as_python

    def evaluate(self, input_values: "list[Any]", now_ns: int) -> "list[Any]":
        """Call the body with `input_values` in port order at `now_ns`, answering its outputs in port order.

        A number or bool output is answered as a `float`, a `dict` as given.
        The new state is kept only once every output is answered.
        """
        signature = self._run_as_python.function_signature
        self._take_the_config_fields_its_inputs_past_its_own_set(
            input_values[len(self._input_parameters) :]
        )
        arguments: "dict[str, Any]" = {}
        for parameter_name, input_value in zip(self._input_parameters, input_values):
            arguments[parameter_name] = self._the_input_the_body_takes(
                parameter_name, self._value_type_by_input_parameter[parameter_name], input_value
            )
        if signature["time_parameter"] is not None:
            arguments[signature["time_parameter"]] = now_ns
        state_handed_to_the_body = None
        if signature["state_parameter"] is not None:
            state_handed_to_the_body = copy.deepcopy(self._state)
            arguments[signature["state_parameter"]] = state_handed_to_the_body
        if signature["config_parameter"] is not None:
            arguments[signature["config_parameter"]] = self._config
        positional_arguments = [
            arguments.pop(parameter_name) for parameter_name in self._positional_only_parameters
        ]
        returned = self._run_as_python.body(*positional_arguments, **arguments)
        if signature["state_parameter"] is not None:
            output, new_state = returned
        else:
            output, new_state = returned, self._state
        outputs = signature["outputs"]
        output_values = [
            output if each_output["field"] is None else getattr(output, each_output["field"])
            for each_output in outputs
        ]
        values_handed_to_be_handed_on_unread = [
            input_value
            for parameter_name, input_value in zip(self._input_parameters, input_values)
            if self._value_type_by_input_parameter[parameter_name] == UNREAD_MESSAGE_VALUE_TYPE
        ] + _the_field_values_of(state_handed_to_the_body)
        answered = [
            self._the_output_the_evaluator_takes(
                each_output, output_value, values_handed_to_be_handed_on_unread
            )
            for each_output, output_value in zip(outputs, output_values)
        ]
        self._the_state_values_of(new_state)
        self._state = new_state
        return answered

    def _take_the_config_fields_its_inputs_past_its_own_set(self, input_values: "list[Any]") -> None:
        changed_fields: "dict[str, Any]" = {}
        for field, input_value in zip(self.config_fields_set_by_the_inputs_past_its_own, input_values):
            value = the_value_a_config_field_takes(getattr(self._config, field), float(input_value))
            if value != getattr(self._config, field):
                changed_fields[field] = value
        if changed_fields:
            self._config = dataclasses.replace(self._config, **changed_fields)

    def state_values(self) -> "list[float]":
        """The node's state as the f64s its stream carries, one per field of a state record of numbers and bools."""
        return self._the_state_values_of(self._state)

    def _the_state_values_of(self, state: Any) -> "list[float]":
        return [
            _exactly_as_f64(getattr(state, field), f"function node `{self._node_id}`'s state `{field}`")
            for field in self._run_as_python.state_value_types
        ]

    def resume_from_state_values(self, state_values: "list[float]") -> None:
        """Resume from the state its stream carries, as `state_values` gives it."""
        state_record_class = self._run_as_python.state_record_class
        assert state_record_class is not None
        self._state = state_record_class(
            **{
                field: _python_value_of_an_f64(state_value, value_type)
                for (field, value_type), state_value in zip(
                    self._run_as_python.state_value_types.items(), state_values
                )
            }
        )

    def _the_input_the_body_takes(self, parameter_name: str, value_type: str, input_value: Any) -> Any:
        if value_type == UNREAD_MESSAGE_VALUE_TYPE:
            return input_value
        if value_type == "records":
            if not isinstance(input_value, dict):
                raise TypeError(
                    f"function node `{self._node_id}`'s input `{parameter_name}` takes a dict, "
                    f"and was handed {type(input_value).__name__}"
                )
            return copy.deepcopy(input_value)
        return _python_value_of_an_f64(float(input_value), value_type)

    def _the_output_the_evaluator_takes(
        self,
        each_output: "dict[str, Any]",
        output_value: Any,
        values_handed_to_be_handed_on_unread: "list[Any]",
    ) -> Any:
        what_it_is = f"function node `{self._node_id}`'s output `{each_output['port']}`"
        if output_value is None and each_output.get("may_hand_out_nothing"):
            return None
        if each_output["value_type"] == UNREAD_MESSAGE_VALUE_TYPE:
            if output_value is not None and any(
                output_value is handed for handed in values_handed_to_be_handed_on_unread
            ):
                return output_value
            if output_value is None or isinstance(output_value, (bool, int, float, dict)):
                raise TypeError(
                    f"{what_it_is} hands on a message it was handed, and the body gave "
                    f"{type(output_value).__name__}"
                )
            return output_value
        if each_output["value_type"] == "records":
            if not isinstance(output_value, dict):
                raise TypeError(f"{what_it_is} is a dict, and the body gave {type(output_value).__name__}")
            return output_value
        if not isinstance(output_value, (bool, int, float)):
            raise TypeError(
                f"{what_it_is} is {each_output['value_type']}, and the body gave "
                f"{type(output_value).__name__}"
            )
        return _exactly_as_f64(output_value, what_it_is)


def _the_field_values_of(state_record: Any) -> "list[Any]":
    if state_record is None or not dataclasses.is_dataclass(state_record):
        return []
    return [getattr(state_record, field.name) for field in dataclasses.fields(state_record)]


def the_value_a_config_field_takes(value_it_holds: Any, input_value: float) -> Any:
    """`input_value` as the type of the value the config field holds: a bool, an int when it is whole, else a float."""
    if isinstance(value_it_holds, bool):
        return input_value != 0.0
    if isinstance(value_it_holds, int) and input_value.is_integer():
        return int(input_value)
    return input_value


def _config_record_of_one_function_node(
    run_as_python: FunctionNodeRunAsPythonInItsReader, node_id: str, config: Any
) -> Any:
    config_values_by_field = dict(config) if isinstance(config, dict) else {}
    config_class = run_as_python.config_class
    if config_class is None:
        if config_values_by_field:
            raise FunctionNodeDoesNotCompile(
                f"function node `{node_id}`'s config sets "
                f"{', '.join(sorted(config_values_by_field))}, and it takes no config"
            )
        return None
    declared_fields = [field.name for field in dataclasses.fields(config_class)]
    fields_it_does_not_declare = sorted(set(config_values_by_field) - set(declared_fields))
    if fields_it_does_not_declare:
        raise FunctionNodeDoesNotCompile(
            f"function node `{node_id}`'s config sets {', '.join(fields_it_does_not_declare)}, "
            f"which its config class does not declare; its fields are "
            f"{', '.join(declared_fields) or 'none'}"
        )
    try:
        return _a_dataclass_built_from_its_fields(config_class, config_values_by_field)
    except TypeError as config_refused:
        raise FunctionNodeDoesNotCompile(
            f"function node `{node_id}`'s config does not build its config class: {config_refused}"
        ) from config_refused


def _a_dataclass_built_from_its_fields(dataclass_type: type, values_by_field: "dict[str, Any]") -> Any:
    try:
        field_types: "dict[str, Any]" = typing.get_type_hints(dataclass_type)
    except NameError:
        field_types = {field.name: field.type for field in dataclasses.fields(dataclass_type)}
    return dataclass_type(
        **{
            field: (
                _a_dataclass_built_from_its_fields(field_types[field], value)
                if isinstance(value, dict) and dataclasses.is_dataclass(field_types.get(field))
                else value
            )
            for field, value in values_by_field.items()
        }
    )


def _pin_defaults_by_input_port(import_path: str, function_signature: "dict[str, Any]") -> "dict[str, float]":
    return {
        each_input["port"]: _exactly_as_f64(
            each_input["default"], f"{import_path}'s input `{each_input['parameter']}` default"
        )
        for each_input in function_signature["inputs"]
        if each_input.get("default") is not None
    }


def _python_value_of_an_f64(value: float, value_type: str) -> Any:
    if value_type == "int":
        return int(value)
    if value_type == "bool":
        return value != 0.0
    return value


def _scalar_field_value_types_or_none(record_class: Optional[type]) -> "Optional[dict[str, str]]":
    """Each field's value type when every field of `record_class` is a number or a bool, else `None`."""
    if record_class is None:
        return None
    field_annotations = typing.get_type_hints(record_class)
    value_types: "dict[str, str]" = {}
    for field in dataclasses.fields(record_class):
        value_type = _SCALAR_VALUE_TYPES_AN_EVALUATE_ENTRY_CARRIES.get(field_annotations[field.name])
        if value_type is None:
            return None
        value_types[field.name] = value_type
    return value_types


def _refuse_a_stateful_node_two_islands_hold(
    node_id: str,
    input_port: str,
    input_port_of_the_island_holding_each_stateful_node: "dict[str, str]",
) -> None:
    first_input_port = input_port_of_the_island_holding_each_stateful_node.setdefault(
        node_id, input_port
    )
    if first_input_port != input_port:
        raise FunctionNodeDoesNotCompile(
            f"stateful function node `{node_id}` feeds both `{first_input_port}` and "
            f"`{input_port}`, and each input evaluates its own island, so the two would keep "
            f"two state histories. A stateful node feeds one input in this release. To fix: "
            f"give each input its own node."
        )


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
) -> "list[tuple[Optional[tuple[int, int]], float]]":
    """Each input's earlier node output, or else its pin value, or else its declared default."""
    linked_from_by_input_port = {link["port"]: link["from"] for link in links_into}
    where_each_input_takes_its_value: "list[tuple[Optional[tuple[int, int]], float]]" = []
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
                (None, _a_pin_value_as_f64(node_id, input_port, pin_values_by_input_port[input_port]))
            )
        elif input_port in prepared.pin_defaults_by_input_port:
            where_each_input_takes_its_value.append(
                (None, prepared.pin_defaults_by_input_port[input_port])
            )
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


def config_values_of_one_function_node(
    compiled: FunctionNodeCompiledToItsEvaluateEntry, node_id: str, config: Any
) -> "list[float]":
    """`node_id`'s config, as the f64s its evaluate entry takes, each field in its declared order."""
    config_values_by_field = dict(config) if isinstance(config, dict) else {}
    fields_it_does_not_declare = sorted(set(config_values_by_field) - set(compiled.config_fields))
    if fields_it_does_not_declare:
        raise FunctionNodeDoesNotCompile(
            f"function node `{node_id}`'s config sets {', '.join(fields_it_does_not_declare)}, "
            f"which its config class does not declare; its fields are "
            f"{', '.join(compiled.config_fields) or 'none'}"
        )
    config_values: "list[float]" = []
    for field in compiled.config_fields:
        what_it_is = f"function node `{node_id}`'s config `{field}`"
        if field in config_values_by_field:
            config_values.append(_exactly_as_f64(config_values_by_field[field], what_it_is))
        elif field in compiled.config_field_defaults:
            config_values.append(compiled.config_field_defaults[field])
        elif field in compiled.config_field_default_factories:
            config_values.append(
                _exactly_as_f64(compiled.config_field_default_factories[field](), what_it_is)
            )
        else:
            raise FunctionNodeDoesNotCompile(
                f"function node `{node_id}`'s config sets no `{field}`, and the field declares "
                f"no default"
            )
    return config_values


def compile_function_node_to_its_evaluate_entry(
    function_node: Any, import_path: str
) -> FunctionNodeCompiledToItsEvaluateEntry:
    """Compile `function_node`'s body with Numba to its evaluate entry, refusing by name what does not compile.

    A Rust function node exports its entry already; it is read, never compiled.
    """
    if isinstance(function_node, RustFunctionNode):
        return _the_evaluate_entry_a_rust_function_node_exports(function_node, import_path)
    if not isinstance(function_node, FunctionNode):
        raise FunctionNodeDoesNotCompile(
            f"{import_path} names {function_node!r}, which is not a `@node.function` function"
        )
    function_signature: "dict[str, Any]" = getattr(
        function_node, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP
    )
    body: Callable[..., Any] = function_node.__wrapped__
    annotations = typing.get_type_hints(body)
    for each_input in function_signature["inputs"]:
        _refuse_a_value_the_evaluate_entry_cannot_carry(
            import_path, f"input `{each_input['parameter']}`", each_input["value_type"]
        )
        if each_input["optional"] and each_input["value_type"] != UNREAD_MESSAGE_VALUE_TYPE:
            raise FunctionNodeDoesNotCompile(
                f"{import_path}'s input `{each_input['parameter']}` is optional, and a compiled "
                f"function node takes a number on every input but an `UnreadMessage`. To fix: "
                f"give it a numeric default."
            )
    for each_output in function_signature["outputs"]:
        _refuse_a_value_the_evaluate_entry_cannot_carry(
            import_path, f"output `{each_output['port']}`", each_output["value_type"]
        )

    config_class: Optional[type] = getattr(function_node, NODE_DECLARATION_CONFIG_CLASS_STAMP, None)
    state_parameter: Optional[str] = function_signature["state_parameter"]
    state_record_class: Optional[type] = (
        None if state_parameter is None else typing.get_args(annotations[state_parameter])[0]
    )
    output_record_class = _output_record_class(annotations["return"], state_record_class)
    config_value_types = _scalar_field_value_types(import_path, "config", config_class)
    state_value_types = _scalar_field_value_types(import_path, "state", state_record_class)

    named_tuple_by_dataclass = {
        dataclass_lowered: collections.namedtuple(  # pyright: ignore[reportUntypedNamedTuple]
            dataclass_lowered.__name__,
            [field.name for field in dataclasses.fields(dataclass_lowered)],
        )
        for dataclass_lowered in (config_class, state_record_class, output_record_class)
        if dataclass_lowered is not None
    }
    try:
        import numba
        from numba.np.numpy_support import carray
    except ImportError as numba_missing:
        raise FunctionNodeDoesNotCompile(
            f"{import_path} is a function node, and Numba, which compiles function nodes, is "
            f"not installed in this stream's environment. To compile it: install `numba`."
        ) from numba_missing

    constructor_by_dataclass = {
        dataclass_lowered: numba.njit(
            _a_constructor_keeping_the_dataclass_defaults(import_path, dataclass_lowered, named_tuple)
        )
        for dataclass_lowered, named_tuple in named_tuple_by_dataclass.items()
        if dataclass_lowered is not config_class
    }
    body_with_its_dataclasses_lowered = numba.njit(cache=True, error_model="numpy")(
        _the_body_with_its_dataclasses_lowered(body, constructor_by_dataclass)
    )
    wrapper_source, wrapper_globals = _evaluate_entry_wrapper_source(
        body,
        function_signature,
        config_class,
        config_value_types,
        state_record_class,
        state_value_types,
        output_record_class,
        named_tuple_by_dataclass,
    )
    wrapper_globals["carray"] = carray
    wrapper_globals["body_with_its_dataclasses_lowered"] = body_with_its_dataclasses_lowered
    exec(compile(wrapper_source, f"<evaluate entry of {import_path}>", "exec"), wrapper_globals)
    float64_pointer = numba.types.CPointer(numba.types.float64)
    evaluate_entry_signature = numba.types.int32(
        float64_pointer, float64_pointer, float64_pointer, float64_pointer, numba.types.int64
    )
    try:
        compiled_wrapper = numba.cfunc(evaluate_entry_signature, error_model="numpy")(
            wrapper_globals["evaluate"]
        )
    except Exception as does_not_compile:
        # Numba refuses a body through `NumbaError` and through exceptions of
        # its own outside it, such as `UnsupportedBytecodeError`.
        raise FunctionNodeDoesNotCompile(
            f"{import_path}'s body does not compile to native code. Numba said: "
            f"{type(does_not_compile).__name__}: {does_not_compile}"
        ) from does_not_compile

    evaluate_entry_address = compiled_wrapper.address
    if evaluate_entry_address is None:
        raise FunctionNodeDoesNotCompile(f"{import_path} compiled to no evaluate entry address")

    pin_defaults_by_input_port = _pin_defaults_by_input_port(import_path, function_signature)
    config_fields = dataclasses.fields(config_class) if config_class is not None else ()
    config_field_defaults = {
        field.name: _exactly_as_f64(field.default, f"{import_path}'s config `{field.name}` default")
        for field in config_fields
        if field.default is not dataclasses.MISSING
    }
    config_field_default_factories: "dict[str, Callable[[], Any]]" = {
        field.name: field.default_factory
        for field in config_fields
        if field.default_factory is not dataclasses.MISSING
    }
    return FunctionNodeCompiledToItsEvaluateEntry(
        evaluate_entry_address=evaluate_entry_address,
        input_ports=tuple(each_input["port"] for each_input in function_signature["inputs"]),
        pin_defaults_by_input_port=pin_defaults_by_input_port,
        output_ports=tuple(each_output["port"] for each_output in function_signature["outputs"]),
        config_fields=tuple(config_value_types),
        config_field_defaults=config_field_defaults,
        config_field_default_factories=config_field_default_factories,
        state_fields=tuple(state_value_types),
        initial_state_values=_initial_state_values(import_path, state_record_class, state_value_types),
        is_stateful=state_record_class is not None,
        body_loaded_from_numbas_cache=sum(body_with_its_dataclasses_lowered.stats.cache_hits.values())
        > 0,
        compiled_wrapper_kept_alive=compiled_wrapper,
        input_value_types=tuple(each_input["value_type"] for each_input in function_signature["inputs"]),
        how_its_values_are_carried=_how_the_values_of_a_signature_are_carried(function_signature),
    )


def _the_evaluate_entry_a_rust_function_node_exports(
    function_node: RustFunctionNode, import_path: str
) -> FunctionNodeCompiledToItsEvaluateEntry:
    declaration = function_node.rust_function_node_declaration
    _refuse_a_rust_function_node_built_for_another_module_host_table(declaration, import_path)
    function_signature: "dict[str, Any]" = getattr(
        function_node, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP
    )
    return FunctionNodeCompiledToItsEvaluateEntry(
        evaluate_entry_address=declaration.evaluate_entry_address,
        input_ports=tuple(input_pin.port for input_pin in declaration.input_pins),
        pin_defaults_by_input_port={
            input_pin.port: input_pin.pin_default
            for input_pin in declaration.input_pins
            if input_pin.pin_default is not None
        },
        output_ports=tuple(output_port.name for output_port in declaration.output_ports),
        config_fields=tuple(config_field.name for config_field in declaration.config_fields),
        config_field_defaults={
            config_field.name: config_field.default for config_field in declaration.config_fields
        },
        config_field_default_factories={},
        state_fields=tuple(state_field.name for state_field in declaration.state_fields),
        initial_state_values=tuple(state_field.default for state_field in declaration.state_fields),
        is_stateful=declaration.state_parameter is not None,
        body_loaded_from_numbas_cache=False,
        compiled_wrapper_kept_alive=function_node,
        input_value_types=tuple(input_pin.value_type for input_pin in declaration.input_pins),
        how_its_values_are_carried=_how_the_values_of_a_signature_are_carried(function_signature),
    )


def _refuse_a_rust_function_node_built_for_another_module_host_table(
    declaration: RustFunctionNodeDeclaration, import_path: str
) -> None:
    built_for = (
        f"{declaration.module_host_table_abi_major}.{declaration.module_host_table_abi_minor}"
    )
    runtime_serves = f"{MODULE_HOST_TABLE_ABI_MAJOR}.{MODULE_HOST_TABLE_ABI_MINOR}"
    if declaration.module_host_table_abi_major != MODULE_HOST_TABLE_ABI_MAJOR:
        raise FunctionNodeDoesNotCompile(
            f"{import_path} is a Rust function node built against module host table {built_for}, "
            f"and this runtime serves {runtime_serves}, another major. To fix: rebuild its "
            f"module against a `tatolab-module-sdk` {MODULE_HOST_TABLE_ABI_MAJOR}.x."
        )
    if declaration.module_host_table_abi_minor > MODULE_HOST_TABLE_ABI_MINOR:
        raise FunctionNodeDoesNotCompile(
            f"{import_path} is a Rust function node built against module host table {built_for}, "
            f"newer than the {runtime_serves} this runtime serves. To fix: update the runtime, or "
            f"build its module against an older `tatolab-module-sdk`."
        )


def _initial_state_values(
    import_path: str, state_record_class: Optional[type], state_value_types: "dict[str, str]"
) -> "tuple[float, ...]":
    """The state a new node of this type starts from: one record of its defaults, as f64s."""
    if state_record_class is None:
        return ()
    initial_state = state_record_class()
    return tuple(
        _exactly_as_f64(getattr(initial_state, field), f"{import_path}'s state `{field}` default")
        for field in state_value_types
    )


def _refuse_a_value_the_evaluate_entry_cannot_carry(
    import_path: str, what_it_types: str, value_type: str
) -> None:
    if (
        value_type not in _SCALAR_VALUE_TYPES_AN_EVALUATE_ENTRY_CARRIES.values()
        and value_type != UNREAD_MESSAGE_VALUE_TYPE
    ):
        raise FunctionNodeDoesNotCompile(
            f"{import_path}'s {what_it_types} is {value_type}, and a compiled function node "
            f"carries numbers, bools and `UnreadMessage`s in this release. To fix: make it "
            f"`float`, `int` or `bool`."
        )


def _output_record_class(return_annotation: Any, state_record_class: Optional[type]) -> Optional[type]:
    output_annotation = (
        typing.get_args(return_annotation)[0] if state_record_class is not None else return_annotation
    )
    if isinstance(output_annotation, type) and dataclasses.is_dataclass(output_annotation):
        return output_annotation
    return None


def _scalar_field_value_types(
    import_path: str, what_the_record_is: str, record_class: Optional[type]
) -> "dict[str, str]":
    if record_class is None:
        return {}
    field_annotations = typing.get_type_hints(record_class)
    value_types: "dict[str, str]" = {}
    for field in dataclasses.fields(record_class):
        value_type = _SCALAR_VALUE_TYPES_AN_EVALUATE_ENTRY_CARRIES.get(field_annotations[field.name])
        if value_type is None:
            raise FunctionNodeDoesNotCompile(
                f"{import_path}'s {what_the_record_is} field `{field.name}` is "
                f"{field_annotations[field.name]!r}, and a compiled function node carries "
                f"numbers and bools in this release. To fix: make it `float`, `int` or `bool`."
            )
        value_types[field.name] = value_type
    return value_types


def _a_constructor_keeping_the_dataclass_defaults(
    import_path: str, dataclass_lowered: type, named_tuple: type
) -> Callable[..., Any]:
    """A function building `named_tuple` from `dataclass_lowered`'s fields, defaulting
    each field its dataclass gives a plain default; Numba's named tuples take none."""
    constructor_globals: "dict[str, Any]" = {"named_tuple": named_tuple}
    parameters: "list[str]" = []
    for field in dataclasses.fields(dataclass_lowered):
        if field.default_factory is not dataclasses.MISSING:
            raise FunctionNodeDoesNotCompile(
                f"{import_path}'s body builds `{dataclass_lowered.__qualname__}`, whose field "
                f"`{field.name}` defaults through `default_factory`, and a compiled body cannot "
                f"call a Python factory. To fix: give `{field.name}` a plain default."
            )
        if field.default is dataclasses.MISSING:
            parameters.append(field.name)
        else:
            constructor_globals[f"default_{field.name}"] = field.default
            parameters.append(f"{field.name}=default_{field.name}")
    field_names = ", ".join(field.name for field in dataclasses.fields(dataclass_lowered))
    source = f"def construct({', '.join(parameters)}):\n    return named_tuple({field_names})\n"
    exec(
        compile(source, f"<constructor of {dataclass_lowered.__qualname__}>", "exec"),
        constructor_globals,
    )
    return constructor_globals["construct"]


def _the_body_with_its_dataclasses_lowered(
    body: Callable[..., Any], constructor_by_dataclass: "dict[type, Any]"
) -> Callable[..., Any]:
    """`body` building the named tuple in place of each dataclass its globals name,
    with its keyword-only parameters made positional."""
    lowered_globals = dict(body.__globals__)
    for global_name, global_value in body.__globals__.items():
        if isinstance(global_value, type) and global_value in constructor_by_dataclass:
            lowered_globals[global_name] = constructor_by_dataclass[global_value]
    code = body.__code__
    code_taking_every_parameter_positionally = code.replace(
        co_argcount=code.co_argcount + code.co_kwonlyargcount, co_kwonlyargcount=0
    )
    lowered = types.FunctionType(
        code_taking_every_parameter_positionally, lowered_globals, body.__name__
    )
    lowered.__module__ = body.__module__
    lowered.__qualname__ = body.__qualname__
    return lowered


def _evaluate_entry_wrapper_source(
    body: Callable[..., Any],
    function_signature: "dict[str, Any]",
    config_class: Optional[type],
    config_value_types: "dict[str, str]",
    state_record_class: Optional[type],
    state_value_types: "dict[str, str]",
    output_record_class: Optional[type],
    named_tuple_by_dataclass: "dict[type, type]",
) -> "tuple[str, dict[str, Any]]":
    wrapper_globals: "dict[str, Any]" = {}
    value_type_by_input_parameter = {
        each_input["parameter"]: each_input["value_type"] for each_input in function_signature["inputs"]
    }
    optional_input_parameters = {
        each_input["parameter"] for each_input in function_signature["inputs"] if each_input["optional"]
    }
    input_parameters = list(value_type_by_input_parameter)
    lines = ["def evaluate(input_values, config_values, state_values, output_values, now_ns):"]
    if input_parameters:
        lines.append(f"    inputs = carray(input_values, ({len(input_parameters)},))")
    if config_value_types:
        lines.append(f"    config = carray(config_values, ({len(config_value_types)},))")
    if state_value_types:
        lines.append(f"    state = carray(state_values, ({len(state_value_types)},))")
    output_ports = [each_output["port"] for each_output in function_signature["outputs"]]
    handed_out_slot_by_output_index = {
        output_index: len(output_ports) + handed_out_slot_index
        for handed_out_slot_index, output_index in enumerate(
            index
            for index, each_output in enumerate(function_signature["outputs"])
            if each_output.get("may_hand_out_nothing")
        )
    }
    lines.append(
        f"    outputs = carray(output_values, ({len(output_ports) + len(handed_out_slot_by_output_index)},))"
    )

    arguments: "list[str]" = []
    for parameter_name in inspect.signature(body).parameters:
        if parameter_name == function_signature["time_parameter"]:
            arguments.append("now_ns")
        elif parameter_name == function_signature["state_parameter"]:
            assert state_record_class is not None
            wrapper_globals["StateRecord"] = named_tuple_by_dataclass[state_record_class]
            arguments.append(
                "StateRecord("
                + ", ".join(
                    _from_f64(f"state[{index}]", value_type)
                    for index, value_type in enumerate(state_value_types.values())
                )
                + ")"
            )
        elif parameter_name == function_signature["config_parameter"]:
            assert config_class is not None
            wrapper_globals["ConfigRecord"] = named_tuple_by_dataclass[config_class]
            arguments.append(
                "ConfigRecord("
                + ", ".join(
                    _from_f64(f"config[{index}]", value_type)
                    for index, value_type in enumerate(config_value_types.values())
                )
                + ")"
            )
        else:
            input_expression = f"inputs[{input_parameters.index(parameter_name)}]"
            if parameter_name in optional_input_parameters:
                arguments.append(f"(None if {input_expression} == 0.0 else {input_expression})")
            else:
                arguments.append(
                    _from_f64(input_expression, value_type_by_input_parameter[parameter_name])
                )
    lines.append("    try:")
    lines.append(f"        returned = body_with_its_dataclasses_lowered({', '.join(arguments)})")
    lines.append("    except Exception:")
    lines.append(f"        return {_EVALUATE_ENTRY_STATUS_REFUSED}")
    if state_record_class is not None:
        lines.append("    output, new_state = returned")
        for index, (field_name, value_type) in enumerate(state_value_types.items()):
            lines.append(f"    state[{index}] = {_to_f64(f'new_state.{field_name}', value_type)}")
    else:
        lines.append("    output = returned")
    for index, each_output in enumerate(function_signature["outputs"]):
        output_expression = "output" if output_record_class is None else f"output.{each_output['field']}"
        handed_out_slot = handed_out_slot_by_output_index.get(index)
        if handed_out_slot is None:
            lines.append(f"    outputs[{index}] = {_to_f64(output_expression, each_output['value_type'])}")
            continue
        lines.append(f"    output_{index} = {output_expression}")
        lines.append(f"    if output_{index} is None:")
        lines.append(f"        outputs[{index}] = 0.0")
        lines.append(f"        outputs[{handed_out_slot}] = 0.0")
        lines.append("    else:")
        lines.append(
            f"        outputs[{index}] = "
            f"{_optional_to_f64(f'output_{index}', each_output['value_type'])}"
        )
        lines.append(f"        outputs[{handed_out_slot}] = 1.0")
    lines.append("    return 0")
    return "\n".join(lines) + "\n", wrapper_globals


def _a_pin_value_as_f64(node_id: str, input_port: str, pin_value: Any) -> float:
    what_it_is = f"function node `{node_id}`'s pin on input `{input_port}`"
    if not isinstance(pin_value, (bool, int, float)):
        raise FunctionNodeDoesNotCompile(
            f"{what_it_is} is {pin_value!r}, and an island input takes a number or a bool. "
            f"To fix: set the pin to a number, or link the input."
        )
    return _exactly_as_f64(pin_value, what_it_is)


def _exactly_as_f64(value: Any, what_it_is: str) -> float:
    """`value` as the f64 an evaluate entry carries, refusing an int it cannot hold exactly."""
    if isinstance(value, bool):
        return 1.0 if value else 0.0
    if isinstance(value, int) and abs(value) > _LARGEST_INTEGER_AN_F64_HOLDS_EXACTLY:
        raise FunctionNodeDoesNotCompile(
            f"{what_it_is} is {value}, and an evaluate entry carries each value as an f64, "
            f"which holds an integer exactly only up to 2**53"
        )
    return float(value)


def _to_f64(value_expression: str, value_type: str) -> str:
    if value_type == "bool":
        return f"(1.0 if {value_expression} else 0.0)"
    return f"float({value_expression})"


def _optional_to_f64(value_expression: str, value_type: str) -> str:
    """`_to_f64` of a value Numba types as optional, which its `float` refuses and its arithmetic unwraps."""
    if value_type == "bool":
        return f"(1.0 if {value_expression} else 0.0)"
    return f"(0.0 + {value_expression})"


def _from_f64(value_expression: str, value_type: str) -> str:
    if value_type == "int":
        return f"int({value_expression})"
    if value_type == "bool":
        return f"({value_expression} != 0.0)"
    return value_expression

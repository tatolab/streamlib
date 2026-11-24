# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The node a helper runs a function node as, once the node is promoted out of its readers' islands.

Each input reads the newest value its link carries, an island its own helper
evaluates or a channel from a class node or another promoted node, and keeps
its pin value until one arrives. A number input reads the first sample of a
Channels block, f32 or f64, exactly as an island reads one, a Color, Vector,
Rectangle or Transform input the first sample of each channel, and each
refuses any other message by name; an `UnreadMessage` input holds only the
message that arrived since the last call, and the node is not called while it
holds none. A live config update, and so each value a link gives a field the
node exposes, reaches the next call. A number or bool output is written as a
control message, one f64 sample on one channel at rate 0, a Color, Vector,
Rectangle or Transform output one f64 sample on each channel of its parts, a
String, Menu, Asset or List as its type packs, a Records message `{"value": ...}`
or an Array frame for a List of numbers or values of parts, which an input of
one reads back, a `dict` output as a Records message, a value of a named type
as the Bytes message its class packs, and an `UnreadMessage` output exactly as
the message arrived. A named type's input that may take `None` holds `None`
until a value arrives, and every other input holds back the call until one
does. A node whose output feeds a setting read at setup is called once as it
sets up too, so the setting's node, which the engine builds after it, is built
with its value.
"""

from __future__ import annotations

import json
import struct
from typing import Any, Optional

from tatolab.stream import NodeLinkDataAccess, log

from tatolab.runtime._node_the_wheel_hosts import (
    NODE_THE_WHEEL_BUILDS_TAKES_ITS_CONFIG,
    link_output_data_writer_of,
    stamp_the_ports_of_a_node_the_wheel_hosts,
)
from tatolab.runtime._engine import (
    FunctionNodeEvaluatedAsAStandaloneNode,
    hand_an_unread_message_on,
    read_the_control_value_at_an_input_port,
    read_the_message_at_an_input_port_unread,
    read_the_named_value_at_an_input_port,
    read_the_parts_at_an_input_port,
    read_the_stored_value_at_an_input_port,
    write_the_named_value_on_an_output_port,
    write_the_stored_value_on_an_output_port,
)
from tatolab.runtime._function_node_carried_values import NAMED_VALUE_TYPE, PART_VALUE_TYPES
from tatolab.runtime._function_node_stored_values import STORED_VALUE_TYPES
from tatolab.runtime._function_node_compilation import (
    FunctionNodeBodyRunAsPython,
    FunctionNodeCompiledToItsEvaluateEntry,
    FunctionNodeDoesNotCompile,
    FunctionNodeReadyToEvaluate,
    how_one_function_node_is_called,
    prepare_function_node_to_evaluate,
)

# A control message on a wire: one f64 sample on each channel, at no regular rate.
_CONTROL_VALUE_SAMPLE_RATE = 0


def the_node_class_a_promoted_function_node_runs_as(
    function_node: Any,
    import_path: str,
    node_id: str,
    link_data_access: NodeLinkDataAccess,
) -> type:
    """A node class whose `process` calls `function_node`, compiled or as Python, once per run of its loop."""
    prepared = prepare_function_node_to_evaluate(function_node, import_path)
    runner_class = type(
        f"FunctionNodeRunAsAStandaloneNode_{function_node.__name__}",
        (_FunctionNodeRunAsAStandaloneNode,),
        {
            "__doc__": f"Function node `{node_id}` ({import_path}), run in its own helper.",
            "__module__": __name__,
        },
    )
    runner_class._prepared = prepared
    runner_class._node_id = node_id
    runner_class._link_data_access = link_data_access
    declared = stamp_the_ports_of_a_node_the_wheel_hosts(
        runner_class, input_ports=prepared.input_ports, output_ports=prepared.output_ports
    )
    setattr(declared, NODE_THE_WHEEL_BUILDS_TAKES_ITS_CONFIG, True)
    setattr(declared, "__init__", _FunctionNodeRunAsAStandaloneNode.start_from_its_config)
    return declared


def evaluate_a_promoted_function_node_at_its_setup(node_class: Any) -> None:
    """Have `node_class`, the class a promoted function node runs as, call the node once at its setup; any other class is left as it is."""
    if isinstance(node_class, type) and issubclass(node_class, _FunctionNodeRunAsAStandaloneNode):
        node_class._evaluated_at_setup = True


def ran_compiled_by_node_id_of_the_function_node_a_standalone_node_runs(
    node_class: Any,
) -> "dict[str, bool]":
    """`{node_id: ran_compiled}` for the function node `node_class` runs once promoted; empty for any other class."""
    if not (
        isinstance(node_class, type)
        and issubclass(node_class, _FunctionNodeRunAsAStandaloneNode)
    ):
        return {}
    return {
        node_class._node_id: isinstance(
            node_class._prepared, FunctionNodeCompiledToItsEvaluateEntry
        )
    }


class _FunctionNodeRunAsAStandaloneNode:
    """Calls one function node with the newest value on each input and writes its outputs."""

    _prepared: FunctionNodeReadyToEvaluate
    _node_id: str
    _link_data_access: NodeLinkDataAccess
    _input_values: "list[Any]"
    _compiled_config_values_handed_to_each_call: "list[float]"
    _body_run_as_python: Optional[FunctionNodeBodyRunAsPython]
    _control_values_written: int
    _evaluation: FunctionNodeEvaluatedAsAStandaloneNode
    _evaluated_at_setup: bool = False

    def setup(self, ctx: Any) -> None:
        if not self._evaluated_at_setup:
            return
        try:
            self.process(ctx)
        except Exception as error:
            log.warn(
                "a promoted function node raised as it set up, so the setting it feeds is "
                "built with its own value",
                error=str(error),
            )

    def start_from_its_config(self, config: Any = None) -> None:
        """Take the node's config and start its state from its initial record; set as `__init__` once declared."""
        prepared = self._prepared
        self._input_values = [
            prepared.pin_defaults_by_input_port.get(input_port) for input_port in prepared.input_ports
        ]
        self._control_values_written = 0
        compiled_config_fields = _compiled_config_fields(prepared)
        called_through, config_values, compiled_config_value_indices = how_one_function_node_is_called(
            prepared, self._node_id, _config_as_a_mapping(config), compiled_config_fields
        )
        self._compiled_config_values_handed_to_each_call = list(config_values)
        self._body_run_as_python = (
            called_through if isinstance(called_through, FunctionNodeBodyRunAsPython) else None
        )
        self._evaluation = FunctionNodeEvaluatedAsAStandaloneNode(
            link_data_access=self._link_data_access,
            node_id=self._node_id,
            called_through=called_through,
            input_count=len(prepared.input_ports) + len(compiled_config_fields),
            config_values=config_values,
            config_value_indices_set_by_the_inputs_past_its_own=compiled_config_value_indices,
            initial_state_values=list(prepared.initial_state_values),
            output_count=len(prepared.output_ports),
            outputs_that_may_hand_out_nothing=list(
                prepared.how_its_values_are_carried["outputs_that_may_hand_out_nothing"]
            ),
            inputs_carrying_an_unread_message=list(
                prepared.how_its_values_are_carried["inputs_carrying_an_unread_message"]
            ),
            outputs_carrying_an_unread_message=list(
                prepared.how_its_values_are_carried["outputs_carrying_an_unread_message"]
            ),
            inputs_carried_in_parts=list(
                prepared.how_its_values_are_carried["inputs_carried_in_parts"]
            ),
            outputs_carried_in_parts=list(
                prepared.how_its_values_are_carried["outputs_carried_in_parts"]
            ),
        )

    def process(self, ctx: Any) -> None:
        prepared = self._prepared
        ports_carrying_an_unread_message = _ports_carrying_an_unread_message(prepared)
        value_type_by_input_port = dict(zip(prepared.input_ports, prepared.input_value_types))
        pin_types_of_stored_values = _pin_types_of_the_stored_values_it_carries(prepared)
        ports_carrying_a_named_value = _ports_carrying_a_named_value(prepared)
        for input_index, input_port in enumerate(prepared.input_ports):
            if input_port in ports_carrying_an_unread_message:
                self._input_values[input_index] = read_the_message_at_an_input_port_unread(
                    link_data_access=self._link_data_access, input_port=input_port
                )
                continue
            value_type = value_type_by_input_port.get(input_port, "float")
            if input_port in pin_types_of_stored_values:
                read: Any = read_the_stored_value_at_an_input_port(
                    link_data_access=self._link_data_access,
                    node_id=self._node_id,
                    input_port=input_port,
                    pin_type=json.dumps(pin_types_of_stored_values[input_port]),
                )
            elif value_type == NAMED_VALUE_TYPE:
                read = read_the_named_value_at_an_input_port(
                    link_data_access=self._link_data_access,
                    node_id=self._node_id,
                    input_port=input_port,
                )
            elif value_type in PART_VALUE_TYPES:
                read = read_the_parts_at_an_input_port(
                    link_data_access=self._link_data_access,
                    node_id=self._node_id,
                    input_port=input_port,
                    value_type=value_type,
                )
            else:
                read = read_the_control_value_at_an_input_port(
                    link_data_access=self._link_data_access, node_id=self._node_id, input_port=input_port
                )
            if read is not None:
                self._input_values[input_index] = read
        inputs_that_may_hold_none = _named_inputs_that_take_none(prepared)
        if any(
            value is None and input_port not in inputs_that_may_hold_none
            for input_port, value in zip(prepared.input_ports, self._input_values)
        ):
            return
        output_values = self._evaluation.evaluate(
            [*self._input_values, *self._compiled_config_values_handed_to_each_call]
        )
        for output_port, output_value in zip(prepared.output_ports, output_values):
            if output_value is None:
                continue
            if output_port in ports_carrying_an_unread_message:
                if isinstance(output_value, (int, float, dict, bytes)):
                    raise TypeError(
                        f"output {output_port!r} hands on an `UnreadMessage`, and the node gave {output_value!r}"
                    )
                hand_an_unread_message_on(
                    link_data_access=self._link_data_access,
                    output_port=output_port,
                    message=output_value,
                )
                continue
            if output_port in ports_carrying_a_named_value:
                if not isinstance(output_value, bytes):
                    raise TypeError(
                        f"output {output_port!r} carries a named type, and the node gave {output_value!r}"
                    )
                write_the_named_value_on_an_output_port(
                    link_data_access=self._link_data_access,
                    output_port=output_port,
                    payload=output_value,
                )
                continue
            if output_port in pin_types_of_stored_values:
                if not isinstance(output_value, dict):
                    raise TypeError(
                        f"output {output_port!r} carries a stored value, and the node gave {output_value!r}"
                    )
                write_the_stored_value_on_an_output_port(
                    link_data_access=self._link_data_access,
                    output_port=output_port,
                    pin_type=json.dumps(pin_types_of_stored_values[output_port]),
                    record=output_value,
                )
                continue
            parts = output_value if isinstance(output_value, tuple) else (output_value,)
            link_output_data_writer_of(ctx).write_channels_block(
                output_port,
                memoryview(struct.pack(f"={len(parts)}d", *parts)).cast("d", (len(parts), 1)),
                start_sample=self._control_values_written,
                sample_rate=_CONTROL_VALUE_SAMPLE_RATE,
            )
        self._control_values_written += 1


def _ports_carrying_an_unread_message(prepared: FunctionNodeReadyToEvaluate) -> "frozenset[str]":
    how_its_values_are_carried = prepared.how_its_values_are_carried
    return frozenset(
        [
            prepared.input_ports[input_index]
            for input_index in how_its_values_are_carried["inputs_carrying_an_unread_message"]
        ]
        + [
            prepared.output_ports[output_index]
            for output_index in how_its_values_are_carried["outputs_carrying_an_unread_message"]
        ]
    )


def _ports_carrying_a_named_value(prepared: FunctionNodeReadyToEvaluate) -> "frozenset[str]":
    """Each port of a named type, which a node declares on Records."""
    return frozenset(
        port
        for ports, value_types in (
            (prepared.input_ports, prepared.input_value_types),
            (prepared.output_ports, _the_output_value_types_of(prepared)),
        )
        for port, value_type in zip(ports, value_types)
        if value_type == NAMED_VALUE_TYPE
    )


def _named_inputs_that_take_none(prepared: FunctionNodeReadyToEvaluate) -> "frozenset[str]":
    """Each input of a named type that may take `None`, which the node is called without."""
    return frozenset(
        prepared.input_ports[input_index]
        for input_index in prepared.how_its_values_are_carried["inputs_that_take_none"]
        if prepared.input_value_types[input_index] == NAMED_VALUE_TYPE
    )


def _pin_types_of_the_stored_values_it_carries(
    prepared: FunctionNodeReadyToEvaluate,
) -> "dict[str, dict[str, Any]]":
    """The pin type of each port that carries a String, a Menu, an Asset or a List, by port."""
    return {
        port: pin_type
        for ports, value_types, pin_types in (
            (prepared.input_ports, prepared.input_value_types, prepared.input_pin_types),
            (
                prepared.output_ports,
                _the_output_value_types_of(prepared),
                prepared.output_pin_types,
            ),
        )
        for port, value_type, pin_type in zip(ports, value_types, pin_types)
        if value_type in STORED_VALUE_TYPES and pin_type is not None
    }


def _the_output_value_types_of(prepared: FunctionNodeReadyToEvaluate) -> "tuple[str, ...]":
    if isinstance(prepared, FunctionNodeCompiledToItsEvaluateEntry):
        return ()
    return tuple(each_output["value_type"] for each_output in prepared.function_signature["outputs"])


def _compiled_config_fields(prepared: FunctionNodeReadyToEvaluate) -> "tuple[str, ...]":
    if isinstance(prepared, FunctionNodeCompiledToItsEvaluateEntry):
        return prepared.config_fields
    return ()


def _config_as_a_mapping(config: Any) -> "dict[str, Any]":
    if config is None:
        return {}
    if isinstance(config, dict):
        return config
    raise FunctionNodeDoesNotCompile(
        f"the config handed to a promoted function node is a {type(config).__name__}, not a dict"
    )

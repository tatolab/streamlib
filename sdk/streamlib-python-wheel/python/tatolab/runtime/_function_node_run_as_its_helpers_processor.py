# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The processor a helper runs a function node as, once the node is promoted out of its readers' islands.

Each input reads the newest value its link carries, an island its own helper
evaluates or a channel from a class node or another promoted node, and keeps
its pin value until one arrives; an `UnreadMessage` input holds only the
message that arrived since the last call, and the node is not called while it
holds none. A live config update, and so each value a link gives a field the
node exposes, reaches the next call. A number or bool output is written as a
control value, one f64 sample on one channel at rate 0, a `dict` output as a
Records message, and an `UnreadMessage` output exactly as the message arrived.
"""

from __future__ import annotations

import dataclasses
import struct
from typing import Any, Optional

from tatolab.stream import (
    Array,
    Channels,
    Image,
    Input,
    NodeLinkDataAccess,
    Output,
    Packets,
    Records,
    node,
)
from tatolab.stream._node_declaration import NODE_DECLARATION_CONFIG_CLASS_STAMP

from tatolab.runtime._engine import (
    FunctionNodeEvaluatedAsItsHelpersProcessor,
    hand_an_unread_message_on,
    read_the_message_at_an_input_port_unread,
)
from tatolab.runtime._function_node_compilation import (
    FunctionNodeBodyRunAsPython,
    FunctionNodeCompiledToItsEvaluateEntry,
    FunctionNodeDoesNotCompile,
    FunctionNodeReadyToEvaluate,
    config_values_of_one_function_node,
    how_one_function_node_is_called,
    prepare_function_node_to_evaluate,
)

# A control value on a wire: one f64 sample, at no regular rate.
_CONTROL_VALUE_SAMPLE_FORMAT = "<d"
_CONTROL_VALUE_SAMPLE_RATE = 0


def the_processor_class_a_promoted_function_node_runs_as(
    function_node: Any,
    import_path: str,
    node_id: str,
    link_data_access: NodeLinkDataAccess,
) -> type:
    """A `@node.process` class whose `process` calls `function_node`, compiled or as Python, once per run of its loop."""
    prepared = prepare_function_node_to_evaluate(function_node, import_path)
    ports_carrying_records = _ports_carrying_records(prepared)
    config_class: Optional[type] = getattr(function_node, NODE_DECLARATION_CONFIG_CLASS_STAMP, None)
    ports_named_like_a_runner_member = sorted(
        port
        for port in (*prepared.input_ports, *prepared.output_ports)
        if port in vars(_FunctionNodeRunAsItsHelpersProcessor)
        or port in _FunctionNodeRunAsItsHelpersProcessor.__annotations__
    )
    ports_carrying_an_unread_message = _ports_carrying_an_unread_message(prepared)
    if ports_named_like_a_runner_member:
        raise FunctionNodeDoesNotCompile(
            f"function node `{node_id}` ({import_path}) cannot run in a helper of its own: its "
            f"port {', '.join(repr(port) for port in ports_named_like_a_runner_member)} is "
            "named like a member of the processor it would run as"
        )

    class_attributes: "dict[str, Any]" = {}
    for input_port in prepared.input_ports:
        if input_port in ports_carrying_an_unread_message:
            class_attributes[input_port] = Input(
                Image, also_takes=(Array, Channels, Packets, Records), delivery="newest"
            )
        else:
            class_attributes[input_port] = Input(
                Records if input_port in ports_carrying_records else Channels, delivery="newest"
            )
    for output_port in prepared.output_ports:
        if output_port in ports_carrying_an_unread_message:
            class_attributes[output_port] = Output(Image)
        else:
            class_attributes[output_port] = Output(
                Records if output_port in ports_carrying_records else Channels
            )
    class_attributes["__doc__"] = f"Function node `{node_id}` ({import_path}), run in its own helper."
    class_attributes["__module__"] = __name__
    runner_class = type(
        f"FunctionNodeRunAsItsHelpersProcessor_{function_node.__name__}",
        (_FunctionNodeRunAsItsHelpersProcessor,),
        class_attributes,
    )
    runner_class._prepared = prepared
    runner_class._node_id = node_id
    runner_class._link_data_access = link_data_access
    declared = node.process(execution="continuous")(runner_class)
    setattr(declared, NODE_DECLARATION_CONFIG_CLASS_STAMP, config_class)
    declared.__init__ = _FunctionNodeRunAsItsHelpersProcessor.start_from_its_config
    return declared


def ran_compiled_by_node_id_of_the_promoted_function_node_a_processor_class_runs(
    processor_class: Any,
) -> "dict[str, bool]":
    """`{node_id: ran_compiled}` for the function node `processor_class` runs once promoted; empty for any other class."""
    if not (
        isinstance(processor_class, type)
        and issubclass(processor_class, _FunctionNodeRunAsItsHelpersProcessor)
    ):
        return {}
    return {
        processor_class._node_id: isinstance(
            processor_class._prepared, FunctionNodeCompiledToItsEvaluateEntry
        )
    }


class _FunctionNodeRunAsItsHelpersProcessor:
    """Calls one function node with the newest value on each input and writes its outputs."""

    _prepared: FunctionNodeReadyToEvaluate
    _node_id: str
    _link_data_access: NodeLinkDataAccess
    _input_values: "list[Any]"
    _compiled_config_values_handed_to_each_call: "list[float]"
    _body_run_as_python: Optional[FunctionNodeBodyRunAsPython]
    _control_values_written: int
    _evaluation: FunctionNodeEvaluatedAsItsHelpersProcessor

    def start_from_its_config(self, config: Any = None) -> None:
        """Take the node's config and start its state from its initial record; set as `__init__` once declared."""
        prepared = self._prepared
        self._input_values = [
            prepared.pin_defaults_by_input_port.get(input_port) for input_port in prepared.input_ports
        ]
        self._control_values_written = 0
        compiled_config_fields = _compiled_config_fields(prepared)
        called_through, config_values, compiled_config_value_indices = how_one_function_node_is_called(
            prepared, self._node_id, _config_as_a_mapping(prepared, config), compiled_config_fields
        )
        self._compiled_config_values_handed_to_each_call = list(config_values)
        self._body_run_as_python = (
            called_through if isinstance(called_through, FunctionNodeBodyRunAsPython) else None
        )
        self._evaluation = FunctionNodeEvaluatedAsItsHelpersProcessor(
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
        )

    def configure(self, config: Any) -> None:
        """Call the node with `config` from the next run on, keeping its state."""
        prepared = self._prepared
        config_as_a_mapping = _config_as_a_mapping(prepared, config)
        if isinstance(prepared, FunctionNodeCompiledToItsEvaluateEntry):
            self._compiled_config_values_handed_to_each_call = config_values_of_one_function_node(
                prepared, self._node_id, config_as_a_mapping
            )
        elif self._body_run_as_python is not None:
            self._body_run_as_python.take_the_config(config_as_a_mapping)

    def process(self, ctx: Any) -> None:
        prepared = self._prepared
        ports_carrying_records = _ports_carrying_records(prepared)
        ports_carrying_an_unread_message = _ports_carrying_an_unread_message(prepared)
        for input_index, input_port in enumerate(prepared.input_ports):
            if input_port in ports_carrying_an_unread_message:
                self._input_values[input_index] = read_the_message_at_an_input_port_unread(
                    link_data_access=self._link_data_access, input_port=input_port
                )
                continue
            if input_port in ports_carrying_records:
                record = ctx.inputs.read(getattr(type(self), input_port))
                if record is not None:
                    self._input_values[input_index] = record
                continue
            payload = ctx.inputs.read(input_port)
            if payload is not None:
                (self._input_values[input_index],) = struct.unpack_from(
                    _CONTROL_VALUE_SAMPLE_FORMAT, bytes(payload)
                )
        input_values = [value for value in self._input_values if value is not None]
        if len(input_values) != len(self._input_values):
            return
        output_values = self._evaluation.evaluate(
            [*input_values, *self._compiled_config_values_handed_to_each_call]
        )
        for output_port, output_value in zip(prepared.output_ports, output_values):
            if output_value is None:
                continue
            if output_port in ports_carrying_an_unread_message:
                hand_an_unread_message_on(
                    link_data_access=self._link_data_access,
                    output_port=output_port,
                    message=output_value,
                )
                continue
            if output_port in ports_carrying_records:
                ctx.outputs.write(getattr(type(self), output_port), output_value)
                continue
            ctx.outputs.write_channels_block(
                output_port,
                memoryview(struct.pack("=d", output_value)).cast("d", (1, 1)),
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


def _ports_carrying_records(prepared: FunctionNodeReadyToEvaluate) -> "frozenset[str]":
    if isinstance(prepared, FunctionNodeCompiledToItsEvaluateEntry):
        return frozenset()
    return prepared.ports_carrying_records


def _compiled_config_fields(prepared: FunctionNodeReadyToEvaluate) -> "tuple[str, ...]":
    if isinstance(prepared, FunctionNodeCompiledToItsEvaluateEntry):
        return prepared.config_fields
    return ()


def _config_fields(prepared: FunctionNodeReadyToEvaluate) -> "tuple[str, ...]":
    if isinstance(prepared, FunctionNodeCompiledToItsEvaluateEntry):
        return prepared.config_fields
    if prepared.config_class is None:
        return ()
    return tuple(field.name for field in dataclasses.fields(prepared.config_class))


def _config_as_a_mapping(prepared: FunctionNodeReadyToEvaluate, config: Any) -> "dict[str, Any]":
    if config is None:
        return {}
    if isinstance(config, dict):
        return config
    config_fields = _config_fields(prepared)
    missing_fields = [field for field in config_fields if not hasattr(config, field)]
    if missing_fields:
        raise FunctionNodeDoesNotCompile(
            f"the config handed to a promoted function node has no {', '.join(missing_fields)}"
        )
    return {field: getattr(config, field) for field in config_fields}

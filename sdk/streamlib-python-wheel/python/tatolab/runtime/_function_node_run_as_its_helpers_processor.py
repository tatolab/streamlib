# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The processor a helper runs a function node as, once the node is promoted out of its readers' islands.

Each input reads the newest value its link carries, an island its own helper
evaluates or a channel from another promoted node, and keeps its pin value
until one arrives. A number or bool output is written as a control value, one
f64 sample on one channel at rate 0, and a `dict` output as a Records message.
"""

from __future__ import annotations

import dataclasses
import struct
from typing import Any, Optional

from tatolab.stream import Channels, Input, NodeLinkDataAccess, Output, Records, node
from tatolab.stream._node_declaration import NODE_DECLARATION_CONFIG_CLASS_STAMP

from tatolab.runtime._engine import FunctionNodeEvaluatedAsItsHelpersProcessor
from tatolab.runtime._function_node_compilation import (
    FunctionNodeCompiledToItsEvaluateEntry,
    FunctionNodeDoesNotCompile,
    FunctionNodeReadyToEvaluate,
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
    if ports_named_like_a_runner_member:
        raise FunctionNodeDoesNotCompile(
            f"function node `{node_id}` ({import_path}) cannot run in a helper of its own: its "
            f"port {', '.join(repr(port) for port in ports_named_like_a_runner_member)} is "
            "named like a member of the processor it would run as"
        )

    class_attributes: "dict[str, Any]" = {
        input_port: Input(Records if input_port in ports_carrying_records else Channels, delivery="newest")
        for input_port in prepared.input_ports
    }
    class_attributes.update(
        {
            output_port: Output(Records if output_port in ports_carrying_records else Channels)
            for output_port in prepared.output_ports
        }
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


class _FunctionNodeRunAsItsHelpersProcessor:
    """Calls one function node with the newest value on each input and writes its outputs."""

    _prepared: FunctionNodeReadyToEvaluate
    _node_id: str
    _link_data_access: NodeLinkDataAccess
    _input_values: "list[Any]"
    _control_values_written: int
    _evaluation: FunctionNodeEvaluatedAsItsHelpersProcessor

    def start_from_its_config(self, config: Any = None) -> None:
        """Take the node's config and start its state from its initial record; set as `__init__` once declared."""
        prepared = self._prepared
        self._input_values = [
            prepared.pin_defaults_by_input_port.get(input_port) for input_port in prepared.input_ports
        ]
        self._control_values_written = 0
        called_through, config_values = how_one_function_node_is_called(
            prepared, self._node_id, _config_as_a_mapping(prepared, config)
        )
        self._evaluation = FunctionNodeEvaluatedAsItsHelpersProcessor(
            link_data_access=self._link_data_access,
            node_id=self._node_id,
            called_through=called_through,
            input_count=len(prepared.input_ports),
            config_values=config_values,
            initial_state_values=list(prepared.initial_state_values),
            output_count=len(prepared.output_ports),
        )

    def process(self, ctx: Any) -> None:
        prepared = self._prepared
        ports_carrying_records = _ports_carrying_records(prepared)
        for input_index, input_port in enumerate(prepared.input_ports):
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
        output_values = self._evaluation.evaluate(input_values)
        for output_port, output_value in zip(prepared.output_ports, output_values):
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


def _ports_carrying_records(prepared: FunctionNodeReadyToEvaluate) -> "frozenset[str]":
    if isinstance(prepared, FunctionNodeCompiledToItsEvaluateEntry):
        return frozenset()
    return prepared.ports_carrying_records


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

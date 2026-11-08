# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The processor a helper runs a function node as, once the node is promoted out of its readers' islands.

Each input reads the newest control value its link carries, an island its own
helper evaluates or a channel from another promoted node, and keeps its pin
value until one arrives. Each output is written as a control value: one f64
sample on one channel at rate 0.
"""

from __future__ import annotations

import struct
from typing import Any, Optional

from tatolab.stream import Channels, Input, NodeLinkDataAccess, Output, node
from tatolab.stream._node_declaration import NODE_DECLARATION_CONFIG_CLASS_STAMP

from tatolab.runtime._engine import FunctionNodeEvaluatedAsItsHelpersProcessor
from tatolab.runtime._function_node_compilation import (
    FunctionNodeCompiledToItsEvaluateEntry,
    FunctionNodeDoesNotCompile,
    compile_function_node_to_its_evaluate_entry,
    config_values_of_one_function_node,
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
    """A `@node.process` class whose `process` calls `function_node` compiled, once per run of its loop."""
    compiled = compile_function_node_to_its_evaluate_entry(function_node, import_path)
    config_class: Optional[type] = getattr(function_node, NODE_DECLARATION_CONFIG_CLASS_STAMP, None)
    ports_named_like_a_runner_member = sorted(
        port
        for port in (*compiled.input_ports, *compiled.output_ports)
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
        input_port: Input(Channels, delivery="newest") for input_port in compiled.input_ports
    }
    class_attributes.update(
        {output_port: Output(Channels) for output_port in compiled.output_ports}
    )
    class_attributes["__doc__"] = f"Function node `{node_id}` ({import_path}), run in its own helper."
    class_attributes["__module__"] = __name__
    runner_class = type(
        f"FunctionNodeRunAsItsHelpersProcessor_{function_node.__name__}",
        (_FunctionNodeRunAsItsHelpersProcessor,),
        class_attributes,
    )
    runner_class._compiled = compiled
    runner_class._node_id = node_id
    runner_class._link_data_access = link_data_access
    declared = node.process(execution="continuous")(runner_class)
    setattr(declared, NODE_DECLARATION_CONFIG_CLASS_STAMP, config_class)
    declared.__init__ = _FunctionNodeRunAsItsHelpersProcessor.start_from_its_config
    return declared


class _FunctionNodeRunAsItsHelpersProcessor:
    """Calls one compiled function node with the newest value on each input and writes its outputs."""

    _compiled: FunctionNodeCompiledToItsEvaluateEntry
    _node_id: str
    _link_data_access: NodeLinkDataAccess
    _input_values: "list[Optional[float]]"
    _control_values_written: int
    _evaluation: FunctionNodeEvaluatedAsItsHelpersProcessor

    def start_from_its_config(self, config: Any = None) -> None:
        """Take the node's config and start its state from its initial record; set as `__init__` once declared."""
        compiled = self._compiled
        self._input_values = [
            compiled.pin_defaults_by_input_port.get(input_port) for input_port in compiled.input_ports
        ]
        self._control_values_written = 0
        self._evaluation = FunctionNodeEvaluatedAsItsHelpersProcessor(
            link_data_access=self._link_data_access,
            node_id=self._node_id,
            evaluate_entry_address=compiled.evaluate_entry_address,
            input_count=len(compiled.input_ports),
            config_values=config_values_of_one_function_node(
                compiled, self._node_id, _config_as_a_mapping(compiled, config)
            ),
            initial_state_values=list(compiled.initial_state_values),
            output_count=len(compiled.output_ports),
        )

    def process(self, ctx: Any) -> None:
        compiled = self._compiled
        for input_index, input_port in enumerate(compiled.input_ports):
            payload = ctx.inputs.read(input_port)
            if payload is not None:
                (self._input_values[input_index],) = struct.unpack_from(
                    _CONTROL_VALUE_SAMPLE_FORMAT, bytes(payload)
                )
        input_values = [value for value in self._input_values if value is not None]
        if len(input_values) != len(self._input_values):
            return
        output_values = self._evaluation.evaluate(input_values)
        for output_port, output_value in zip(compiled.output_ports, output_values):
            ctx.outputs.write_channels_block(
                output_port,
                memoryview(struct.pack("=d", output_value)).cast("d", (1, 1)),
                start_sample=self._control_values_written,
                sample_rate=_CONTROL_VALUE_SAMPLE_RATE,
            )
        self._control_values_written += 1


def _config_as_a_mapping(compiled: FunctionNodeCompiledToItsEvaluateEntry, config: Any) -> "dict[str, Any]":
    if config is None:
        return {}
    if isinstance(config, dict):
        return config
    missing_fields = [field for field in compiled.config_fields if not hasattr(config, field)]
    if missing_fields:
        raise FunctionNodeDoesNotCompile(
            f"the config handed to a promoted function node has no {', '.join(missing_fields)}"
        )
    return {field: getattr(config, field) for field in compiled.config_fields}

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The processor the stream's function node pool runs.

Each island the pool carries feeds one of its inputs and is evaluated at that
input's read, as in any reader; the pool writes the value to the matching
output, whose channel reaches the reader that could not evaluate the island
itself. A number or bool is written as a control value, one f64 sample on one
channel at rate 0, and a `dict` as a Records message.
"""

from __future__ import annotations

import struct
from typing import Any, Mapping, Sequence

from tatolab.stream import Channels, Input, Output, Records, node

# A control value on a wire: one f64 sample, at no regular rate.
_CONTROL_VALUE_SAMPLE_FORMAT = "<d"
_CONTROL_VALUE_SAMPLE_RATE = 0


class FunctionNodePool:
    """What the pool's helper loads; its processor is built from the pool's config at setup."""


def the_processor_class_the_function_node_pool_runs_as(pool_config: "Mapping[str, Any]") -> type:
    """A `@node.process` class with one input and one output per island `pool_config["carries"]` names.

    Wire contract with the engine: `{"carries": [{"input_port", "output_port",
    "family": "channels" | "records"}]}`.
    """
    carries: "Sequence[Mapping[str, Any]]" = pool_config.get("carries") or []
    class_attributes: "dict[str, Any]" = {}
    for carry in carries:
        family = Records if carry["family"] == "records" else Channels
        class_attributes[carry["input_port"]] = Input(family, delivery="newest")
        class_attributes[carry["output_port"]] = Output(family)
    class_attributes["__doc__"] = "The stream's function node pool."
    class_attributes["__module__"] = __name__
    pool_class = type("FunctionNodePoolProcessor", (_FunctionNodePoolProcessor,), class_attributes)
    pool_class._carries = tuple(
        (carry["input_port"], carry["output_port"], carry["family"] == "records") for carry in carries
    )
    return node.process(execution="continuous")(pool_class)


class _FunctionNodePoolProcessor:
    """Reads each carried island and writes its value to the island's output."""

    _carries: "tuple[tuple[str, str, bool], ...]"

    def __init__(self) -> None:
        self._control_values_written = {output_port: 0 for _, output_port, _ in self._carries}

    def process(self, ctx: Any) -> None:
        for input_port, output_port, carries_records in self._carries:
            if carries_records:
                record = ctx.inputs.read(getattr(type(self), input_port))
                if record is not None:
                    ctx.outputs.write(getattr(type(self), output_port), record)
                continue
            payload = ctx.inputs.read(input_port)
            if payload is None:
                continue
            (control_value,) = struct.unpack_from(_CONTROL_VALUE_SAMPLE_FORMAT, bytes(payload))
            ctx.outputs.write_channels_block(
                output_port,
                memoryview(struct.pack("=d", control_value)).cast("d", (1, 1)),
                start_sample=self._control_values_written[output_port],
                sample_rate=_CONTROL_VALUE_SAMPLE_RATE,
            )
            self._control_values_written[output_port] += 1

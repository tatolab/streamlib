# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The streams and stand-ins both stream graph builder test files use.

A stream's identity is the import path of its function,
`stream_graph_builder_streams:<name>`.
"""

from types import MappingProxyType
from typing import Any

from stream_graph_builder_nodes import (
    FrameFilters,
    FrameInverter,
)
from tatolab.stream import (
    StreamBuilder,
    graph_generator,
)


TENSOR_TO_AUDIO_TYPE = "tatolab.stream:TensorToAudio"
RIG_CHANNEL_NAMES: "dict[str, Any]" = {"channel_names": ["level"]}


FRAME_INVERTER_TYPE = "stream_graph_builder_nodes:FrameInverter"
BRIGHTNESS_READER_TYPE = "stream_graph_builder_nodes:BrightnessReader"


class StandInMarker:
    """A class carrying a `str` `type`, the way every native marker does."""

    type = "stand_in_builtins::stand_in_marker::StandInMarker"


class UndecoratedFilter:
    """A class that never met `@node`."""


NESTED_INPUT_VALUES: "dict[str, Any]" = {
    "title": "Rig",
    "size": (640, 480),
    "overlay": MappingProxyType({"labels": ["left", "right"], "opacity": 0.5}),
    "enabled": True,
    "device": None,
}


@graph_generator
def every_kind_of_node(stream_builder: StreamBuilder) -> None:
    """A class node, a nested one and a marker-shaped class.

    The second line of the description.
    """
    stream_builder.add(FrameInverter)
    stream_builder.add(FrameFilters.FrameDarkener)
    stream_builder.add(StandInMarker)


@graph_generator
def a_stream_setting_input_values(stream_builder: StreamBuilder) -> None:
    stream_builder.add(StandInMarker, input_values=NESTED_INPUT_VALUES)


def names_of_nodes_added(stream_builder: StreamBuilder, *node_classes: type) -> "list[str]":
    return [stream_builder.add(node_class).name for node_class in node_classes]

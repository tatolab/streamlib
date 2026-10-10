# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_stream_graph_load_on_tatolabd.py` runs, each
compiled in the suite project's own interpreter by `tatolab run <module>:<function>`."""

from typing import Any

from runtime_load_open_config_nodes import OpenConfigSink
from runtime_load_served_graph_nodes import LoadedFrameSink
from tatolab.stream import DisplayWindow, StreamBuilder, TestPatternSource, stream

#: The deepest config the builder compiles: 126 containers from the graph's
#: root, less the graph, its `nodes` list and the node enclosing the config.
CONTAINERS_A_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF = 123


def config_nesting_containers_deep(containers_counting_the_config: int) -> "dict[str, Any]":
    """`{"nested": [[...]]}`, `containers_counting_the_config` containers deep in all."""
    nested: "list[Any]" = []
    for _ in range(containers_counting_the_config - 2):
        nested = [nested]
    return {"nested": nested}


@stream
def open_config_sink_with_the_deepest_config_the_builder_compiles(stream_builder: StreamBuilder) -> None:
    stream_builder.add(
        OpenConfigSink,
        config=config_nesting_containers_deep(CONTAINERS_A_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF),
    )


@stream
def pattern_linked_from_a_port_it_lacks_into_a_window(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(TestPatternSource)
    window = stream_builder.add(DisplayWindow)
    stream_builder.connect(pattern.output("no_such_port"), window.input("video"))


@stream
def named_pattern_into_a_named_sink(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(
        TestPatternSource, name="Loaded Pattern", config={"width": 320, "height": 180}
    )
    sink = stream_builder.add(LoadedFrameSink, name="Loaded Sink")
    stream_builder.connect(pattern.output("video"), sink.input("bags_from_upstream"))

# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The built-in node classes `tatolab.stream` publishes, generated from the
runtime's own descriptors: each named by the type the runtime registers, each
compiled into a graph by pure Python. That the generated types are exactly the
ones the runtime registers is tested against the runtime."""

import inspect

import pytest

import tatolab.stream
from tatolab.stream import _built_in_nodes
from tatolab.stream._built_in_node import BuiltInNode
from tatolab.stream._stream_graph_builder import graph_generator, run_graph_generator

BUILT_IN_NODE_TYPE_PREFIX = "tatolab.stream:"

GENERATED_BUILT_IN_NODE_CLASSES = [
    exported
    for exported in vars(_built_in_nodes).values()
    if inspect.isclass(exported)
    and issubclass(exported, BuiltInNode)
    and exported is not BuiltInNode
]


def test_the_generated_module_holds_every_built_in():
    assert len(GENERATED_BUILT_IN_NODE_CLASSES) == 6


@pytest.mark.parametrize(
    "built_in_node_class", GENERATED_BUILT_IN_NODE_CLASSES, ids=lambda cls: cls.__name__
)
def test_a_built_in_is_named_by_its_own_class_in_the_stream_package(built_in_node_class):
    class_name = built_in_node_class.__name__
    assert built_in_node_class.type == f"{BUILT_IN_NODE_TYPE_PREFIX}{class_name}"
    assert getattr(tatolab.stream, class_name) is built_in_node_class
    assert class_name in tatolab.stream.__all__
    assert f"{class_name}Config" in tatolab.stream.__all__


@pytest.mark.parametrize(
    "built_in_node_class", GENERATED_BUILT_IN_NODE_CLASSES, ids=lambda cls: cls.__name__
)
def test_the_built_in_class_cannot_be_instantiated(built_in_node_class):
    with pytest.raises(TypeError):
        built_in_node_class()


def test_a_built_in_is_never_constructed_and_says_how_to_add_it():
    with pytest.raises(TypeError, match=r"stream_builder\.add\(ChannelsToRecords, config="):
        tatolab.stream.ChannelsToRecords()  # pyright: ignore[reportCallIssue]


@graph_generator
def a_channels_to_records_conversion(stream_builder: tatolab.stream.StreamBuilder) -> None:
    stream_builder.add(
        _built_in_nodes.ChannelsToRecords,
        config={"channel_names": ["level"]},
    )


def test_a_stream_of_built_ins_compiles_with_no_runtime_installed():
    graph = run_graph_generator(a_channels_to_records_conversion)

    assert [(node["name"], node["type"]) for node in graph["nodes"]] == [
        ("channelstorecords", "tatolab.stream:ChannelsToRecords"),
    ]
    assert graph["nodes"][0]["config"] == {"channel_names": ["level"]}


@graph_generator
def a_control_value_round_trip(stream_builder: tatolab.stream.StreamBuilder) -> None:
    to_channels = stream_builder.add(
        tatolab.stream.RecordsToChannels,
        config={"channel_names": ["gain"], "sample_rate": 30},
    )
    to_array = stream_builder.add(tatolab.stream.ChannelsToArray)
    back_to_channels = stream_builder.add(
        tatolab.stream.ArrayToChannels,
        config={"channel_names": ["gain"], "sample_rate": 30},
    )
    to_records = stream_builder.add(
        tatolab.stream.ChannelsToRecords, config={"channel_names": ["gain"]}
    )
    stream_builder.connect(to_channels.output("channels"), to_array.input("channels"))
    stream_builder.connect(to_array.output("array"), back_to_channels.input("array"))
    stream_builder.connect(back_to_channels.output("channels"), to_records.input("channels"))


def test_the_conversion_built_ins_compile_with_no_runtime_installed():
    graph = run_graph_generator(a_control_value_round_trip)

    assert [node["type"] for node in graph["nodes"]] == [
        "tatolab.stream:RecordsToChannels",
        "tatolab.stream:ChannelsToArray",
        "tatolab.stream:ArrayToChannels",
        "tatolab.stream:ChannelsToRecords",
    ]

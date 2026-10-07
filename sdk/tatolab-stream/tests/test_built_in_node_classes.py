# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The built-in node classes `tatolab.stream` publishes, generated from the
runtime's own descriptors: each named by the type the runtime registers, each
compiled into a graph by pure Python. That the generated types are exactly the
ones the runtime registers is tested against the runtime."""

import inspect

import pytest

import tatolab.stream
from tatolab.stream import _built_in_nodes
from tatolab.stream._built_in_node import BuiltInNode
from tatolab.stream._stream_graph_builder import compile_stream_to_graph, stream

BUILT_IN_NODE_TYPE_PREFIX = "tatolab.stream:"

GENERATED_BUILT_IN_NODE_CLASSES = [
    exported
    for exported in vars(_built_in_nodes).values()
    if inspect.isclass(exported)
    and issubclass(exported, BuiltInNode)
    and exported is not BuiltInNode
]


def test_the_generated_module_holds_every_built_in():
    assert len(GENERATED_BUILT_IN_NODE_CLASSES) == 13


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
    with pytest.raises(TypeError, match=r"stream_builder\.add\(Mp4Sink, config="):
        tatolab.stream.Mp4Sink()  # pyright: ignore[reportCallIssue]


@stream
def camera_to_display(stream_builder: tatolab.stream.StreamBuilder) -> None:
    camera = stream_builder.add(
        _built_in_nodes.CameraSource, config={"device_id": "/dev/video2"}
    )
    display = stream_builder.add(_built_in_nodes.DisplayWindow)
    stream_builder.connect(camera.output("video"), display.input("video"))


def test_a_stream_of_built_ins_compiles_with_no_runtime_installed():
    graph = compile_stream_to_graph(camera_to_display)

    assert [(node["name"], node["type"]) for node in graph["nodes"]] == [
        ("camerasource", "tatolab.stream:CameraSource"),
        ("displaywindow", "tatolab.stream:DisplayWindow"),
    ]
    assert graph["nodes"][0]["config"] == {"device_id": "/dev/video2"}

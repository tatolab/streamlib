# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The built-in node classes `tatolab.stream` publishes, generated from the
runtime's own descriptors: each named by the type the runtime registers, each
compiled into a graph by pure Python."""

import inspect
import json
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

import tatolab.stream
from tatolab.runtime._engine import processor_class_import_paths_in_this_processes_catalog
from tatolab.stream import _built_in_nodes
from tatolab.stream._built_in_node import BuiltInNode

BUILT_IN_NODE_TYPE_PREFIX = "tatolab.stream:"
STREAM_PACKAGE_DIRECTORY = Path(tatolab.stream.__file__).parent

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


def test_the_generated_types_are_exactly_the_built_ins_the_runtime_registers():
    registered_built_in_node_types = {
        registered
        for registered in processor_class_import_paths_in_this_processes_catalog()
        if registered.startswith(BUILT_IN_NODE_TYPE_PREFIX)
    }
    generated_built_in_node_types = {cls.type for cls in GENERATED_BUILT_IN_NODE_CLASSES}
    if sys.platform == "linux":
        assert generated_built_in_node_types == registered_built_in_node_types
    else:
        # A floor compiles some built-ins out; the module still names every one,
        # and the runtime refuses an absent one at load naming its floors.
        assert registered_built_in_node_types <= generated_built_in_node_types


def test_a_built_in_is_never_constructed_and_says_how_to_add_it():
    with pytest.raises(TypeError, match=r"stream_builder\.add\(Mp4Sink, config="):
        tatolab.stream.Mp4Sink()  # pyright: ignore[reportCallIssue]


def test_a_stream_of_built_ins_compiles_without_the_native_module():
    compile_without_the_native_module = textwrap.dedent(
        f"""
        import importlib
        import json
        import sys
        import types

        # The stream package's own modules, loaded as one package without its
        # `__init__`, which still imports the runtime's names until the package
        # stands alone.
        modules_under_test = types.ModuleType("stream_package_modules_under_test")
        modules_under_test.__path__ = [{str(STREAM_PACKAGE_DIRECTORY)!r}]
        sys.modules["stream_package_modules_under_test"] = modules_under_test
        built_in_nodes = importlib.import_module(
            "stream_package_modules_under_test._built_in_nodes"
        )
        builder = importlib.import_module(
            "stream_package_modules_under_test._stream_graph_builder"
        )

        @builder.stream
        def camera_to_display(stream_builder):
            camera = stream_builder.add(
                built_in_nodes.CameraSource, config={{"device_id": "/dev/video2"}}
            )
            display = stream_builder.add(built_in_nodes.DisplayWindow)
            stream_builder.connect(camera.output("video"), display.input("video"))

        graph = builder.compile_stream_to_graph(camera_to_display)
        loaded = sorted(name for name in sys.modules if name.split(".")[0] == "tatolab")
        print(json.dumps({{"graph": graph, "tatolab_modules_loaded": loaded}}))
        """
    )
    completed = subprocess.run(
        [sys.executable, "-c", compile_without_the_native_module],
        capture_output=True,
        text=True,
        check=False,
    )
    assert completed.returncode == 0, completed.stderr
    reported = json.loads(completed.stdout)

    assert reported["tatolab_modules_loaded"] == []
    assert [(node["name"], node["type"]) for node in reported["graph"]["nodes"]] == [
        ("camerasource", "tatolab.stream:CameraSource"),
        ("displaywindow", "tatolab.stream:DisplayWindow"),
    ]
    assert reported["graph"]["nodes"][0]["config"] == {"device_id": "/dev/video2"}

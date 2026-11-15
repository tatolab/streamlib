# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`@graph_generator` and `run_graph_generator`, with no engine: what the
decorator accepts and refuses, and the graph a stream compiles to.

A stream compiles to its graph as plain data, so every case is a literal
expectation about that data or a refusal at the line the author wrote.
"""

import ast
import functools
import json
import sys
from pathlib import Path
from typing import Any

import pytest

import tatolab.stream
from stream_graph_builder_nodes import (
    BrightnessReader,
    FrameInverter,
    RigMonitor,
)
from tatolab.stream import (
    StreamBuilder,
    run_graph_generator,
    graph_generator,
)
from tatolab.stream._stream_graph_builder import is_graph_generator

from stream_graph_builder_streams import (
    BRIGHTNESS_READER_TYPE,
    CHANNELS_TO_RECORDS_TYPE,
    FRAME_INVERTER_TYPE,
    RIG_CHANNELS_TO_RECORDS,
    UndecoratedFilter,
    a_configured_stream,
    every_kind_of_node,
    names_of_nodes_added,
)


STREAM_GRAPH_BUILDER_SOURCES = tuple(
    Path(tatolab.stream.__file__).resolve().parent / module_file_name
    for module_file_name in (
        "_stream_graph_builder.py",
        "_stream_builder.py",
        "_graph_generator_detection.py",
    )
)


RIG_MONITOR_TYPE = "stream_graph_builder_nodes:RigMonitor"


class StreamBuildingFailure(Exception):
    """The one exception `a_stream_that_raises` raises."""


THE_STREAM_BUILDING_FAILURE = StreamBuildingFailure("the camera rig is unplugged")


@graph_generator
def camera_rig(stream_builder: StreamBuilder) -> None:
    """A marker source fanned out to an inverter feeding two readers and to a second inverter."""
    source = stream_builder.add(tatolab.stream.ChannelsToRecords, config=RIG_CHANNELS_TO_RECORDS)
    inverter = stream_builder.add(FrameInverter)
    first_reader = stream_builder.add(BrightnessReader)
    backup_reader = stream_builder.add(BrightnessReader, name="Backup Meter")
    second_inverter = stream_builder.add(FrameInverter)
    monitor = stream_builder.add(RigMonitor, config={"name": "Rig Monitor"})
    stream_builder.connect(source.output("track"), inverter.input("video_from_upstream"))
    stream_builder.connect(
        inverter.output("video_to_downstream"),
        first_reader.input("VIDEO_FROM_UPSTREAM"),
    )
    stream_builder.connect(
        inverter.output("video_to_downstream"),
        backup_reader.input("video_from_upstream"),
    )
    stream_builder.connect(
        source.output("Track"),
        second_inverter.input("video_from_upstream"),
    )
    stream_builder.connect(second_inverter.output("video_to_downstream"), monitor.input("Video"))


@graph_generator
def adds_nothing(stream_builder: StreamBuilder) -> None:
    """A stream whose function adds no node."""


@graph_generator
def a_stream_that_raises(stream_builder: StreamBuilder) -> None:
    stream_builder.add(FrameInverter)
    raise THE_STREAM_BUILDING_FAILURE


@graph_generator
def a_stream_with_a_typed_duplicate(stream_builder: StreamBuilder) -> None:
    stream_builder.add(FrameInverter, name="Front Camera")
    stream_builder.add(BrightnessReader, name="front-CAMERA")


def not_decorated(stream_builder: StreamBuilder) -> None:
    stream_builder.add(FrameInverter)


def test_a_stream_compiles_to_its_graph() -> None:
    graph = run_graph_generator(camera_rig)

    assert graph == {
        "stream": "camera_rig",
        "nodes": [
            {
                "name": "channelstorecords",
                "type": CHANNELS_TO_RECORDS_TYPE,
                "config": RIG_CHANNELS_TO_RECORDS,
            },
            {"name": "frameinverter", "type": FRAME_INVERTER_TYPE, "config": {}},
            {"name": "brightnessreader", "type": BRIGHTNESS_READER_TYPE, "config": {}},
            {"name": "backup-meter", "type": BRIGHTNESS_READER_TYPE, "config": {}},
            {"name": "frameinverter-2", "type": FRAME_INVERTER_TYPE, "config": {}},
            {
                "name": "rigmonitor",
                "type": RIG_MONITOR_TYPE,
                "config": {"name": "Rig Monitor"},
            },
        ],
        "links": [
            {
                "source": {"node": "channelstorecords", "port": "track"},
                "target": {"node": "frameinverter", "port": "video_from_upstream"},
            },
            {
                "source": {"node": "frameinverter", "port": "video_to_downstream"},
                "target": {"node": "brightnessreader", "port": "video_from_upstream"},
            },
            {
                "source": {"node": "frameinverter", "port": "video_to_downstream"},
                "target": {"node": "backup-meter", "port": "video_from_upstream"},
            },
            {
                "source": {"node": "channelstorecords", "port": "track"},
                "target": {"node": "frameinverter-2", "port": "video_from_upstream"},
            },
            {
                "source": {"node": "frameinverter-2", "port": "video_to_downstream"},
                "target": {"node": "rigmonitor", "port": "video"},
            },
        ],
    }
    assert json.loads(json.dumps(graph)) == graph


def test_a_compiled_graph_has_exactly_the_three_keys() -> None:
    graph = run_graph_generator(every_kind_of_node)

    assert list(graph) == ["stream", "nodes", "links"]
    assert graph["stream"] == "every_kind_of_node"
    assert graph["links"] == []


def test_a_stream_that_adds_nothing_compiles_to_an_empty_graph() -> None:
    assert run_graph_generator(adds_nothing) == {
        "stream": "adds_nothing",
        "nodes": [],
        "links": [],
    }


def test_each_compile_returns_a_fresh_graph() -> None:
    first = run_graph_generator(a_configured_stream)
    first["nodes"][0]["config"]["overlay"]["labels"].append("mutated")

    assert run_graph_generator(a_configured_stream)["nodes"][0]["config"][
        "overlay"
    ]["labels"] == ["left", "right"]


def test_a_stream_function_that_raises_propagates_its_exception_unchanged() -> None:
    with pytest.raises(StreamBuildingFailure) as raised:
        run_graph_generator(a_stream_that_raises)

    assert raised.value is THE_STREAM_BUILDING_FAILURE


def test_a_builder_refusal_inside_a_stream_function_propagates() -> None:
    with pytest.raises(ValueError, match="'front-CAMERA'"):
        run_graph_generator(a_stream_with_a_typed_duplicate)


def test_a_function_that_is_not_a_stream_is_refused_naming_the_decorator() -> None:
    with pytest.raises(TypeError, match="decorate it with @graph_generator"):
        run_graph_generator(not_decorated)


def test_stream_stamps_identity_name_and_description() -> None:
    assert getattr(every_kind_of_node, "__streamlib_graph_generator_identity__") == (
        "stream_graph_builder_streams:every_kind_of_node"
    )
    assert getattr(every_kind_of_node, "__streamlib_graph_generator_name__") == (
        "every_kind_of_node"
    )
    assert getattr(every_kind_of_node, "__streamlib_graph_generator_description__") == (
        "A `@node.process` class, a nested one and a marker-shaped class.\n\n"
        "The second line of the description."
    )


def test_stream_returns_the_function_it_declares_unchanged() -> None:
    namespace: "dict[str, Any]" = {"__name__": "rig_streams"}
    exec("def main(stream_builder):\n    pass\n", namespace)
    undeclared_function = namespace["main"]

    assert graph_generator(undeclared_function) is undeclared_function


def test_a_stream_with_no_docstring_has_an_empty_description() -> None:
    assert getattr(a_stream_that_raises, "__streamlib_graph_generator_description__") == ""


def test_a_stream_in_the_entry_file_is_named_from_main() -> None:
    entry_file_namespace: "dict[str, Any]" = {"__name__": "__main__"}
    exec("def main(stream_builder):\n    pass\n", entry_file_namespace)

    entry_file_stream = graph_generator(entry_file_namespace["main"])

    assert (
        getattr(entry_file_stream, "__streamlib_graph_generator_identity__") == "__main__:main"
    )
    assert getattr(entry_file_stream, "__streamlib_graph_generator_name__") == "main"


@pytest.mark.parametrize(
    "misuse",
    [
        lambda: graph_generator(),  # pyright: ignore[reportCallIssue]
        lambda: graph_generator(name="rig"),  # pyright: ignore[reportCallIssue]
        lambda: graph_generator(not_decorated, name="rig"),  # pyright: ignore[reportCallIssue]
    ],
    ids=["no-arguments", "a-name", "a-function-and-a-name"],
)
def test_stream_called_with_arguments_is_refused_naming_the_bare_form(
    misuse: Any,
) -> None:
    with pytest.raises(TypeError) as refusal:
        misuse()

    assert str(refusal.value).startswith(
        "@graph_generator takes no arguments: the name is the function's."
    )
    assert "`def main(stream_builder: StreamBuilder) -> None:`" in str(refusal.value)


def test_a_nested_function_is_refused_as_not_module_level() -> None:
    def main(stream_builder: StreamBuilder) -> None:
        stream_builder.add(FrameInverter)

    with pytest.raises(TypeError) as refusal:
        graph_generator(main)

    message = str(refusal.value)
    assert "a graph generator is a module-level function" in message
    assert "<locals>.main" in message


def test_a_method_is_refused_as_not_module_level() -> None:
    namespace: "dict[str, Any]" = {"__name__": "rig_streams"}
    exec("class Streams:\n    def main(stream_builder):\n        pass\n", namespace)

    with pytest.raises(TypeError) as refusal:
        graph_generator(namespace["Streams"].main)

    message = str(refusal.value)
    assert "a graph generator is a module-level function" in message
    assert "`rig_streams:Streams.main`" in message


@pytest.mark.parametrize(
    "source",
    [
        "def main():\n    pass\n",
        "def main(*streams):\n    pass\n",
        "def main(*, stream):\n    pass\n",
        "def main(**streams):\n    pass\n",
        "def main(stream, radius, /):\n    pass\n",
        "def main(stream, *radii):\n    pass\n",
        "def main(stream, **settings):\n    pass\n",
    ],
    ids=[
        "none",
        "var-positional",
        "keyword-only",
        "var-keyword",
        "a-second-positional-only",
        "var-positional-settings",
        "var-keyword-settings",
    ],
)
def test_a_function_not_taking_the_builder_then_named_settings_is_refused(
    source: str,
) -> None:
    namespace: "dict[str, Any]" = {"__name__": "rig_streams"}
    exec(source, namespace)

    with pytest.raises(TypeError, match="as its one positional parameter") as refusal:
        graph_generator(namespace["main"])
    assert "(stream_builder: StreamBuilder, radius: int = 4) -> None:" in str(refusal.value)


@pytest.mark.parametrize(
    "source",
    [
        "def main(stream_builder, radius):\n    pass\n",
        "def main(stream_builder, radius=4, *, tint='warm'):\n    pass\n",
    ],
    ids=["a-setting", "a-keyword-only-setting"],
)
def test_a_function_taking_the_builder_then_named_settings_is_a_graph_generator(
    source: str,
) -> None:
    namespace: "dict[str, Any]" = {"__name__": "rig_streams"}
    exec(source, namespace)

    assert is_graph_generator(graph_generator(namespace["main"]))


@pytest.mark.parametrize(
    "callable_but_not_a_plain_function",
    [UndecoratedFilter, len, StreamBuilder("rig").add],
    ids=["a-class", "a-builtin", "a-bound-method"],
)
def test_a_callable_that_is_not_a_plain_function_is_refused_as_what_it_is(
    callable_but_not_a_plain_function: Any,
) -> None:
    with pytest.raises(TypeError) as refusal:
        graph_generator(callable_but_not_a_plain_function)

    message = str(refusal.value)
    assert message.startswith("@graph_generator decorates a plain module-level function")
    assert "@graph_generator takes no arguments" not in message


@pytest.mark.parametrize(
    "not_callable",
    ["rig", UndecoratedFilter()],
    ids=["a-string", "an-instance"],
)
def test_what_is_not_callable_is_refused_naming_the_bare_form(
    not_callable: Any,
) -> None:
    with pytest.raises(TypeError) as refusal:
        graph_generator(not_callable)

    message = str(refusal.value)
    assert message.startswith("@graph_generator decorates a plain module-level function")
    assert "@graph_generator takes no arguments: the name is the function's" in message


@pytest.mark.parametrize(
    ("class_member_decorator", "descriptor_type_as_written"),
    [
        ("classmethod", "classmethod"),
        ("staticmethod", "staticmethod"),
        ("property", "property"),
        ("functools.cached_property", "functools.cached_property"),
    ],
    ids=["classmethod", "staticmethod", "property", "cached-property"],
)
def test_stream_stacked_over_a_class_member_decorator_is_refused_naming_it(
    class_member_decorator: str, descriptor_type_as_written: str
) -> None:
    namespace: "dict[str, Any]" = {
        "__name__": "rig_streams",
        "functools": functools,
        "graph_generator": graph_generator,
    }

    with pytest.raises(TypeError) as refusal:
        exec(
            f"@graph_generator\n@{class_member_decorator}\ndef main(stream_builder):\n    pass\n",
            namespace,
        )

    message = str(refusal.value)
    assert message.startswith("@graph_generator decorates a plain module-level function")
    assert (
        f"is of type `{descriptor_type_as_written}`, which makes a class member of what it "
        f"wraps. Put `@graph_generator` on a plain module-level `def`, with no "
        f"`@{descriptor_type_as_written}` under it."
    ) in message
    assert "@graph_generator takes no arguments" not in message


def test_a_lambda_is_refused_naming_def() -> None:
    namespace: "dict[str, Any]" = {"__name__": "rig_streams"}
    exec("main = lambda stream: None\n", namespace)

    with pytest.raises(TypeError, match="`def`"):
        graph_generator(namespace["main"])


@pytest.mark.parametrize(
    "source",
    [
        "async def main(stream_builder):\n    pass\n",
        "def main(stream_builder):\n    yield stream_builder\n",
        "async def main(stream_builder):\n    yield stream_builder\n",
    ],
    ids=["async", "generator", "async-generator"],
)
def test_a_function_whose_call_runs_none_of_its_body_is_refused(source: str) -> None:
    namespace: "dict[str, Any]" = {"__name__": "rig_streams"}
    exec(source, namespace)

    with pytest.raises(TypeError, match="runs none of its body"):
        graph_generator(namespace["main"])


def test_is_graph_generator_tells_a_stream_from_everything_else() -> None:
    assert is_graph_generator(every_kind_of_node)
    assert not is_graph_generator(names_of_nodes_added)
    assert not is_graph_generator(UndecoratedFilter)
    assert not is_graph_generator(None)
    assert not is_graph_generator("every_kind_of_node")


def test_the_builder_is_on_the_public_surface_and_the_cli_helper_is_not() -> None:
    for public_name in (
        "NodeInputPortReference",
        "NodeOutputPortReference",
        "NodeReference",
        "GraphFragmentReference",
        "StreamBuilder",
        "graph_generator",
        "run_graph_generator",
    ):
        assert public_name in tatolab.stream.__all__
    assert "is_graph_generator" not in tatolab.stream.__all__


def test_the_builder_is_not_importable_as_stream() -> None:
    assert "Stream" not in tatolab.stream.__all__
    with pytest.raises(ImportError, match="'Stream'"):
        exec("from tatolab.stream import Stream", {})


def test_the_builder_module_imports_nothing_native() -> None:
    imported: "list[tuple[int, str]]" = []
    for builder_source in STREAM_GRAPH_BUILDER_SOURCES:
        module = ast.parse(builder_source.read_text(encoding="utf-8"))
        for statement in ast.walk(module):
            if isinstance(statement, ast.Import):
                imported.extend((0, alias.name) for alias in statement.names)
            elif isinstance(statement, ast.ImportFrom):
                imported.append((statement.level, statement.module or ""))

    assert imported
    for level, module_name in imported:
        if level == 0:
            assert module_name.split(".")[0] in sys.stdlib_module_names, module_name
        else:
            assert (level, module_name) in {
                (1, "_exposed_name_cast"),
                (1, "_built_in_node"),
                (1, "_node_declaration"),
                (1, "_function_node_declaration"),
                (1, "_stream_clock_spec"),
                (1, "_port_family_declaration"),
                (1, "_stream_builder"),
                (1, "_graph_generator_detection"),
            }, module_name


def test_a_node_reference_in_config_compiles_to_the_node_object_it_names() -> None:
    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)
    stream_builder.add(
        BrightnessReader, config={"source": inverter, "nested": [{"also": inverter}]}
    )

    assert stream_builder._compiled_graph()["nodes"][1]["config"] == {
        "source": {"node": "frameinverter"},
        "nested": [{"also": {"node": "frameinverter"}}],
    }


def test_a_node_reference_to_a_node_the_stream_does_not_hold_is_refused_at_add() -> None:
    elsewhere = StreamBuilder("elsewhere").add(FrameInverter, name="gateway")
    stream_builder = StreamBuilder("rig")
    stream_builder.add(FrameInverter)

    with pytest.raises(ValueError, match="node `gateway` is not in stream `rig`"):
        stream_builder.add(BrightnessReader, config={"source": elsewhere})


def test_a_variable_set_from_a_port_and_read_twice_compiles_to_its_drawn_nodes_and_links() -> None:
    from tatolab.stream import GetVariable, SetVariable

    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)
    setter = stream_builder.add(SetVariable, config={"variable": "inverted"})
    stream_builder.connect(inverter.output("video_to_downstream"), setter.input("value"))
    for reader_name in ("front", "back"):
        getter = stream_builder.add(GetVariable, config={"variable": "inverted"})
        reader = stream_builder.add(BrightnessReader, name=reader_name)
        stream_builder.connect(getter.output("value"), reader.input("video_from_upstream"))

    graph = stream_builder._compiled_graph()
    assert [(node["name"], node["type"]) for node in graph["nodes"]] == [
        ("frameinverter", FrameInverter.__module__ + ":FrameInverter"),
        ("setvariable", "tatolab.stream:SetVariable"),
        ("getvariable", "tatolab.stream:GetVariable"),
        ("front", BrightnessReader.__module__ + ":BrightnessReader"),
        ("getvariable-2", "tatolab.stream:GetVariable"),
        ("back", BrightnessReader.__module__ + ":BrightnessReader"),
    ]
    assert graph["links"] == [
        {
            "source": {"node": "frameinverter", "port": "video_to_downstream"},
            "target": {"node": "setvariable", "port": "value"},
        },
        {
            "source": {"node": "getvariable", "port": "value"},
            "target": {"node": "front", "port": "video_from_upstream"},
        },
        {
            "source": {"node": "getvariable-2", "port": "value"},
            "target": {"node": "back", "port": "video_from_upstream"},
        },
    ]


def test_a_variable_holding_a_node_compiles_its_reference_and_a_get_passes_as_one() -> None:
    from tatolab.stream import GetVariable, SetVariable

    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)
    stream_builder.add(
        SetVariable, config={"variable": "connection", "node_reference": inverter}
    )
    getter = stream_builder.add(GetVariable, config={"variable": "connection"})
    stream_builder.add(BrightnessReader, config={"source": getter})

    nodes = stream_builder._compiled_graph()["nodes"]
    assert nodes[1]["config"] == {
        "variable": "connection",
        "node_reference": {"node": "frameinverter"},
    }
    assert nodes[3]["config"] == {"source": {"node": "getvariable"}}


def test_a_node_reference_in_the_deepest_container_a_config_nests_is_refused_as_too_deep() -> None:
    from tatolab.stream._stream_builder import (
        _MOST_CONTAINERS_A_CONFIG_NESTS_COUNTING_ITSELF,
    )

    def config_whose_deepest_container_holds(leaf: object) -> dict[str, object]:
        config: dict[str, object] = {"leaf": leaf}
        for _ in range(_MOST_CONTAINERS_A_CONFIG_NESTS_COUNTING_ITSELF - 1):
            config = {"down": config}
        return config

    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)
    stream_builder.add(BrightnessReader, config=config_whose_deepest_container_holds("a str"))

    with pytest.raises(ValueError, match="config nests too deep"):
        stream_builder.add(
            BrightnessReader, config=config_whose_deepest_container_holds(inverter)
        )


def test_interface_nodes_compile_with_their_family_written_by_its_wire_name() -> None:
    from tatolab.stream import GraphInput, GraphOutput, Image

    stream_builder = StreamBuilder("rig")
    entering = stream_builder.add(GraphInput, config={"port": "camera", "family": Image})
    inverter = stream_builder.add(FrameInverter)
    leaving = stream_builder.add(GraphOutput, config={"port": "inverted", "family": Image})
    stream_builder.connect(entering.output("value"), inverter.input("video_from_upstream"))
    stream_builder.connect(inverter.output("video_to_downstream"), leaving.input("value"))

    graph = stream_builder._compiled_graph()
    assert [(node["name"], node["type"], node["config"]) for node in graph["nodes"]] == [
        ("graphinput", "tatolab.stream:GraphInput", {"port": "camera", "family": "image"}),
        ("frameinverter", FrameInverter.__module__ + ":FrameInverter", {}),
        ("graphoutput", "tatolab.stream:GraphOutput", {"port": "inverted", "family": "image"}),
    ]
    assert graph["links"] == [
        {
            "source": {"node": "graphinput", "port": "value"},
            "target": {"node": "frameinverter", "port": "video_from_upstream"},
        },
        {
            "source": {"node": "frameinverter", "port": "video_to_downstream"},
            "target": {"node": "graphoutput", "port": "value"},
        },
    ]

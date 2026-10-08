# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What `tatolabd` loads from a graph file, and what it refuses by name.

A graph is the JSON `compile_stream_to_graph` returns, written to the file
`tatolabd --stream-graph` reads; its nodes name their types through the
built-ins' own `type` or a `@node` class's import path. Every load here but the
rig's runs with no Vulkan driver reachable: a refused load ends with its
refusal, and a load that succeeds logs `the stream ... loaded with N nodes` and
is then refused at the GPU, before any device opens.

A Python node type is described in a processor interpreter started from the
suite venv, which holds `tatolab-stream` and nothing of the runtime.
"""

from __future__ import annotations

import errno
import os
import re
import signal
import sys
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from conftest import StreamGraphLoadOutcome, environment_reaching_no_vulkan_driver
from node_module_whose_describe_holds_the_load import (
    HELD_DESCRIBE_DEADLINE_SECONDS,
    NodeModuleWhoseDescribeHoldsTheLoad,
)
from runtime_load_open_config_nodes import OpenConfigSink
from runtime_load_served_graph_nodes import LoadedFrameSink
from runtime_process_under_test import STREAM_NEVER_STARTED_LOG_LINE_FRAGMENT, RuntimeProcessUnderTest
from tatolab.stream import (
    CameraSource,
    DisplayWindow,
    H264Decoder,
    H264Encoder,
    H265Decoder,
    H265Encoder,
    MicrophoneSource,
    Mp4Sink,
    OpusDecoder,
    OpusEncoder,
    SpeakerSink,
    StreamBuilder,
    TestPatternSource,
    VirtualCameraSink,
    stream,
)

EVERY_BUILT_IN_NODE_CLASS: "list[type]" = [
    TestPatternSource,
    CameraSource,
    DisplayWindow,
    MicrophoneSource,
    SpeakerSink,
    H264Encoder,
    H264Decoder,
    H265Encoder,
    H265Decoder,
    OpusEncoder,
    OpusDecoder,
    Mp4Sink,
    VirtualCameraSink,
]

# The native VirtualCameraSink is compiled on Linux only.
BUILT_IN_NODE_CLASSES_THIS_PLATFORM_COMPILES = [
    built_in_node_class
    for built_in_node_class in EVERY_BUILT_IN_NODE_CLASS
    if built_in_node_class is not VirtualCameraSink or sys.platform.startswith("linux")
]

# A setting a built-in's config requires; every other built-in loads with `{}`.
THE_CONFIG_A_BUILT_IN_CANNOT_LOAD_WITHOUT: "dict[type, dict[str, object]]" = {
    Mp4Sink: {"path": "recording.mp4"},
}

OPEN_CONFIG_SINK_TYPE = "runtime_load_open_config_nodes:OpenConfigSink"

# Imported by nothing in this process: `tatolabd` describes it in a processor interpreter.
DESCRIBED_NODE_MODULE = "runtime_load_nodes"
DESCRIBED_NODE_TYPE = f"{DESCRIBED_NODE_MODULE}:LoadedFrameRelay"

# The deepest config the builder compiles: 126 containers from the graph's
# root, less the graph, its `nodes` list and the node enclosing the config.
CONTAINERS_A_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF = 123

# The deepest config a graph file `tatolabd` parses carries: its parser reads
# 127 nested containers, one more than the builder compiles, which leaves
# `tatolab` room for the compile document wrapping the graph.
CONTAINERS_A_CONFIG_IN_A_GRAPH_FILE_NESTS_AT_MOST_COUNTING_ITSELF = 124

# The graph file's parser refuses JSON nested past the depth the builder compiles.
GRAPH_FILE_NESTED_PAST_THE_MAXIMUM = "recursion limit exceeded"
GRAPH_FILE_IS_NOT_A_GRAPH = "is not a graph this runtime loads"

SERVED_GRAPH_RUNTIME_NAME = f"runtime-load-served-graph-{os.getpid()}"

LoadStreamGraphOnTatolabd = Callable[..., StreamGraphLoadOutcome]


def pattern_to_window_graph(*, stream_name: "str | None" = None) -> "dict[str, Any]":
    """A test pattern into a window, exposing the pattern's output."""
    graph: "dict[str, Any]" = {
        "nodes": [
            {"name": "testpatternsource", "type": TestPatternSource.type, "config": {}},
            {
                "name": "displaywindow",
                "type": DisplayWindow.type,
                "config": {"title": "load", "scaling": "fit"},
            },
        ],
        "links": [
            {
                "source": {"node": "testpatternsource", "port": "video"},
                "target": {"node": "displaywindow", "port": "video"},
            }
        ],
        "exposed": [{"node": "testpatternsource", "port": "video"}],
    }
    if stream_name is not None:
        graph["stream"] = stream_name
    return graph


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


def empty_graph() -> "dict[str, Any]":
    """What a stream whose function adds nothing compiles to."""
    return {"stream": "main", "nodes": [], "links": [], "exposed": []}


def refused_naming(load_outcome: StreamGraphLoadOutcome) -> str:
    """A refused load's reason; fails if the graph loaded."""
    assert not load_outcome.loaded, (
        f"the graph loaded; it should have been refused:\n{load_outcome.stderr_text[-4000:]}"
    )
    assert load_outcome.exit_status == 1, load_outcome.stderr_text[-4000:]
    assert load_outcome.refusal is not None, load_outcome.stderr_text[-4000:]
    return load_outcome.refusal


def loaded_with(load_outcome: StreamGraphLoadOutcome) -> int:
    """How many of the stream's nodes a load that succeeded loaded; fails if it was refused."""
    assert load_outcome.loaded, (
        f"the graph was refused: {load_outcome.refusal}\n{load_outcome.stderr_text[-4000:]}"
    )
    assert load_outcome.loaded_node_count is not None
    return load_outcome.loaded_node_count


# ---- built-in types ---------------------------------------------------------


def test_a_graph_naming_every_compiled_built_in_by_its_type_loads(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    load_outcome = load_stream_graph_on_tatolabd(
        {
            "nodes": [
                {
                    "name": built_in_node_class.__name__,
                    "type": built_in_node_class.type,
                    "config": THE_CONFIG_A_BUILT_IN_CANNOT_LOAD_WITHOUT.get(built_in_node_class, {}),
                }
                for built_in_node_class in BUILT_IN_NODE_CLASSES_THIS_PLATFORM_COMPILES
            ]
        }
    )

    assert loaded_with(load_outcome) == len(BUILT_IN_NODE_CLASSES_THIS_PLATFORM_COMPILES)


# ---- loading ----------------------------------------------------------------


def test_a_loaded_graphs_nodes_are_in_the_runtimes_graph(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    load_outcome = load_stream_graph_on_tatolabd(
        pattern_to_window_graph(stream_name="pattern-to-window")
    )

    assert loaded_with(load_outcome) == 2
    assert load_outcome.loaded_stream_name == "pattern-to-window"


def test_the_deepest_config_the_builder_compiles_loads(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    load_outcome = load_stream_graph_on_tatolabd(
        open_config_sink_with_the_deepest_config_the_builder_compiles
    )

    assert loaded_with(load_outcome) == 1


def test_a_config_one_container_deeper_than_a_graph_file_carries_is_refused_by_load(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    """One container past what the graph file's parser reads, written by hand, is refused."""
    load_outcome = load_stream_graph_on_tatolabd(
        {
            "nodes": [
                {
                    "name": "displaywindow",
                    "type": DisplayWindow.type,
                    "config": config_nesting_containers_deep(
                        CONTAINERS_A_CONFIG_IN_A_GRAPH_FILE_NESTS_AT_MOST_COUNTING_ITSELF + 1
                    ),
                }
            ]
        }
    )

    refusal = refused_naming(load_outcome)
    assert GRAPH_FILE_IS_NOT_A_GRAPH in refusal
    assert GRAPH_FILE_NESTED_PAST_THE_MAXIMUM in refusal


@pytest.mark.parametrize("not_a_number", ["NaN", "Infinity"], ids=["nan", "infinity"])
def test_nan_and_infinity_in_a_graph_files_config_are_refused_as_not_json(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd, not_a_number: str
):
    """JSON carries neither, so a graph file holding one is not a graph; the
    builder refuses both before a file is written."""
    load_outcome = load_stream_graph_on_tatolabd(
        '{"nodes": [{"name": "openconfigsink", "type": "%s", "config": {"value": %s}}]}'
        % (OPEN_CONFIG_SINK_TYPE, not_a_number)
    )

    refusal = refused_naming(load_outcome)
    assert GRAPH_FILE_IS_NOT_A_GRAPH in refusal
    assert "the graph does not parse" in refusal


def test_a_graph_file_holding_a_list_rather_than_a_graph_is_refused_naming_the_file(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    load_outcome = load_stream_graph_on_tatolabd('[{"name": "x", "type": "y"}]')

    refusal = refused_naming(load_outcome)
    assert re.search(r"--stream-graph \S+\.json " + re.escape(GRAPH_FILE_IS_NOT_A_GRAPH), refusal)
    assert "the graph does not parse" in refusal


def test_a_lone_surrogate_escape_in_a_graph_file_is_refused_naming_it(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    load_outcome = load_stream_graph_on_tatolabd(
        '{"nodes": [{"name": "displaywindow", "type": "%s", "config": {"title": "\\udc80"}}]}'
        % DisplayWindow.type
    )

    refusal = refused_naming(load_outcome)
    assert GRAPH_FILE_IS_NOT_A_GRAPH in refusal
    assert "lone leading surrogate" in refusal


def test_a_config_integer_wider_than_64_bits_in_a_graph_file_is_refused_naming_it(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    """The builder refuses one before a file is written; a graph file written
    any other way must not reach a node carrying a different number."""
    load_outcome = load_stream_graph_on_tatolabd(
        '{"nodes": [{"name": "openconfigsink", "type": "%s", "config": {"value": %d}}]}'
        % (OPEN_CONFIG_SINK_TYPE, 2**64)
    )

    refusal = refused_naming(load_outcome)
    assert str(2**64) in refusal


def test_a_python_node_type_the_process_never_imported_loads_by_describing_it(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    assert DESCRIBED_NODE_MODULE not in sys.modules, "nothing in this process may import it"

    load_outcome = load_stream_graph_on_tatolabd(
        {
            "stream": "relayed",
            "nodes": [
                {"name": "testpatternsource", "type": TestPatternSource.type, "config": {}},
                {"name": "loadedframerelay", "type": DESCRIBED_NODE_TYPE, "config": {}},
                {"name": "displaywindow", "type": DisplayWindow.type, "config": {}},
            ],
            "links": [
                {
                    "source": {"node": "testpatternsource", "port": "video"},
                    "target": {"node": "loadedframerelay", "port": "video_from_upstream"},
                },
                {
                    "source": {"node": "loadedframerelay", "port": "video_to_downstream"},
                    "target": {"node": "displaywindow", "port": "video"},
                },
            ],
            "exposed": [{"node": "loadedframerelay", "port": "video_to_downstream"}],
        }
    )

    assert loaded_with(load_outcome) == 3
    assert DESCRIBED_NODE_MODULE not in sys.modules


def test_the_stream_name_a_graph_carries_is_cast(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    load_outcome = load_stream_graph_on_tatolabd(pattern_to_window_graph(stream_name="Front Camera"))

    assert loaded_with(load_outcome) == 2
    assert load_outcome.loaded_stream_name == "front-camera"


@pytest.mark.skipif(sys.platform == "win32", reason="surrogate-escaped paths are POSIX's")
def test_a_project_directory_whose_name_is_not_utf8_is_accepted(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd, tmp_path: Path
):
    """A directory name that is not UTF-8 is still a path `tatolabd` hands its
    processor interpreters; the describe of a Python type imports from it."""
    project_directory_not_utf8 = bytes(tmp_path) + b"/caf\xe9"
    try:
        os.mkdir(project_directory_not_utf8)
    except OSError as refused_name:
        if refused_name.errno != errno.EILSEQ:
            raise
        # APFS stores names as UTF-8 only, so no project there can carry one.
        pytest.skip("this filesystem refuses a file name that is not UTF-8")
    (Path(os.fsdecode(project_directory_not_utf8)) / f"{DESCRIBED_NODE_MODULE}.py").write_text(
        (Path(__file__).with_name(f"{DESCRIBED_NODE_MODULE}.py")).read_text()
    )

    load_outcome = load_stream_graph_on_tatolabd(
        {
            "nodes": [
                {"name": "testpatternsource", "type": TestPatternSource.type, "config": {}},
                {"name": "loadedframerelay", "type": DESCRIBED_NODE_TYPE, "config": {}},
            ]
        },
        project_directory=project_directory_not_utf8,
    )

    assert loaded_with(load_outcome) == 2


# ---- refusals ---------------------------------------------------------------


def test_an_empty_graph_is_refused_by_name(load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd):
    refusal = refused_naming(load_stream_graph_on_tatolabd(empty_graph()))

    assert "the stream `main` holds no node" in refusal
    assert "stream_builder.add(" in refusal


def test_a_stream_name_casting_to_nothing_is_refused_naming_it(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    refusal = refused_naming(load_stream_graph_on_tatolabd(pattern_to_window_graph(stream_name="..")))

    assert "`..`" in refusal
    assert "cannot name anything" in refusal


def test_a_graph_file_nested_far_past_the_maximum_is_refused_rather_than_crashing(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    nested_far_past_the_maximum = "[" * 100_000 + "]" * 100_000
    load_outcome = load_stream_graph_on_tatolabd(
        '{"nodes": [{"name": "displaywindow", "type": "%s", "config": {"nested": %s}}]}'
        % (DisplayWindow.type, nested_far_past_the_maximum)
    )

    refusal = refused_naming(load_outcome)
    assert GRAPH_FILE_IS_NOT_A_GRAPH in refusal
    assert GRAPH_FILE_NESTED_PAST_THE_MAXIMUM in refusal


def test_a_virtual_camera_sink_loads_on_linux_and_is_refused_at_load_naming_the_platform_elsewhere(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    load_outcome = load_stream_graph_on_tatolabd(
        {"nodes": [{"name": "camera", "type": VirtualCameraSink.type, "config": {}}]}
    )
    if sys.platform.startswith("linux"):
        assert loaded_with(load_outcome) == 1
        return

    refusal = refused_naming(load_outcome)
    assert "`tatolab.stream:VirtualCameraSink`" in refusal
    assert "macOS" in refusal
    assert "runs on Linux only" in refusal


def test_a_setting_a_built_in_does_not_take_is_refused_at_load_naming_the_node_and_the_setting(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    refusal = refused_naming(
        load_stream_graph_on_tatolabd(
            {"nodes": [{"name": "pattern", "type": TestPatternSource.type, "config": {"widht": 640}}]}
        )
    )

    assert "node `pattern`" in refusal
    assert "`tatolab.stream:TestPatternSource`" in refusal
    assert "`widht`" in refusal


def test_a_graph_that_does_not_parse_is_refused_with_the_engines_text(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    refusal = refused_naming(load_stream_graph_on_tatolabd({"nodes": [{"name": "nameless-type"}]}))

    assert "the graph does not parse" in refusal


@pytest.mark.parametrize(
    ("unknown_type", "engine_refusal"),
    [
        ("tatolab.stream:NoSuchNode", "has no node type"),
        (
            "no_such_module_for_runtime_load:NoSuchNode",
            "No module named 'no_such_module_for_runtime_load'",
        ),
        (f"{DESCRIBED_NODE_MODULE}:NoSuchNode", "has no attribute 'NoSuchNode'"),
    ],
    ids=["native-path", "python-path", "python-module-without-the-class"],
)
def test_a_graph_naming_an_unknown_type_is_refused(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd, unknown_type: str, engine_refusal: str
):
    refusal = refused_naming(
        load_stream_graph_on_tatolabd(
            {"nodes": [{"name": "unknown", "type": unknown_type, "config": {}}]}
        )
    )

    assert engine_refusal in refusal
    assert unknown_type in refusal


# ---- links: a port no node has, and an end naming a runtime -----------------


@stream
def pattern_linked_from_a_port_it_lacks_into_a_window(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(TestPatternSource)
    window = stream_builder.add(DisplayWindow)
    stream_builder.connect(pattern.output("no_such_port"), window.input("video"))


def test_a_link_from_a_port_its_node_lacks_is_refused_by_load_naming_the_port(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
):
    refusal = refused_naming(
        load_stream_graph_on_tatolabd(pattern_linked_from_a_port_it_lacks_into_a_window)
    )

    assert "`no_such_port`" in refusal


@pytest.mark.parametrize("end", ["source", "target"])
def test_a_link_end_naming_a_runtime_is_refused_by_load_naming_it(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd, end: str
):
    """Both ends of a link are on the runtime that loads it, so an end naming a
    runtime is refused rather than read as a local end with its runtime dropped."""
    graph = pattern_to_window_graph()
    graph["links"][0][end]["runtime_name"] = "studio-display-9f3c"

    refusal = refused_naming(load_stream_graph_on_tatolabd(graph))

    assert "studio-display-9f3c" in refusal


# ---- an interrupt during a load's describe ------------------------------------


def test_a_ctrl_c_during_a_loads_describe_ends_tatolabd_at_once(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]",
    held_node_module: NodeModuleWhoseDescribeHoldsTheLoad,
    tmp_path: Path,
):
    """The describe leads its own process group, so a terminal's Ctrl-C reaches
    only `tatolabd`; the load still has to give it up rather than wait the
    describe's bound out, and a stream never started is a clean exit."""
    tatolabd = start_tatolabd(
        {"nodes": [{"name": "held", "type": f"{held_node_module.name}:LoadedFrameRelay", "config": {}}]},
        project_directory=held_node_module.project_directory,
        extra_environment=environment_reaching_no_vulkan_driver(tmp_path),
    )
    assert held_node_module.wait_until_the_load_reaches_the_import(), (
        f"the load never reached the describe:\n{tatolabd.recent_stderr()}"
    )
    interrupted_at = time.monotonic()
    tatolabd.send_signal(signal.SIGINT)
    exit_status = tatolabd.await_exit(timeout=15)
    interrupt_honoured_within_seconds = time.monotonic() - interrupted_at

    assert exit_status == 0, tatolabd.recent_stderr()
    assert STREAM_NEVER_STARTED_LOG_LINE_FRAGMENT in tatolabd.stderr_text, tatolabd.recent_stderr()
    assert interrupt_honoured_within_seconds < HELD_DESCRIBE_DEADLINE_SECONDS / 2


# ---- a loaded stream, run and read back by name -----------------------------


@stream
def named_pattern_into_a_named_sink(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(
        TestPatternSource, name="Loaded Pattern", config={"width": 320, "height": 180}
    )
    sink = stream_builder.add(LoadedFrameSink, name="Loaded Sink")
    stream_builder.connect(pattern.output("video"), sink.input("bags_from_upstream"))


@pytest.mark.requires_gpu
def test_a_loaded_streams_nodes_and_link_are_served_by_name_over_its_local_api(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]",
):
    tatolabd = start_tatolabd(
        named_pattern_into_a_named_sink,
        extra_environment={"STREAMLIB_RUNTIME_NAME": SERVED_GRAPH_RUNTIME_NAME},
    )
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(expected_node_names=["loaded-pattern", "loaded-sink"])
    served_graph = local_api.call_tool("graph", {})
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert served_graph["stream"] == "named_pattern_into_a_named_sink"
    assert served_graph["runtime_name"] == SERVED_GRAPH_RUNTIME_NAME
    served_node_names = [served_node["name"] for served_node in served_graph["nodes"]]
    assert "loaded-pattern" in served_node_names, served_node_names
    assert "loaded-sink" in served_node_names, served_node_names
    served_link_ends = [
        (
            served_link["source"]["node"],
            served_link["source"]["port"],
            served_link["target"]["node"],
            served_link["target"]["port"],
        )
        for served_link in served_graph["links"]
    ]
    assert (
        "loaded-pattern",
        "video",
        "loaded-sink",
        "bags_from_upstream",
    ) in served_link_ends, served_link_ends

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What the runtime loads from a stream's graph, and what its load refuses by name.

A graph reaches `tatolabd` through `run_stream`, as `tatolab run` makes it: the
runtime compiles the project's stream in the project's own interpreter and
loads the graph that compile printed. A graph written here by hand goes
through a project whose interpreter prints it as the compile document, so the
runtime parses exactly the text a test wrote; a `@stream` function is compiled
from the suite project. Every run here but the rig's is on a `tatolabd` that
reaches no Vulkan driver: a refused load ends `tatolab run` with its refusal,
a load that succeeds is refused at its start, at the GPU, before any device
opens — and the runtime keeps serving either way, holding nothing.

A Python node type is described in a processor interpreter started from the
suite venv, which holds `tatolab-stream` and nothing of the runtime.
"""

from __future__ import annotations

import errno
import inspect
import os
import signal
import sys
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from conftest import (
    StreamRunWithNoVulkanDriverOutcome,
    TatolabdUnderTest,
    environment_reaching_no_vulkan_driver,
)
from node_module_whose_describe_holds_the_load import (
    HELD_DESCRIBE_DEADLINE_SECONDS,
    NodeModuleWhoseDescribeHoldsTheLoad,
)
from runtime_load_streams import (
    CONTAINERS_A_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF,
    config_nesting_containers_deep,
    named_pattern_into_a_named_sink,
    open_config_sink_with_the_deepest_config_the_builder_compiles,
    pattern_linked_from_a_port_it_lacks_into_a_window,
)
from runtime_process_under_test import STREAM_NEVER_STARTED_LOG_LINE_FRAGMENT
from stream_runs_on_tatolabd import TatolabRunOfAProject
from tatolab.stream import (
    DisplayWindow,
    Mp4Sink,
    TestPatternSource,
    VirtualCameraSink,
    _built_in_nodes,
)
from tatolab.stream._built_in_node import BuiltInNode

EVERY_BUILT_IN_NODE_CLASS: "list[type[BuiltInNode[Any]]]" = [
    exported
    for exported in vars(_built_in_nodes).values()
    if inspect.isclass(exported)
    and issubclass(exported, BuiltInNode)
    and exported is not BuiltInNode
]

# The native VirtualCameraSink is compiled on Linux only.
BUILT_IN_NODE_CLASSES_THIS_PLATFORM_COMPILES = [
    built_in_node_class
    for built_in_node_class in EVERY_BUILT_IN_NODE_CLASS
    if built_in_node_class is not VirtualCameraSink or sys.platform.startswith("linux")
]

# A setting a built-in's config requires; every other built-in loads with `{}`.
THE_CONFIG_A_BUILT_IN_CANNOT_LOAD_WITHOUT: "dict[type[BuiltInNode[Any]], dict[str, object]]" = {
    Mp4Sink: {"path": "recording.mp4"},
}

OPEN_CONFIG_SINK_TYPE = "runtime_load_open_config_nodes:OpenConfigSink"

# Imported by nothing in this process: `tatolabd` describes it in a processor interpreter.
DESCRIBED_NODE_MODULE = "runtime_load_nodes"
DESCRIBED_NODE_TYPE = f"{DESCRIBED_NODE_MODULE}:LoadedFrameRelay"

# The compile document's parser refuses JSON nested past the depth it reads.
COMPILE_DOCUMENT_NESTED_PAST_THE_MAXIMUM = "recursion limit exceeded"
NOT_THE_COMPILE_DOCUMENT = "standard output is not the compile document"

SERVED_GRAPH_RUNTIME_NAME = f"runtime-load-served-graph-{os.getpid()}"

RunStreamOnTatolabdWithNoVulkanDriver = Callable[..., StreamRunWithNoVulkanDriverOutcome]


def pattern_to_window_graph(*, stream_name: str = "pattern-to-window") -> "dict[str, Any]":
    """A test pattern into a window, exposing the pattern's output."""
    return {
        "stream": stream_name,
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


def empty_graph() -> "dict[str, Any]":
    """What a stream whose function adds nothing compiles to."""
    return {"stream": "main", "nodes": [], "links": [], "exposed": []}


def refused_naming(run_outcome: StreamRunWithNoVulkanDriverOutcome) -> str:
    """A refused load's reason; fails if the graph loaded."""
    assert not run_outcome.loaded, (
        f"the graph loaded; it should have been refused:\n"
        f"{run_outcome.tatolabd_stderr_text_during_the_run[-4000:]}"
    )
    assert run_outcome.tatolab_run_exit_status == 1, run_outcome.tatolab_run_stderr_text
    assert run_outcome.refusal is not None, run_outcome.tatolab_run_stderr_text
    return run_outcome.refusal


def loaded_with(run_outcome: StreamRunWithNoVulkanDriverOutcome) -> int:
    """How many of the stream's nodes a load that succeeded loaded; fails if it was refused."""
    assert run_outcome.loaded, (
        f"the graph was refused: {run_outcome.refusal}\n"
        f"{run_outcome.tatolabd_stderr_text_during_the_run[-4000:]}"
    )
    assert run_outcome.loaded_node_count is not None, run_outcome.tatolabd_stderr_text_during_the_run
    return run_outcome.loaded_node_count


# ---- built-in types ---------------------------------------------------------


def test_a_graph_naming_every_compiled_built_in_by_its_type_loads(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        {
            "stream": "every-built-in",
            "nodes": [
                {
                    "name": built_in_node_class.__name__,
                    "type": built_in_node_class.type,
                    "config": THE_CONFIG_A_BUILT_IN_CANNOT_LOAD_WITHOUT.get(built_in_node_class, {}),
                }
                for built_in_node_class in BUILT_IN_NODE_CLASSES_THIS_PLATFORM_COMPILES
            ],
        }
    )

    assert loaded_with(run_outcome) == len(BUILT_IN_NODE_CLASSES_THIS_PLATFORM_COMPILES)


# ---- loading ----------------------------------------------------------------


def test_a_loaded_graphs_nodes_are_in_the_runtimes_graph(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        pattern_to_window_graph(stream_name="pattern-to-window")
    )

    assert loaded_with(run_outcome) == 2
    assert run_outcome.loaded_stream_name == "pattern-to-window"


def test_the_deepest_config_the_builder_compiles_loads(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        open_config_sink_with_the_deepest_config_the_builder_compiles
    )

    assert loaded_with(run_outcome) == 1


def test_a_config_one_container_deeper_than_the_builder_compiles_is_refused_by_load(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    """The compile document's parser reads the deepest config the builder
    compiles and no deeper, so one container past it, written by hand, is refused."""
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        {
            "stream": "deep",
            "nodes": [
                {
                    "name": "openconfigsink",
                    "type": OPEN_CONFIG_SINK_TYPE,
                    "config": config_nesting_containers_deep(
                        CONTAINERS_A_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF + 1
                    ),
                }
            ],
        }
    )

    refusal = refused_naming(run_outcome)
    assert NOT_THE_COMPILE_DOCUMENT in refusal
    assert COMPILE_DOCUMENT_NESTED_PAST_THE_MAXIMUM in refusal


@pytest.mark.parametrize("not_a_number", ["NaN", "Infinity"], ids=["nan", "infinity"])
def test_nan_and_infinity_in_a_compiled_config_are_refused_as_not_json(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
    not_a_number: str,
):
    """JSON carries neither, so a compile document holding one is not one; the
    builder refuses both before a document is printed."""
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        '{"stream": "nan", "nodes": [{"name": "openconfigsink", "type": "%s", "config": {"value": %s}}]}'
        % (OPEN_CONFIG_SINK_TYPE, not_a_number)
    )

    refusal = refused_naming(run_outcome)
    assert NOT_THE_COMPILE_DOCUMENT in refusal
    assert "expected value" in refusal


def test_a_compiled_graph_that_is_a_list_rather_than_a_graph_is_refused_naming_it(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    refusal = refused_naming(run_stream_on_tatolabd_with_no_vulkan_driver('[{"name": "x", "type": "y"}]'))

    assert "names no stream" in refusal


def test_a_lone_surrogate_escape_in_a_compiled_config_is_refused_naming_it(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        '{"stream": "surrogate", "nodes": [{"name": "displaywindow", "type": "%s", '
        '"config": {"title": "\\udc80"}}]}' % DisplayWindow.type
    )

    refusal = refused_naming(run_outcome)
    assert NOT_THE_COMPILE_DOCUMENT in refusal
    assert "lone leading surrogate" in refusal


def test_a_config_integer_wider_than_64_bits_in_a_compiled_graph_is_refused_naming_it(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    """The builder refuses one before a document is printed; a compile document
    written any other way must not reach a node carrying a different number."""
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        '{"stream": "wide", "nodes": [{"name": "openconfigsink", "type": "%s", "config": {"value": %d}}]}'
        % (OPEN_CONFIG_SINK_TYPE, 2**64)
    )

    refusal = refused_naming(run_outcome)
    assert str(2**64) in refusal


def test_a_python_node_type_the_process_never_imported_loads_by_describing_it(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    assert DESCRIBED_NODE_MODULE not in sys.modules, "nothing in this process may import it"

    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
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

    assert loaded_with(run_outcome) == 3
    assert DESCRIBED_NODE_MODULE not in sys.modules


def test_the_stream_name_a_graph_carries_is_cast(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        pattern_to_window_graph(stream_name="Front Camera")
    )

    assert loaded_with(run_outcome) == 2
    assert run_outcome.loaded_stream_name == "front-camera"


@pytest.mark.skipif(sys.platform == "win32", reason="surrogate-escaped paths are POSIX's")
def test_a_project_directory_whose_name_is_not_utf8_is_refused_naming_it(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
    tmp_path: Path,
):
    """`run_stream` names its project directory in JSON, which carries UTF-8
    only, so `tatolab run` refuses a project it cannot name before the runtime
    is asked."""
    project_directory_not_utf8 = bytes(tmp_path) + b"/caf\xe9"
    try:
        os.mkdir(project_directory_not_utf8)
    except OSError as refused_name:
        if refused_name.errno != errno.EILSEQ:
            raise
        # APFS stores names as UTF-8 only, so no project there can carry one.
        pytest.skip("this filesystem refuses a file name that is not UTF-8")

    refusal = refused_naming(
        run_stream_on_tatolabd_with_no_vulkan_driver(
            TatolabRunOfAProject(working_directory=Path(os.fsdecode(project_directory_not_utf8)))
        )
    )

    assert "the project directory" in refusal
    assert "caf" in refusal
    assert "is not UTF-8" in refusal


# ---- refusals ---------------------------------------------------------------


def test_an_empty_graph_is_refused_by_name(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    refusal = refused_naming(run_stream_on_tatolabd_with_no_vulkan_driver(empty_graph()))

    assert "the stream `main` holds no node" in refusal
    assert "stream_builder.add(" in refusal


def test_a_graph_naming_no_stream_is_refused(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    """A runtime's streams are told apart by name, so a graph has to carry one."""
    graph = pattern_to_window_graph()
    del graph["stream"]

    refusal = refused_naming(run_stream_on_tatolabd_with_no_vulkan_driver(graph))

    assert "names no stream" in refusal


def test_a_stream_name_casting_to_nothing_is_refused_naming_it(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    refusal = refused_naming(
        run_stream_on_tatolabd_with_no_vulkan_driver(pattern_to_window_graph(stream_name=".."))
    )

    assert "`..`" in refusal
    assert "cannot name anything" in refusal


def test_a_compile_document_nested_far_past_the_maximum_is_refused_rather_than_crashing(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    nested_far_past_the_maximum = "[" * 100_000 + "]" * 100_000
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        '{"stream": "nested", "nodes": [{"name": "displaywindow", "type": "%s", '
        '"config": {"nested": %s}}]}' % (DisplayWindow.type, nested_far_past_the_maximum)
    )

    refusal = refused_naming(run_outcome)
    assert NOT_THE_COMPILE_DOCUMENT in refusal
    assert COMPILE_DOCUMENT_NESTED_PAST_THE_MAXIMUM in refusal


def test_a_virtual_camera_sink_loads_on_linux_and_is_refused_at_load_naming_the_platform_elsewhere(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        {"stream": "camera", "nodes": [{"name": "camera", "type": VirtualCameraSink.type, "config": {}}]}
    )
    if sys.platform.startswith("linux"):
        assert loaded_with(run_outcome) == 1
        return

    refusal = refused_naming(run_outcome)
    assert "`tatolab.stream:VirtualCameraSink`" in refusal
    assert "macOS" in refusal
    assert "runs on Linux only" in refusal


def test_a_setting_a_built_in_does_not_take_is_refused_at_load_naming_the_node_and_the_setting(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    refusal = refused_naming(
        run_stream_on_tatolabd_with_no_vulkan_driver(
            {
                "stream": "pattern",
                "nodes": [{"name": "pattern", "type": TestPatternSource.type, "config": {"widht": 640}}],
            }
        )
    )

    assert "node `pattern`" in refusal
    assert "`tatolab.stream:TestPatternSource`" in refusal
    assert "`widht`" in refusal


def test_a_graph_that_does_not_parse_is_refused_with_the_engines_text(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    refusal = refused_naming(
        run_stream_on_tatolabd_with_no_vulkan_driver(
            {"stream": "unparsed", "nodes": [{"name": "nameless-type"}]}
        )
    )

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
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
    unknown_type: str,
    engine_refusal: str,
):
    refusal = refused_naming(
        run_stream_on_tatolabd_with_no_vulkan_driver(
            {"stream": "unknown", "nodes": [{"name": "unknown", "type": unknown_type, "config": {}}]}
        )
    )

    assert engine_refusal in refusal
    assert unknown_type in refusal


# ---- links: a port no node has, and an end naming a runtime -----------------


def test_a_link_from_a_port_its_node_lacks_is_refused_by_load_naming_the_port(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    refusal = refused_naming(
        run_stream_on_tatolabd_with_no_vulkan_driver(pattern_linked_from_a_port_it_lacks_into_a_window)
    )

    assert "`no_such_port`" in refusal


@pytest.mark.parametrize("end", ["source", "target"])
def test_a_link_end_naming_a_runtime_is_refused_by_load_naming_it(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver, end: str
):
    """Both ends of a link are on the runtime that loads it, so an end naming a
    runtime is refused rather than read as a local end with its runtime dropped."""
    graph = pattern_to_window_graph()
    graph["links"][0][end]["runtime_name"] = "studio-display-9f3c"

    refusal = refused_naming(run_stream_on_tatolabd_with_no_vulkan_driver(graph))

    assert "studio-display-9f3c" in refusal


# ---- an interrupt during a load's describe ------------------------------------


def test_a_ctrl_c_during_a_loads_describe_ends_tatolabd_at_once(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
    held_node_module: NodeModuleWhoseDescribeHoldsTheLoad,
    tmp_path: Path,
):
    """The describe leads its own process group, so a terminal's Ctrl-C reaches
    only `tatolabd`; the load still has to give it up rather than wait the
    describe's bound out, and a stream never started is a clean exit."""
    tatolabd = start_tatolabd_running_stream(
        {
            "stream": "held",
            "nodes": [{"name": "held", "type": f"{held_node_module.name}:LoadedFrameRelay", "config": {}}],
        },
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
    (attached_run,) = tatolabd.attached_stream_runs
    assert attached_run.await_exit() != 0, attached_run.recent_stderr()
    assert " loaded (" not in attached_run.stderr_text, attached_run.recent_stderr()


# ---- a loaded stream, run and read back by name -----------------------------


@pytest.mark.requires_gpu
def test_a_loaded_streams_nodes_and_link_are_served_by_name_over_its_local_api(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd_running_stream(
        named_pattern_into_a_named_sink,
        extra_environment={"STREAMLIB_RUNTIME_NAME": SERVED_GRAPH_RUNTIME_NAME},
    )
    stream_name = tatolabd.attached_stream_runs[-1].await_loaded()["stream_name"]
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(
        stream=stream_name, expected_node_names=["loaded-pattern", "loaded-sink"]
    )
    served_graph = local_api.call_tool("graph", {"stream": stream_name})
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

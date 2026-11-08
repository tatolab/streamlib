# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What the runtime loads from a stream's graph, and what its load refuses by name.

A graph reaches `tatolabd` through `run_project`, as `tatolab run` makes it: the
runtime reads each graph document of the project and loads the graph it holds. A
graph written here by hand is written as a project's graph document, the text a
test wrote verbatim but for its stream asset, so the runtime parses exactly that
text; a `@stream` function is compiled in this process to its graph document. Every
run here but the rig's is on a `tatolabd` that reaches no Vulkan driver: a document
the runtime cannot load is a stream recorded failed with the reason; a load that
succeeds fails at its start, at the GPU, before any device opens — and the runtime
keeps serving either way.

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
    config_nesting_containers_deep,
    named_pattern_into_a_named_sink,
    open_config_sink_with_the_deepest_config_the_builder_compiles,
    pattern_linked_from_a_port_it_lacks_into_an_encoder,
)
from runtime_process_under_test import STREAM_NEVER_STARTED_LOG_LINE_FRAGMENT
from stream_runs_on_tatolabd import (
    TatolabRunOfAProject,
    stream_id_of_a_hand_written_graph,
    stream_id_of_a_one_stream_project,
)
from tatolab.stream import _built_in_nodes
from tatolab.stream._built_in_node import BuiltInNode

#: The H.264 encoder module's node type, which the suite venv installs.
H264_ENCODER_MODULE_TYPE = "tatolab.h264.processors:H264Encoder"
#: The test pattern module's node type, which the suite venv installs.
TEST_PATTERN_MODULE_TYPE = "tatolab.testpattern:TestPatternSource"

EVERY_BUILT_IN_NODE_CLASS: "list[type[BuiltInNode[Any]]]" = [
    exported
    for exported in vars(_built_in_nodes).values()
    if inspect.isclass(exported)
    and issubclass(exported, BuiltInNode)
    and exported is not BuiltInNode
]

# The keys a built-in's config cannot leave out; every other built-in loads with none.
CONFIG_A_BUILT_IN_CANNOT_LEAVE_OUT: "dict[type[BuiltInNode[Any]], dict[str, Any]]" = {
    _built_in_nodes.ArrayToChannels: {"channel_names": ["left", "right"], "sample_rate": 48_000},
    _built_in_nodes.ChannelsToRecords: {"channel_names": ["level"]},
    _built_in_nodes.RecordsToChannels: {"channel_names": ["level"], "sample_rate": 48_000},
}

OPEN_CONFIG_SINK_TYPE = "runtime_load_open_config_nodes:OpenConfigSink"

# Imported by nothing in this process: `tatolabd` describes it in a processor interpreter.
DESCRIBED_NODE_MODULE = "runtime_load_nodes"
DESCRIBED_NODE_TYPE = f"{DESCRIBED_NODE_MODULE}:LoadedFrameRelay"

# The graph document's parser refuses JSON nested past the depth it reads.
GRAPH_DOCUMENT_NESTED_PAST_THE_MAXIMUM = "recursion limit exceeded"
#: How the runtime names a graph document that is not JSON.
A_GRAPH_DOCUMENT_THAT_DOES_NOT_PARSE = "does not parse"
#: The deepest config a graph document's parser reads: 127 containers from the document's
#: root, less the document, its `nodes` list and the node enclosing the config.
CONTAINERS_A_GRAPH_DOCUMENTS_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF = 124

RunStreamOnTatolabdWithNoVulkanDriver = Callable[..., StreamRunWithNoVulkanDriverOutcome]


def pattern_to_encoder_graph(*, stream_name: str = "pattern-to-encoder") -> "dict[str, Any]":
    """A test pattern into an H.264 encoder."""
    return {
        "stream": stream_name,
        "nodes": [
            {"name": "testpatternsource", "type": TEST_PATTERN_MODULE_TYPE, "config": {}},
            {
                "name": "h264encoder",
                "type": H264_ENCODER_MODULE_TYPE,
                "config": {"keyframe_interval_seconds": 2, "fps": 30},
            },
        ],
        "links": [
            {
                "source": {"node": "testpatternsource", "port": "video"},
                "target": {"node": "h264encoder", "port": "video"},
            }
        ],
    }


def empty_graph() -> "dict[str, Any]":
    """A graph of no node: what a stream whose function adds nothing compiles to."""
    return {"stream": "main", "nodes": [], "links": []}


def refused_naming(run_outcome: StreamRunWithNoVulkanDriverOutcome) -> str:
    """Why the run refused the graph or failed its load; fails if the graph loaded."""
    assert not run_outcome.loaded, (
        f"the graph loaded; it should have been refused:\n"
        f"{run_outcome.tatolabd_stderr_text_during_the_run[-4000:]}"
    )
    assert run_outcome.failed_or_refused, run_outcome.tatolabd_stderr_text_during_the_run
    assert run_outcome.failed_or_refused_because is not None
    return run_outcome.failed_or_refused_because


def loaded_with(run_outcome: StreamRunWithNoVulkanDriverOutcome) -> int:
    """How many of the stream's nodes a load that succeeded loaded; fails if it was refused."""
    assert run_outcome.loaded, (
        f"the graph was refused: {run_outcome.failed_or_refused_because}\n"
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
                    "config": CONFIG_A_BUILT_IN_CANNOT_LEAVE_OUT.get(built_in_node_class, {}),
                }
                for built_in_node_class in EVERY_BUILT_IN_NODE_CLASS
            ],
        }
    )

    assert loaded_with(run_outcome) == len(EVERY_BUILT_IN_NODE_CLASS)


# ---- loading ----------------------------------------------------------------


def test_a_loaded_graphs_nodes_are_in_the_runtimes_graph(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    graph = pattern_to_encoder_graph()
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(graph)

    assert loaded_with(run_outcome) == 2
    assert run_outcome.loaded_stream_id == stream_id_of_a_hand_written_graph(
        graph, project_directory=run_outcome.tatolab_run.working_directory
    )


def test_the_deepest_config_the_builder_compiles_loads(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        open_config_sink_with_the_deepest_config_the_builder_compiles
    )

    assert loaded_with(run_outcome) == 1


def test_a_config_one_container_deeper_than_a_graph_document_reads_is_refused_by_load(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    """A graph document's parser reads a config deeper than the deepest the builder
    compiles, and one container past its own limit, written by hand, is refused."""
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        {
            "stream": "deep",
            "nodes": [
                {
                    "name": "openconfigsink",
                    "type": OPEN_CONFIG_SINK_TYPE,
                    "config": config_nesting_containers_deep(
                        CONTAINERS_A_GRAPH_DOCUMENTS_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF + 1
                    ),
                }
            ],
        }
    )

    refusal = refused_naming(run_outcome)
    assert "graph document" in refusal
    assert GRAPH_DOCUMENT_NESTED_PAST_THE_MAXIMUM in refusal


@pytest.mark.parametrize("not_a_number", ["NaN", "Infinity"], ids=["nan", "infinity"])
def test_nan_and_infinity_in_a_compiled_config_are_refused_as_not_json(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
    not_a_number: str,
):
    """JSON carries neither, so a graph document holding one is not one; the
    builder refuses both before a document is written."""
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        '{"stream": "nan", "nodes": [{"name": "openconfigsink", "type": "%s", "config": {"value": %s}}]}'
        % (OPEN_CONFIG_SINK_TYPE, not_a_number)
    )

    refusal = refused_naming(run_outcome)
    assert A_GRAPH_DOCUMENT_THAT_DOES_NOT_PARSE in refusal
    assert "expected value" in refusal


def test_a_compiled_graph_that_is_a_list_rather_than_a_graph_is_refused_naming_it(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    refusal = refused_naming(run_stream_on_tatolabd_with_no_vulkan_driver('[{"name": "x", "type": "y"}]'))

    assert "names no stream asset" in refusal


def test_a_lone_surrogate_escape_in_a_compiled_config_is_refused_naming_it(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        '{"stream": "surrogate", "nodes": [{"name": "openconfigsink", "type": "%s", '
        '"config": {"title": "\\udc80"}}]}' % OPEN_CONFIG_SINK_TYPE
    )

    refusal = refused_naming(run_outcome)
    assert A_GRAPH_DOCUMENT_THAT_DOES_NOT_PARSE in refusal
    assert "lone leading surrogate" in refusal


def test_a_config_integer_wider_than_64_bits_in_a_compiled_graph_is_refused_naming_it(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    """The builder refuses one before a document is written; a graph document
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
                {"name": "testpatternsource", "type": TEST_PATTERN_MODULE_TYPE, "config": {}},
                {"name": "loadedframerelay", "type": DESCRIBED_NODE_TYPE, "config": {}},
                {"name": "h264encoder", "type": H264_ENCODER_MODULE_TYPE, "config": {}},
            ],
            "links": [
                {
                    "source": {"node": "testpatternsource", "port": "video"},
                    "target": {"node": "loadedframerelay", "port": "video_from_upstream"},
                },
                {
                    "source": {"node": "loadedframerelay", "port": "video_to_downstream"},
                    "target": {"node": "h264encoder", "port": "video"},
                },
            ],
        }
    )

    assert loaded_with(run_outcome) == 3
    assert DESCRIBED_NODE_MODULE not in sys.modules


def test_the_stream_id_casts_the_stream_name_of_its_graph_document(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    graph = pattern_to_encoder_graph(stream_name="Front Camera")
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(graph)

    assert loaded_with(run_outcome) == 2
    assert run_outcome.loaded_stream_id == stream_id_of_a_hand_written_graph(
        graph, project_directory=run_outcome.tatolab_run.working_directory
    )
    assert run_outcome.loaded_stream_id is not None
    assert "-front-camera-" in run_outcome.loaded_stream_id


@pytest.mark.skipif(sys.platform == "win32", reason="surrogate-escaped paths are POSIX's")
def test_a_project_directory_whose_name_is_not_utf8_is_refused_naming_it(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
    tmp_path: Path,
):
    """`run_project` names its project directory in JSON, which carries UTF-8
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
    graph = empty_graph()
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(graph)
    refusal = refused_naming(run_outcome)
    stream_id = stream_id_of_a_hand_written_graph(
        graph, project_directory=run_outcome.tatolab_run.working_directory
    )

    assert f"the stream `{stream_id}` holds no node" in refusal
    assert "stream_builder.add(" in refusal


def test_a_graph_naming_no_stream_loads_as_the_main_stream_of_its_project(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    """A stream is named by its graph document's file name, so the graph's own `stream`
    names nothing the runtime keys by and a graph may carry none."""
    graph = pattern_to_encoder_graph()
    del graph["stream"]

    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(graph)

    assert loaded_with(run_outcome) == 2
    assert run_outcome.loaded_stream_id == stream_id_of_a_hand_written_graph(
        graph, project_directory=run_outcome.tatolab_run.working_directory
    )


def test_a_stream_name_casting_to_nothing_is_left_out_of_the_stream_id(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    graph = pattern_to_encoder_graph(stream_name="--")

    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(graph)

    assert loaded_with(run_outcome) == 2
    assert run_outcome.loaded_stream_id == stream_id_of_a_hand_written_graph(
        graph, project_directory=run_outcome.tatolab_run.working_directory
    )


def test_a_graph_document_nested_far_past_the_maximum_is_refused_rather_than_crashing(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    nested_far_past_the_maximum = "[" * 100_000 + "]" * 100_000
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        '{"stream": "nested", "nodes": [{"name": "openconfigsink", "type": "%s", '
        '"config": {"nested": %s}}]}' % (OPEN_CONFIG_SINK_TYPE, nested_far_past_the_maximum)
    )

    refusal = refused_naming(run_outcome)
    assert "graph document" in refusal
    assert GRAPH_DOCUMENT_NESTED_PAST_THE_MAXIMUM in refusal


def test_a_setting_a_built_in_does_not_take_is_refused_at_load_naming_the_node_and_the_setting(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    refusal = refused_naming(
        run_stream_on_tatolabd_with_no_vulkan_driver(
            {
                "stream": "pattern",
                "nodes": [
                    {
                        "name": "pattern",
                        "type": _built_in_nodes.ChannelsToRecords.type,
                        "config": {"channel_names": ["level"], "widht": 640},
                    }
                ],
            }
        )
    )

    assert "node `pattern`" in refusal
    assert "`tatolab.stream:ChannelsToRecords`" in refusal
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


# ---- links: a port no node has, and an end naming a stream or a machine -----


def test_a_link_from_a_port_its_node_lacks_is_refused_by_load_naming_the_port(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    refusal = refused_naming(
        run_stream_on_tatolabd_with_no_vulkan_driver(pattern_linked_from_a_port_it_lacks_into_an_encoder)
    )

    assert "`no_such_port`" in refusal


@pytest.mark.parametrize("end", ["source", "target"])
@pytest.mark.parametrize(
    ("keys_naming_a_port_outside_the_stream", "what_the_refusal_names"),
    [
        ({"machine": "studio-display"}, "names the machine `studio-display`"),
        ({"stream": "camera-rig"}, "names the stream `camera-rig`"),
        (
            {"machine": "studio-display", "stream": "camera-rig"},
            "names the machine `studio-display` and the stream `camera-rig`",
        ),
    ],
    ids=["machine", "stream", "machine-and-stream"],
)
def test_a_link_end_naming_a_stream_or_a_machine_is_refused_by_load_naming_the_track_receive(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
    end: str,
    keys_naming_a_port_outside_the_stream: "dict[str, str]",
    what_the_refusal_names: str,
):
    """A link's ends are both in the stream that loads it, so an end naming a
    stream or a machine is refused naming how a stream reads a port outside it."""
    graph = pattern_to_encoder_graph()
    graph["links"][0][end].update(keys_naming_a_port_outside_the_stream)

    refusal = refused_naming(run_stream_on_tatolabd_with_no_vulkan_driver(graph))

    assert f"the {end} end of the link" in refusal, refusal
    assert what_the_refusal_names in refusal, refusal
    assert (
        "a link's ends are both in this stream; read another stream's track at a relay, on "
        "this machine or another, through a `MoqTrackReceive` node"
    ) in refusal, refusal


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
    (stream_run,) = tatolabd.stream_runs
    assert stream_run.queued_action()


# ---- a loaded stream, run and read back by name -----------------------------


@pytest.mark.requires_gpu
def test_a_loaded_streams_nodes_and_link_are_served_by_name_over_its_local_api(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd_running_stream(named_pattern_into_a_named_sink)
    stream_id = tatolabd.stream_runs[-1].await_loaded()["stream_id"]
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(
        stream=stream_id, expected_node_names=["loaded-pattern", "loaded-sink"]
    )
    served_graph = local_api.call_tool("graph", {"stream": stream_id})
    machine_wide_graph = local_api.graph()
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert served_graph["stream"] == stream_id_of_a_one_stream_project(
        tatolabd.stream_runs[-1].working_directory, named_pattern_into_a_named_sink
    )
    assert served_graph["machine"], served_graph
    assert served_graph["machine"] == machine_wide_graph["machine"], machine_wide_graph
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
        "video_from_upstream",
    ) in served_link_ends, served_link_ends

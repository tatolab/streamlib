# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`Runtime.load`, and the `type` every native marker carries.

`Runtime()` boots the engine without starting it and `load` only builds the
graph, so every outcome here but the rig's is read before anything reaches a
device. A graph is a literal dict naming its nodes' types through the markers'
own `type`, or a `@node` class's import path, the shape
`compile_stream_to_graph` returns and `streamlib graph` renders.

A never-run graph is read through its readiness wait, which returns on an empty
graph and otherwise lists every processor's id as `Pending`. Where a load lands
by node name is read on the rig, off a running stream's own control plane.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import subprocess
import sys
import textwrap
import threading
import time
import types
from collections.abc import Callable, Iterator, Sequence
from pathlib import Path
from typing import Any

import pytest

import streamlib
from streamlib import (
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
    Stream,
    TestPatternSource,
    VirtualCameraSink,
    compile_stream_to_graph,
    stream,
)
from streamlib._control_plane_client import (
    ControlPlaneEndpoint,
    ControlPlaneError,
    call_tool,
    resolve_control_plane_endpoint,
)
from streamlib._engine import (
    TestBagCollector,
    TestBagFeeder,
    close_test_harness_channel,
    open_test_harness_channel,
    processor_class_import_paths_in_this_processes_catalog,
)

MEDIA_BUILTIN_MARKER_TYPES: dict[type, str] = {
    TestPatternSource: "streamlib_media_builtins::test_pattern_source::TestPatternSource",
    CameraSource: "streamlib_media_builtins::camera_source::CameraSource",
    DisplayWindow: "streamlib_media_builtins::display_window::DisplayWindow",
    MicrophoneSource: "streamlib_media_builtins::microphone_source::MicrophoneSource",
    SpeakerSink: "streamlib_media_builtins::speaker_sink::SpeakerSink",
    H264Encoder: "streamlib_media_builtins::h264_encoder::H264Encoder",
    H264Decoder: "streamlib_media_builtins::h264_decoder::H264Decoder",
    H265Encoder: "streamlib_media_builtins::h265_encoder::H265Encoder",
    H265Decoder: "streamlib_media_builtins::h265_decoder::H265Decoder",
    OpusEncoder: "streamlib_media_builtins::opus_encoder::OpusEncoder",
    OpusDecoder: "streamlib_media_builtins::opus_decoder::OpusDecoder",
    Mp4Sink: "streamlib_media_builtins::mp4_sink::Mp4Sink",
    VirtualCameraSink: "streamlib_media_builtins::virtual_camera_sink::VirtualCameraSink",
}

TEST_HARNESS_MARKER_TYPES: dict[type, str] = {
    TestBagFeeder: "_engine::python_test_harness_endpoints::TestBagFeeder",
    TestBagCollector: "_engine::python_test_harness_endpoints::TestBagCollector",
}

EVERY_MARKER_TYPE = {**MEDIA_BUILTIN_MARKER_TYPES, **TEST_HARNESS_MARKER_TYPES}

# The native VirtualCameraSink is compiled on Linux only; its marker still
# names the path it registers under there.
MARKERS_THIS_PLATFORM_COMPILES = [
    marker
    for marker in EVERY_MARKER_TYPE
    if marker is not VirtualCameraSink or sys.platform.startswith("linux")
]

RUN_REFUSAL_DEADLINE_SECONDS = 20.0

# Imported by nothing but the engine's type resolver, during `load`.
RESOLVER_IMPORTED_NODE_MODULE = "runtime_load_nodes"
RESOLVER_IMPORTED_NODE_TYPE = f"{RESOLVER_IMPORTED_NODE_MODULE}:LoadedFrameRelay"
RESOLVER_IMPORTED_NODE_SOURCE = Path(__file__).with_name(f"{RESOLVER_IMPORTED_NODE_MODULE}.py")

GRAPH_IS_NOT_JSON_DATA = "the graph is not JSON data: "
PLAIN_JSON_DATA_FIX = (
    "A graph carries what JSON carries — compile_stream_to_graph always emits plain "
    "dicts, lists, str, int, float, bool and None, so build the graph with it."
)
NESTED_PAST_THE_MAXIMUM = "containers nest more than 128 deep"

# The deepest config the builder compiles: 128 containers from the graph's
# root, less the graph, its `nodes` list and the node enclosing the config.
CONTAINERS_A_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF = 125

OWN_PROCESS_DEADLINE_SECONDS = 120.0

EVERY_PROCESSOR_THE_READINESS_WAIT_LISTS_AFTER = "every processor: "

# The control plane is found by this name in the registry, and nothing keeps two
# live runtimes from sharing a name, so the pid keeps this run's row apart from
# any other run on the rig.
SERVED_GRAPH_RUNTIME_NAME = f"runtime-load-served-graph-{os.getpid()}"
SERVED_GRAPH_COLLECTOR_CHANNEL = "runtime-load-served-graph"
SERVED_GRAPH_READY_TIMEOUT_SECONDS = 90.0
SERVED_GRAPH_CONTROL_PLANE_REGISTRATION_DEADLINE_SECONDS = 30.0
SERVED_GRAPH_ENGINE_TEARDOWN_TIMEOUT_SECONDS = 30.0


@pytest.fixture
def runtime() -> Iterator[streamlib.Runtime]:
    """A Runtime built but never run, shut down whatever the test did."""
    built_runtime = streamlib.Runtime()
    try:
        yield built_runtime
    finally:
        built_runtime.shutdown()


def pattern_to_window_graph(*, stream_name: str | None = None) -> dict[str, Any]:
    """A test pattern into a window, exposing the pattern's output."""
    graph: dict[str, Any] = {
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


def config_nesting_containers_deep(containers_counting_the_config: int) -> dict[str, Any]:
    """`{"nested": [[...]]}`, `containers_counting_the_config` containers deep in all."""
    nested: list[Any] = []
    for _ in range(containers_counting_the_config - 2):
        nested = [nested]
    return {"nested": nested}


@stream
def window_with_the_deepest_config_the_builder_compiles(stream: Stream) -> None:
    stream.add(
        DisplayWindow,
        config=config_nesting_containers_deep(CONTAINERS_A_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF),
    )


def empty_graph() -> dict[str, Any]:
    """What a stream whose function adds nothing compiles to."""
    return {"stream": "main", "nodes": [], "links": [], "exposed": []}


def processor_ids_a_never_run_runtimes_readiness_wait_lists(
    runtime: streamlib.Runtime,
) -> list[str]:
    """Every processor id a never-run runtime's readiness wait lists, sorted; none if it returns."""
    try:
        runtime.wait_until_every_processor_is_running(timeout=0.0)
    except RuntimeError as readiness_refusal:
        _, every_processor_listed, listing = str(readiness_refusal).partition(
            EVERY_PROCESSOR_THE_READINESS_WAIT_LISTS_AFTER
        )
        assert every_processor_listed, readiness_refusal
        processor_id_and_state_pairs = [entry.split("=", 1) for entry in listing.split(", ")]
        assert all(
            processor_state == "Pending" for _, processor_state in processor_id_and_state_pairs
        ), readiness_refusal
        return sorted(processor_id for processor_id, _ in processor_id_and_state_pairs)
    return []


def the_runtimes_graph_holds_a_processor(runtime: streamlib.Runtime) -> bool:
    """Whether a never-run runtime's readiness wait lists a processor rather than returning."""
    return bool(processor_ids_a_never_run_runtimes_readiness_wait_lists(runtime))


def pattern_to_window_graph_with_window_scaling(scaling: object) -> dict[str, Any]:
    """The pattern-to-window graph with `scaling` as its window's scaling."""
    graph = pattern_to_window_graph()
    graph["nodes"][1]["config"]["scaling"] = scaling
    return graph


class NodeModuleWhoseImportHoldsTheLoad(importlib.machinery.SourceFileLoader):
    """`runtime_load_nodes.py` as `held_module_name`, its import parking the load until released."""

    def __init__(self, held_module_name: str) -> None:
        super().__init__(held_module_name, str(RESOLVER_IMPORTED_NODE_SOURCE))
        self.load_reached_this_import = threading.Event()
        self.release_the_load = threading.Event()

    def find_spec(
        self,
        fullname: str,
        path: Sequence[str] | None,
        target: types.ModuleType | None = None,
    ) -> importlib.machinery.ModuleSpec | None:
        if fullname != self.name:
            return None
        return importlib.util.spec_from_loader(fullname, self)

    def exec_module(self, module: types.ModuleType) -> None:
        self.load_reached_this_import.set()
        if not self.release_the_load.wait(RUN_REFUSAL_DEADLINE_SECONDS):
            raise TimeoutError("the test never released the held import")
        super().exec_module(module)


@pytest.fixture
def held_node_module(request: pytest.FixtureRequest) -> Iterator[NodeModuleWhoseImportHoldsTheLoad]:
    """A node module only the resolver imports, found first on `sys.meta_path` for the test."""
    held_module_name = f"runtime_load_held_nodes_{request.node.name}"
    node_module = NodeModuleWhoseImportHoldsTheLoad(held_module_name)
    sys.meta_path.insert(0, node_module)
    try:
        yield node_module
    finally:
        node_module.release_the_load.set()
        sys.meta_path.remove(node_module)
        sys.modules.pop(held_module_name, None)


def graph_relaying_through(relay_type: str, *, stream_name: str) -> dict[str, Any]:
    """A test pattern into a window through a node of `relay_type`."""
    return {
        "stream": stream_name,
        "nodes": [
            {"name": "testpatternsource", "type": TestPatternSource.type, "config": {}},
            {"name": "loadedframerelay", "type": relay_type, "config": {}},
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


def load_on_another_thread_holding_at_the_import(
    runtime: streamlib.Runtime,
    graph: dict[str, Any],
    held_node_module: NodeModuleWhoseImportHoldsTheLoad,
    while_the_load_is_held: Callable[[], None],
) -> Exception | None:
    """What a `load` of `graph` raised, `while_the_load_is_held` called while its import parks."""
    held_load_outcome: list[Exception | None] = []

    def load_the_held_graph() -> None:
        try:
            runtime.load(graph)
        except Exception as held_load_refusal:
            held_load_outcome.append(held_load_refusal)
        else:
            held_load_outcome.append(None)

    loading_thread = threading.Thread(target=load_the_held_graph)
    loading_thread.start()
    try:
        assert held_node_module.load_reached_this_import.wait(RUN_REFUSAL_DEADLINE_SECONDS)
        while_the_load_is_held()
    finally:
        held_node_module.release_the_load.set()
        loading_thread.join(RUN_REFUSAL_DEADLINE_SECONDS)
    assert len(held_load_outcome) == 1, "the held load never returned"
    return held_load_outcome[0]


def run_in_its_own_process(script: str) -> None:
    """Run `script` in its own process, so a crash fails the test rather than ending the suite."""
    completed = subprocess.run(
        [sys.executable, "-c", textwrap.dedent(script)],
        capture_output=True,
        text=True,
        timeout=OWN_PROCESS_DEADLINE_SECONDS,
        check=False,
    )
    assert completed.returncode == 0, (
        f"exit status {completed.returncode}\n{completed.stderr[-4000:]}"
    )


def run_expecting_a_refusal(runtime: streamlib.Runtime) -> RuntimeError:
    """`run()`'s refusal, bounded so a run that starts instead fails rather than hangs."""
    shutdown_if_run_started = threading.Timer(RUN_REFUSAL_DEADLINE_SECONDS, runtime.shutdown)
    shutdown_if_run_started.start()
    try:
        with pytest.raises(RuntimeError) as refused:
            runtime.run()
    finally:
        shutdown_if_run_started.cancel()
    return refused.value


# ---- the `type` class attribute ---------------------------------------------


@pytest.mark.parametrize("marker", list(EVERY_MARKER_TYPE), ids=lambda marker: marker.__name__)
def test_every_marker_carries_the_import_path_the_engine_registers_it_under(marker: type):
    assert isinstance(getattr(marker, "type"), str)
    assert getattr(marker, "type") == EVERY_MARKER_TYPE[marker]


@pytest.mark.parametrize(
    "marker", MARKERS_THIS_PLATFORM_COMPILES, ids=lambda marker: marker.__name__
)
def test_every_compiled_marker_type_is_in_this_processes_catalog(marker: type):
    assert getattr(marker, "type") in processor_class_import_paths_in_this_processes_catalog()


def test_a_graph_naming_every_compiled_marker_by_its_type_loads(runtime: streamlib.Runtime):
    runtime.load(
        {
            "nodes": [
                {"name": marker.__name__, "type": getattr(marker, "type"), "config": {}}
                for marker in MARKERS_THIS_PLATFORM_COMPILES
            ]
        }
    )
    assert len(processor_ids_a_never_run_runtimes_readiness_wait_lists(runtime)) == len(
        MARKERS_THIS_PLATFORM_COMPILES
    )


# ---- loading ----------------------------------------------------------------


def test_a_loaded_graphs_nodes_are_in_the_runtimes_graph(runtime: streamlib.Runtime):
    runtime.load(pattern_to_window_graph(stream_name="pattern-to-window"))

    assert len(processor_ids_a_never_run_runtimes_readiness_wait_lists(runtime)) == 2


def test_a_mapping_that_is_not_a_dict_loads(runtime: streamlib.Runtime):
    runtime.load(types.MappingProxyType(pattern_to_window_graph()))

    assert the_runtimes_graph_holds_a_processor(runtime)


def test_a_tuple_nested_in_the_graph_loads_as_a_list(runtime: streamlib.Runtime):
    graph = pattern_to_window_graph()
    graph["nodes"] = tuple(graph["nodes"])

    runtime.load(graph)

    assert the_runtimes_graph_holds_a_processor(runtime)


def test_the_deepest_config_the_builder_compiles_loads(runtime: streamlib.Runtime):
    runtime.load(compile_stream_to_graph(window_with_the_deepest_config_the_builder_compiles))

    assert the_runtimes_graph_holds_a_processor(runtime)


def test_a_config_one_container_deeper_than_the_builder_compiles_is_refused_by_load(
    runtime: streamlib.Runtime,
):
    """The builder's bound is `load`'s: one container past it, written by hand, is refused."""
    with pytest.raises(ValueError) as refused:
        runtime.load(
            {
                "nodes": [
                    {
                        "name": "displaywindow",
                        "type": DisplayWindow.type,
                        "config": config_nesting_containers_deep(
                            CONTAINERS_A_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF + 1
                        ),
                    }
                ]
            }
        )

    assert str(refused.value).startswith(GRAPH_IS_NOT_JSON_DATA)
    assert NESTED_PAST_THE_MAXIMUM in str(refused.value)
    assert not the_runtimes_graph_holds_a_processor(runtime)


@pytest.mark.parametrize("not_a_number", [float("nan"), float("inf")], ids=["nan", "infinity"])
def test_nan_and_infinity_in_a_graphs_config_load(
    runtime: streamlib.Runtime, not_a_number: float
):
    runtime.load(pattern_to_window_graph_with_window_scaling(not_a_number))

    assert the_runtimes_graph_holds_a_processor(runtime)


def test_a_python_node_type_the_process_never_imported_loads_through_the_resolver(
    runtime: streamlib.Runtime,
):
    assert RESOLVER_IMPORTED_NODE_MODULE not in sys.modules, "only the resolver may import it"
    assert RESOLVER_IMPORTED_NODE_TYPE not in processor_class_import_paths_in_this_processes_catalog()

    runtime.load(
        {
            "stream": "relayed",
            "nodes": [
                {"name": "testpatternsource", "type": TestPatternSource.type, "config": {}},
                {"name": "loadedframerelay", "type": RESOLVER_IMPORTED_NODE_TYPE, "config": {}},
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

    assert RESOLVER_IMPORTED_NODE_MODULE in sys.modules
    assert RESOLVER_IMPORTED_NODE_TYPE in processor_class_import_paths_in_this_processes_catalog()
    assert the_runtimes_graph_holds_a_processor(runtime)


def test_the_name_given_to_load_replaces_the_graphs_and_is_cast(runtime: streamlib.Runtime):
    runtime.load(pattern_to_window_graph(stream_name="graph-own-name"), name="Camera Rig")

    with pytest.raises(RuntimeError, match="already loaded the stream `camera-rig`"):
        runtime.load(pattern_to_window_graph())


def test_the_stream_name_a_graph_carries_is_cast(runtime: streamlib.Runtime):
    runtime.load(pattern_to_window_graph(stream_name="Front Camera"))

    with pytest.raises(RuntimeError, match="already loaded the stream `front-camera`"):
        runtime.load(pattern_to_window_graph())


# ---- refusals ---------------------------------------------------------------


def test_an_empty_graph_is_refused_by_name(runtime: streamlib.Runtime):
    with pytest.raises(RuntimeError) as refused:
        runtime.load(empty_graph())

    assert "the stream `main` holds no node" in str(refused.value)
    assert "stream.add(" in str(refused.value)


@pytest.mark.parametrize(
    ("not_a_graph", "type_name"),
    [
        ([{"name": "x", "type": "y"}], "list"),
        ('{"nodes": []}', "str"),
        (pattern_to_window_graph, "function"),
    ],
    ids=["list", "json-text", "function"],
)
def test_a_value_that_is_not_a_mapping_is_refused_naming_it_and_the_fix(
    runtime: streamlib.Runtime, not_a_graph: object, type_name: str
):
    with pytest.raises(TypeError) as refused:
        runtime.load(not_a_graph)  # type: ignore[arg-type]

    assert f"`{type_name}`" in str(refused.value)
    assert "compile_stream_to_graph" in str(refused.value)


def test_a_stream_name_that_is_not_a_str_is_refused_naming_the_fix(runtime: streamlib.Runtime):
    with pytest.raises(TypeError) as refused:
        runtime.load(pattern_to_window_graph(), name=7)  # type: ignore[arg-type]

    assert "`int`" in str(refused.value)
    assert "pass a str, or leave `name` out" in str(refused.value)


def test_a_stream_name_that_cannot_be_encoded_is_refused_naming_it_and_the_fix(
    runtime: streamlib.Runtime,
):
    with pytest.raises(ValueError) as refused:
        runtime.load(pattern_to_window_graph(), name="\udc80")

    assert type(refused.value) is ValueError
    assert "Runtime.load's `name`" in str(refused.value)
    assert "pass a str without lone surrogates" in str(refused.value)
    assert isinstance(refused.value.__cause__, UnicodeEncodeError)
    assert not the_runtimes_graph_holds_a_processor(runtime)


def test_a_stream_name_casting_to_nothing_is_refused_naming_it(runtime: streamlib.Runtime):
    with pytest.raises(ValueError) as refused:
        runtime.load(pattern_to_window_graph(), name="..")

    assert "`..`" in str(refused.value)
    assert "cannot name anything" in str(refused.value)
    assert not the_runtimes_graph_holds_a_processor(runtime)


@pytest.mark.parametrize(
    ("scaling", "refusal_type", "cause_type", "converter_text"),
    [
        ({"fit"}, TypeError, TypeError, "cannot put a set in a bag"),
        (b"fit", TypeError, TypeError, "invalid type: byte array"),
        (object(), TypeError, TypeError, "cannot put a object in a bag"),
        (types.MappingProxyType({"fit": 1}), TypeError, TypeError, "cannot put a mappingproxy"),
        ({1: "fit"}, TypeError, TypeError, "bag keys must be strings"),
        (2**64, ValueError, ValueError, "does not fit in 64 bits"),
        ("\udc80", ValueError, UnicodeEncodeError, "surrogates not allowed"),
    ],
    ids=[
        "set",
        "bytes",
        "object",
        "nested-mapping-not-a-dict",
        "int-key",
        "int-wider-than-64-bits",
        "lone-surrogate",
    ],
)
def test_a_graph_holding_what_json_cannot_carry_is_refused_with_the_converters_text_and_the_fix(
    runtime: streamlib.Runtime,
    scaling: object,
    refusal_type: type[Exception],
    cause_type: type[Exception],
    converter_text: str,
):
    with pytest.raises(refusal_type) as refused:
        runtime.load(pattern_to_window_graph_with_window_scaling(scaling))

    assert type(refused.value) is refusal_type
    assert str(refused.value).startswith(GRAPH_IS_NOT_JSON_DATA)
    assert str(refused.value).endswith(PLAIN_JSON_DATA_FIX)
    assert converter_text in str(refused.value)
    assert type(refused.value.__cause__) is cause_type
    assert str(refused.value.__cause__).rstrip(".") in str(refused.value)
    assert not the_runtimes_graph_holds_a_processor(runtime)


def test_bytes_in_a_graph_are_refused_naming_how_to_carry_bytes(runtime: streamlib.Runtime):
    with pytest.raises(TypeError) as refused_by_load:
        runtime.load(pattern_to_window_graph_with_window_scaling(b"fit"))

    assert str(refused_by_load.value) == (
        f"{GRAPH_IS_NOT_JSON_DATA}the value must survive a JSON round trip: error while "
        f"decoding value: invalid type: byte array, expected any valid JSON value — carry "
        f"`bytes` as a `str` or a list of ints. {PLAIN_JSON_DATA_FIX}"
    )


GRAPH_HOLDING_ITSELF_REFUSED_IN_ITS_OWN_PROCESS = f"""
    import streamlib
    from streamlib import DisplayWindow

    config_holding_itself = {{"title": "load"}}
    config_holding_itself["itself"] = config_holding_itself
    runtime = streamlib.Runtime()
    try:
        runtime.load(
            {{
                "nodes": [
                    {{"name": "displaywindow", "type": DisplayWindow.type,
                      "config": config_holding_itself}},
                ]
            }}
        )
    except ValueError as refusal:
        assert type(refusal) is ValueError, repr(refusal)
        assert str(refusal).startswith({GRAPH_IS_NOT_JSON_DATA!r}), refusal
        assert {NESTED_PAST_THE_MAXIMUM!r} in str(refusal), refusal
        assert "holds itself" in str(refusal), refusal
        assert str(refusal).endswith({PLAIN_JSON_DATA_FIX!r}), refusal
        assert type(refusal.__cause__) is ValueError, repr(refusal.__cause__)
    else:
        raise AssertionError("a graph holding itself loaded")
    finally:
        runtime.shutdown()
"""

GRAPH_NESTED_FAR_PAST_THE_MAXIMUM_REFUSED_IN_ITS_OWN_PROCESS = f"""
    import streamlib
    from streamlib import DisplayWindow

    nested_far_past_the_maximum = []
    for _ in range(100_000):
        nested_far_past_the_maximum = [nested_far_past_the_maximum]
    runtime = streamlib.Runtime()
    try:
        runtime.load(
            {{
                "nodes": [
                    {{"name": "displaywindow", "type": DisplayWindow.type,
                      "config": {{"nested": nested_far_past_the_maximum}}}},
                ]
            }}
        )
    except ValueError as refusal:
        assert str(refusal).startswith({GRAPH_IS_NOT_JSON_DATA!r}), refusal
        assert {NESTED_PAST_THE_MAXIMUM!r} in str(refusal), refusal
    else:
        raise AssertionError("a graph nested 100,000 deep loaded")
    finally:
        runtime.shutdown()
"""


@pytest.mark.parametrize(
    "script",
    [
        GRAPH_HOLDING_ITSELF_REFUSED_IN_ITS_OWN_PROCESS,
        GRAPH_NESTED_FAR_PAST_THE_MAXIMUM_REFUSED_IN_ITS_OWN_PROCESS,
    ],
    ids=["load-graph-holding-itself", "load-graph-nested-100000-deep"],
)
def test_a_container_holding_itself_or_nested_past_the_maximum_is_refused_rather_than_crashing(
    script: str,
):
    run_in_its_own_process(script)


def test_a_graph_that_does_not_parse_is_refused_with_the_engines_text(
    runtime: streamlib.Runtime,
):
    with pytest.raises(RuntimeError, match="the graph does not parse"):
        runtime.load({"nodes": [{"name": "nameless-type"}]})


@pytest.mark.parametrize(
    ("unknown_type", "engine_refusal"),
    [
        ("streamlib_media_builtins::no_such_node::NoSuchNode", "Unknown processor type"),
        ("no_such_module_for_runtime_load:NoSuchNode", "could not register"),
    ],
    ids=["native-path", "python-path"],
)
def test_a_graph_naming_an_unknown_type_is_refused(
    runtime: streamlib.Runtime, unknown_type: str, engine_refusal: str
):
    with pytest.raises(RuntimeError) as refused:
        runtime.load({"nodes": [{"name": "unknown", "type": unknown_type, "config": {}}]})

    assert engine_refusal in str(refused.value)
    assert unknown_type in str(refused.value)


def test_a_second_load_after_a_success_is_refused_naming_the_stream_and_the_fix(
    runtime: streamlib.Runtime,
):
    runtime.load(pattern_to_window_graph(stream_name="pattern-to-window"))
    processor_ids_the_first_load_added = processor_ids_a_never_run_runtimes_readiness_wait_lists(
        runtime
    )

    with pytest.raises(RuntimeError) as refused:
        runtime.load({"nodes": [{"name": "other", "type": TestPatternSource.type, "config": {}}]})

    assert "`pattern-to-window`" in str(refused.value)
    assert "construct another Runtime to load another" in str(refused.value)
    assert len(processor_ids_the_first_load_added) == 2
    assert (
        processor_ids_a_never_run_runtimes_readiness_wait_lists(runtime)
        == processor_ids_the_first_load_added
    )


def test_a_second_load_after_a_refusal_is_refused_naming_that_refusal(
    runtime: streamlib.Runtime,
):
    with pytest.raises(RuntimeError):
        runtime.load(empty_graph())

    with pytest.raises(RuntimeError) as refused:
        runtime.load(pattern_to_window_graph())

    assert "earlier load was refused" in str(refused.value)
    assert "holds no node" in str(refused.value)
    assert not the_runtimes_graph_holds_a_processor(runtime)


def test_load_after_shutdown_is_refused():
    shut_down_runtime = streamlib.Runtime()
    shut_down_runtime.shutdown()

    with pytest.raises(RuntimeError, match="has been shut down"):
        shut_down_runtime.load(pattern_to_window_graph())


# ---- links: a port no node has, and an end naming a runtime -----------------


@stream
def pattern_linked_from_a_port_it_lacks_into_a_window(stream: Stream) -> None:
    pattern = stream.add(TestPatternSource)
    window = stream.add(DisplayWindow)
    stream.connect(pattern.output("no_such_port"), window.input("video"))


def test_a_link_from_a_port_its_node_lacks_is_refused_by_load_naming_the_port(
    runtime: streamlib.Runtime,
):
    with pytest.raises(RuntimeError) as refused:
        runtime.load(compile_stream_to_graph(pattern_linked_from_a_port_it_lacks_into_a_window))

    assert "`no_such_port`" in str(refused.value)


@pytest.mark.parametrize("end", ["source", "target"])
def test_a_link_end_naming_a_runtime_is_refused_by_load_naming_it(
    runtime: streamlib.Runtime, end: str
):
    """Both ends of a link are on the runtime that loads it, so an end naming a
    runtime is refused rather than read as a local end with its runtime dropped."""
    graph = pattern_to_window_graph()
    graph["links"][0][end]["runtime_name"] = "studio-display-9f3c"

    with pytest.raises(RuntimeError, match="studio-display-9f3c"):
        runtime.load(graph)


# ---- run() after a refused load ---------------------------------------------


@pytest.mark.parametrize(
    "refused_load",
    [
        lambda runtime: runtime.load(empty_graph()),
        lambda runtime: runtime.load(["not", "a", "mapping"]),
        lambda runtime: runtime.load(
            {"nodes": [{"name": "unknown", "type": "no_such_module_for_runtime_load:X"}]}
        ),
        lambda runtime: runtime.load(pattern_to_window_graph_with_window_scaling({"fit"})),
        lambda runtime: runtime.load(pattern_to_window_graph(), name=7),
        lambda runtime: runtime.load(pattern_to_window_graph(), name="\udc80"),
    ],
    ids=[
        "empty-graph",
        "not-a-mapping",
        "unknown-type",
        "not-json-data",
        "name-not-a-str",
        "name-cannot-be-encoded",
    ],
)
def test_run_after_a_refused_load_refuses_naming_it_and_never_starts(
    runtime: streamlib.Runtime, refused_load
):
    with pytest.raises((RuntimeError, TypeError, ValueError)) as load_refused:
        refused_load(runtime)

    run_refusal = run_expecting_a_refusal(runtime)

    assert str(load_refused.value) in str(run_refusal)
    assert "construct a new Runtime and load a corrected graph" in str(run_refusal)
    assert processor_ids_a_never_run_runtimes_readiness_wait_lists(runtime) == []


def test_run_after_a_refused_second_load_refuses_too(runtime: streamlib.Runtime):
    runtime.load(pattern_to_window_graph(stream_name="pattern-to-window"))
    with pytest.raises(RuntimeError) as second_load_refused:
        runtime.load(pattern_to_window_graph())

    run_refusal = run_expecting_a_refusal(runtime)

    assert str(second_load_refused.value) in str(run_refusal)


def test_a_load_refused_while_another_is_underway_stands_over_its_success_and_run_names_it(
    runtime: streamlib.Runtime, held_node_module: NodeModuleWhoseImportHoldsTheLoad
):
    refused_while_underway: list[RuntimeError] = []
    run_refused_while_underway: list[RuntimeError] = []

    def refuse_a_load_and_a_run() -> None:
        with pytest.raises(RuntimeError, match="still underway") as refused:
            runtime.load(empty_graph())
        refused_while_underway.append(refused.value)
        run_refused_while_underway.append(run_expecting_a_refusal(runtime))

    held_load_refusal = load_on_another_thread_holding_at_the_import(
        runtime,
        graph_relaying_through(f"{held_node_module.name}:LoadedFrameRelay", stream_name="held"),
        held_node_module,
        refuse_a_load_and_a_run,
    )

    assert "still underway on another thread" in str(run_refused_while_underway[0])
    assert held_load_refusal is None
    assert the_runtimes_graph_holds_a_processor(runtime)
    run_refusal = run_expecting_a_refusal(runtime)
    assert str(refused_while_underway[0]) in str(run_refusal)


def test_a_held_loads_own_refusal_stands_over_a_load_refused_while_it_was_underway(
    runtime: streamlib.Runtime, held_node_module: NodeModuleWhoseImportHoldsTheLoad
):
    def refuse_a_load() -> None:
        with pytest.raises(RuntimeError, match="still underway"):
            runtime.load(empty_graph())

    held_load_refusal = load_on_another_thread_holding_at_the_import(
        runtime,
        graph_relaying_through(f"{held_node_module.name}:NoSuchNode", stream_name="held"),
        held_node_module,
        refuse_a_load,
    )

    assert isinstance(held_load_refusal, RuntimeError)
    assert "could not register" in str(held_load_refusal)
    run_refusal = run_expecting_a_refusal(runtime)
    assert str(held_load_refusal) in str(run_refusal)
    assert "still underway" not in str(run_refusal)


# ---- a loaded stream, run and read back by name -----------------------------


@stream
def named_pattern_into_a_named_collector(stream: Stream) -> None:
    pattern = stream.add(
        TestPatternSource, name="Loaded Pattern", config={"width": 320, "height": 180}
    )
    collector = stream.add(
        TestBagCollector,
        name="Loaded Collector",
        config={"channel": SERVED_GRAPH_COLLECTOR_CHANNEL},
    )
    stream.connect(pattern.output("video"), collector.input("bags_from_upstream"))


def local_api_socket_once_the_registry_lists(runtime_name: str) -> ControlPlaneEndpoint:
    """The local API socket the node registry lists for `runtime_name`, polled until it lists one."""
    deadline = time.monotonic() + SERVED_GRAPH_CONTROL_PLANE_REGISTRATION_DEADLINE_SECONDS
    while True:
        try:
            return resolve_control_plane_endpoint(None, runtime_name)
        except ControlPlaneError:
            if time.monotonic() >= deadline:
                raise
        time.sleep(0.05)


@pytest.mark.requires_gpu
def test_a_loaded_streams_nodes_and_link_are_served_by_name_over_its_control_plane():
    open_test_harness_channel(SERVED_GRAPH_COLLECTOR_CHANNEL)
    runtime = streamlib.Runtime(runtime_name=SERVED_GRAPH_RUNTIME_NAME)
    run_failures: list[BaseException] = []

    def run_until_shut_down() -> None:
        try:
            runtime.run()
        except BaseException as run_failure:  # noqa: BLE001 — asserted on after the join
            run_failures.append(run_failure)

    run_loop = threading.Thread(
        target=run_until_shut_down, name="runtime-load-served-graph", daemon=True
    )
    try:
        runtime.load(compile_stream_to_graph(named_pattern_into_a_named_collector))
        runtime.host_control_plane()
        run_loop.start()
        runtime.wait_until_every_processor_is_running(timeout=SERVED_GRAPH_READY_TIMEOUT_SECONDS)
        served_graph = json.loads(
            call_tool(
                local_api_socket_once_the_registry_lists(SERVED_GRAPH_RUNTIME_NAME), "graph", {}
            )
        )
    finally:
        runtime.shutdown()
        if run_loop.is_alive():
            run_loop.join(SERVED_GRAPH_ENGINE_TEARDOWN_TIMEOUT_SECONDS)
        close_test_harness_channel(SERVED_GRAPH_COLLECTOR_CHANNEL)

    assert not run_loop.is_alive(), (
        f"the engine did not tear down within {SERVED_GRAPH_ENGINE_TEARDOWN_TIMEOUT_SECONDS}s"
    )
    assert run_failures == []
    assert served_graph["stream"] == "named_pattern_into_a_named_collector"
    served_node_names = [served_node["name"] for served_node in served_graph["nodes"]]
    assert "loaded-pattern" in served_node_names, served_node_names
    assert "loaded-collector" in served_node_names, served_node_names
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
        "loaded-collector",
        "bags_from_upstream",
    ) in served_link_ends, served_link_ends

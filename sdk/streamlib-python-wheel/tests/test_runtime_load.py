# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`Runtime.load`, and the `type` every native marker carries — none of it needs a GPU.

`Runtime()` boots the engine without starting it and `load` only builds the
graph, so every outcome here is read before anything reaches a device. A graph
is a literal dict naming its nodes' types through the markers' own `type`, the
shape `compile_stream_to_graph` returns and `streamlib graph` renders.
"""

from __future__ import annotations

import sys
import threading
import types
from collections.abc import Iterator
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
    TestPatternSource,
    VirtualCameraSink,
)
from streamlib._engine import (
    TestBagCollector,
    TestBagFeeder,
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


def empty_graph() -> dict[str, Any]:
    """What a stream whose function adds nothing compiles to."""
    return {"stream": "main", "nodes": [], "links": [], "exposed": []}


def the_node_name_is_taken(runtime: streamlib.Runtime, node_name: str) -> bool:
    """Whether the runtime's graph holds `node_name`, read by refusing a typed add of it."""
    try:
        runtime.add(TestPatternSource, display_name=node_name)
    except RuntimeError as refusal:
        assert f"`{node_name}`" in str(refusal), refusal
        return True
    return False


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
    for marker in MARKERS_THIS_PLATFORM_COMPILES:
        assert the_node_name_is_taken(runtime, marker.__name__.lower())


# ---- loading ----------------------------------------------------------------


def test_a_loaded_graphs_nodes_are_in_the_runtimes_graph(runtime: streamlib.Runtime):
    runtime.load(pattern_to_window_graph(stream_name="pattern-to-window"))

    assert the_node_name_is_taken(runtime, "testpatternsource")
    assert the_node_name_is_taken(runtime, "displaywindow")
    assert runtime.add(TestPatternSource).display_name == "testpatternsource-2"


def test_a_mapping_that_is_not_a_dict_loads(runtime: streamlib.Runtime):
    runtime.load(types.MappingProxyType(pattern_to_window_graph()))

    assert the_node_name_is_taken(runtime, "testpatternsource")


def test_add_and_connect_still_build_beside_a_loaded_graph(runtime: streamlib.Runtime):
    runtime.load(pattern_to_window_graph())

    source = runtime.add(TestPatternSource, display_name="second-pattern")
    window = runtime.add(DisplayWindow, display_name="second-window")
    runtime.connect(source.output("video"), window.input("video"))


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


def test_a_stream_name_that_is_not_a_str_is_refused(runtime: streamlib.Runtime):
    with pytest.raises(TypeError, match="`int`"):
        runtime.load(pattern_to_window_graph(), name=7)  # type: ignore[arg-type]


def test_a_stream_name_casting_to_nothing_is_refused_naming_it(runtime: streamlib.Runtime):
    with pytest.raises(ValueError) as refused:
        runtime.load(pattern_to_window_graph(), name="..")

    assert "`..`" in str(refused.value)
    assert "cannot name anything" in str(refused.value)
    assert not the_node_name_is_taken(runtime, "testpatternsource")


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

    with pytest.raises(RuntimeError) as refused:
        runtime.load({"nodes": [{"name": "other", "type": TestPatternSource.type, "config": {}}]})

    assert "`pattern-to-window`" in str(refused.value)
    assert "construct another Runtime to load another" in str(refused.value)
    assert not the_node_name_is_taken(runtime, "other")


def test_a_second_load_after_a_refusal_is_refused_naming_that_refusal(
    runtime: streamlib.Runtime,
):
    with pytest.raises(RuntimeError):
        runtime.load(empty_graph())

    with pytest.raises(RuntimeError) as refused:
        runtime.load(pattern_to_window_graph())

    assert "earlier load was refused" in str(refused.value)
    assert "holds no node" in str(refused.value)
    assert not the_node_name_is_taken(runtime, "testpatternsource")


def test_load_after_shutdown_is_refused():
    shut_down_runtime = streamlib.Runtime()
    shut_down_runtime.shutdown()

    with pytest.raises(RuntimeError, match="has been shut down"):
        shut_down_runtime.load(pattern_to_window_graph())


# ---- run() after a refused load ---------------------------------------------


@pytest.mark.parametrize(
    "refused_load",
    [
        lambda runtime: runtime.load(empty_graph()),
        lambda runtime: runtime.load(["not", "a", "mapping"]),
        lambda runtime: runtime.load(
            {"nodes": [{"name": "unknown", "type": "no_such_module_for_runtime_load:X"}]}
        ),
    ],
    ids=["empty-graph", "not-a-mapping", "unknown-type"],
)
def test_run_after_a_refused_load_refuses_naming_it_and_never_starts(
    runtime: streamlib.Runtime, refused_load
):
    with pytest.raises((RuntimeError, TypeError)) as load_refused:
        refused_load(runtime)

    run_refusal = run_expecting_a_refusal(runtime)

    assert str(load_refused.value) in str(run_refusal)
    assert "construct a new Runtime and load a corrected graph" in str(run_refusal)
    added_after_the_refused_run = runtime.add(TestPatternSource, display_name="still-building")
    assert added_after_the_refused_run.display_name == "still-building"


def test_run_after_a_refused_second_load_refuses_too(runtime: streamlib.Runtime):
    runtime.load(pattern_to_window_graph(stream_name="pattern-to-window"))
    with pytest.raises(RuntimeError) as second_load_refused:
        runtime.load(pattern_to_window_graph())

    run_refusal = run_expecting_a_refusal(runtime)

    assert str(second_load_refused.value) in str(run_refusal)

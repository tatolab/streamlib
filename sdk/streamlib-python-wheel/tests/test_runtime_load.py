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
from collections.abc import Iterator, Mapping
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

PLAIN_JSON_DATA_FIX = "plain dict, list, str, int, float, bool and None"


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


def pattern_to_window_graph_with_window_scaling(scaling: object) -> dict[str, Any]:
    """The pattern-to-window graph with `scaling` as its window's scaling."""
    graph = pattern_to_window_graph()
    graph["nodes"][1]["config"]["scaling"] = scaling
    return graph


WINDOW_SCALING_LOCATION = 'graph["nodes"][1]["config"]["scaling"]'


class ConfigMappingThatHoldsItsLoadUntilReleased(Mapping[str, Any]):
    """A config mapping whose iteration parks the `load` reading it until released."""

    def __init__(self, entries: dict[str, Any]) -> None:
        self.entries = entries
        self.load_reached_this_config = threading.Event()
        self.release_the_load = threading.Event()

    def __getitem__(self, key: str) -> Any:
        return self.entries[key]

    def __len__(self) -> int:
        return len(self.entries)

    def __iter__(self) -> Iterator[str]:
        self.load_reached_this_config.set()
        if not self.release_the_load.wait(RUN_REFUSAL_DEADLINE_SECONDS):
            raise TimeoutError("the test never released the held load")
        return iter(self.entries)


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


def test_a_mapping_or_tuple_nested_in_the_graph_loads_as_a_dict_or_list(
    runtime: streamlib.Runtime,
):
    graph = pattern_to_window_graph()
    graph["nodes"][1]["config"] = types.MappingProxyType({"title": "load", "scaling": "fit"})
    graph["nodes"] = tuple(graph["nodes"])

    runtime.load(graph)

    assert the_node_name_is_taken(runtime, "displaywindow")


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


def test_a_stream_name_that_is_not_a_str_is_refused_naming_the_fix(runtime: streamlib.Runtime):
    with pytest.raises(TypeError) as refused:
        runtime.load(pattern_to_window_graph(), name=7)  # type: ignore[arg-type]

    assert "`int`" in str(refused.value)
    assert "pass a str, or leave `name` out" in str(refused.value)


def test_a_stream_name_that_cannot_be_encoded_raises_its_encode_error(
    runtime: streamlib.Runtime,
):
    with pytest.raises(UnicodeEncodeError):
        runtime.load(pattern_to_window_graph(), name="\udc80")

    assert not the_node_name_is_taken(runtime, "testpatternsource")


def test_a_stream_name_casting_to_nothing_is_refused_naming_it(runtime: streamlib.Runtime):
    with pytest.raises(ValueError) as refused:
        runtime.load(pattern_to_window_graph(), name="..")

    assert "`..`" in str(refused.value)
    assert "cannot name anything" in str(refused.value)
    assert not the_node_name_is_taken(runtime, "testpatternsource")


@pytest.mark.parametrize(
    ("scaling", "refusal_type", "what_is_named"),
    [
        ({"fit"}, TypeError, "a `set`"),
        (b"fit", TypeError, "a `bytes`"),
        (object(), TypeError, "a `object`"),
        ({1: "fit"}, TypeError, "the key `1`, a `int`"),
        (float("nan"), ValueError, "the float nan"),
        (float("inf"), ValueError, "the float inf"),
        (2**64, ValueError, "wider than 64 bits"),
        ("\udc80", ValueError, "cannot be encoded as UTF-8"),
    ],
    ids=["set", "bytes", "object", "int-key", "nan", "infinity", "int-wider-than-64-bits", "lone-surrogate"],
)
def test_a_graph_holding_what_json_cannot_carry_is_refused_naming_where_and_the_fix(
    runtime: streamlib.Runtime,
    scaling: object,
    refusal_type: type[Exception],
    what_is_named: str,
):
    with pytest.raises(refusal_type) as refused:
        runtime.load(pattern_to_window_graph_with_window_scaling(scaling))

    assert type(refused.value) is refusal_type
    assert str(refused.value).startswith("the graph is not JSON data: ")
    assert f"`{WINDOW_SCALING_LOCATION}`" in str(refused.value)
    assert what_is_named in str(refused.value)
    assert PLAIN_JSON_DATA_FIX in str(refused.value)
    assert not the_node_name_is_taken(runtime, "testpatternsource")


def test_a_stream_name_in_the_graph_that_cannot_be_encoded_is_refused_chaining_its_encode_error(
    runtime: streamlib.Runtime,
):
    graph = pattern_to_window_graph(stream_name="\udc80")

    with pytest.raises(ValueError) as refused:
        runtime.load(graph)

    assert '`graph["stream"]`' in str(refused.value)
    assert PLAIN_JSON_DATA_FIX in str(refused.value)
    assert isinstance(refused.value.__cause__, UnicodeEncodeError)


def test_a_graph_holding_itself_is_refused_naming_where_the_cycle_closes(
    runtime: streamlib.Runtime,
):
    circular_config: dict[str, Any] = {"title": "load"}
    circular_config["itself"] = circular_config
    graph = pattern_to_window_graph()
    graph["nodes"][1]["config"] = circular_config

    with pytest.raises(ValueError) as refused:
        runtime.load(graph)

    assert '`graph["nodes"][1]["config"]["itself"]`' in str(refused.value)
    assert "a cycle JSON cannot carry" in str(refused.value)
    assert PLAIN_JSON_DATA_FIX in str(refused.value)


def test_a_graph_nested_deeper_than_json_text_carries_is_refused(runtime: streamlib.Runtime):
    deeply_nested: list[Any] = []
    for _ in range(100_000):
        deeply_nested = [deeply_nested]
    graph = pattern_to_window_graph()
    graph["nodes"][1]["config"]["nested"] = deeply_nested

    with pytest.raises(ValueError) as refused:
        runtime.load(graph)

    assert "nests deeper than 128 levels" in str(refused.value)
    assert PLAIN_JSON_DATA_FIX in str(refused.value)


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
        lambda runtime: runtime.load(pattern_to_window_graph_with_window_scaling({"fit"})),
        lambda runtime: runtime.load(pattern_to_window_graph(), name=7),
    ],
    ids=["empty-graph", "not-a-mapping", "unknown-type", "not-json-data", "name-not-a-str"],
)
def test_run_after_a_refused_load_refuses_naming_it_and_never_starts(
    runtime: streamlib.Runtime, refused_load
):
    with pytest.raises((RuntimeError, TypeError, ValueError)) as load_refused:
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


def test_a_load_refused_while_another_is_underway_is_recorded_and_run_then_refuses_naming_it(
    runtime: streamlib.Runtime,
):
    held_config = ConfigMappingThatHoldsItsLoadUntilReleased({"title": "load", "scaling": "fit"})
    held_graph = pattern_to_window_graph(stream_name="held")
    held_graph["nodes"][1]["config"] = held_config
    held_load_outcome: list[Exception | None] = []

    def load_the_held_graph() -> None:
        try:
            runtime.load(held_graph)
        except Exception as held_load_refusal:
            held_load_outcome.append(held_load_refusal)
        else:
            held_load_outcome.append(None)

    loading_thread = threading.Thread(target=load_the_held_graph)
    loading_thread.start()
    try:
        assert held_config.load_reached_this_config.wait(RUN_REFUSAL_DEADLINE_SECONDS)
        with pytest.raises(RuntimeError, match="still underway") as refused_while_underway:
            runtime.load(empty_graph())
        run_refused_while_underway = run_expecting_a_refusal(runtime)
    finally:
        held_config.release_the_load.set()
        loading_thread.join(RUN_REFUSAL_DEADLINE_SECONDS)

    assert "still underway on another thread" in str(run_refused_while_underway)
    assert held_load_outcome == [None]
    assert the_node_name_is_taken(runtime, "displaywindow")
    run_refusal = run_expecting_a_refusal(runtime)
    assert str(refused_while_underway.value) in str(run_refusal)

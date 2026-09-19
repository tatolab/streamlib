# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Building a graph — the half of the authoring surface that needs no device.

`Runtime()` boots the engine but does not start it, and `add` / `connect` only
mutate the graph, so nothing here reaches the GPU context that `run()` brings
up. That is what lets these run on every pull request rather than only on the
rig.
"""

from pathlib import Path

import pytest

import streamlib
from streamlib import RuntimeContextLimitedAccess, input, output, processor

GRAPH_BUILDING_APP = Path(__file__).parent / "graph_building_app.py"


@pytest.fixture
def graph_building_app(start_app_under_test):
    """Starts this suite's app; the shared fixture owns the cleanup."""
    return lambda scenario: start_app_under_test(GRAPH_BUILDING_APP, scenario)


@processor
class GraphBuildingFilter:
    @input(delivery_profile="newest")
    def frames_from_upstream(self) -> None: ...

    @output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("frames_from_upstream")
        if frame is not None:
            ctx.outputs.write("frames_to_downstream", frame)


def test_building_the_graph_from_two_threads_does_not_deadlock(graph_building_app):
    """`add` must not hold the lifecycle lock while it releases the GIL.

    The deadly embrace this locks out: a thread inside `add` holds the lifecycle
    mutex and, having detached, waits to re-attach, while another thread holds
    the GIL and blocks on that same mutex.

    Driven out of process because the wedge takes the GIL with it — an in-process
    assertion could never run, and the suite would hang rather than fail.
    Mental-revert: keeping the lifecycle guard alive across the `python.detach`
    in `add`. The app then stops after RUNTIME_CONSTRUCTED and this fails on the
    bounded wait for GRAPH_BUILT, which is how a deadlock should read.
    """
    app = graph_building_app("concurrent_graph_building")
    app.await_marker("GRAPH_BUILT")
    app.await_clean_exit()


def test_shutdown_racing_graph_building_does_not_wedge(graph_building_app):
    """`shutdown()` landing mid-`add` must resolve, either way round.

    `add` releases the GIL around the engine call and `shutdown` takes the same
    lock while holding it, so this is the other order of the same pair. Every
    add is either accepted or refused by name; none may hang.
    """
    app = graph_building_app("shutdown_racing_graph_building")
    app.await_clean_exit()

    refusals = next(
        int(marker.removeprefix("REFUSED_AFTER_SHUTDOWN="))
        for marker in app.markers()
        if marker.startswith("REFUSED_AFTER_SHUTDOWN=")
    )
    assert refusals > 0, (
        f"every add succeeded after shutdown() — the lifecycle state was never "
        f"observed; output:\n{app.output}"
    )


def test_two_added_processors_of_one_class_get_their_own_identities():
    """One registration, two nodes — the graph, not the registry, holds instances."""
    runtime = streamlib.Runtime()
    try:
        first = runtime.add(GraphBuildingFilter, display_name="First")
        second = runtime.add(GraphBuildingFilter, display_name="Second")
        assert first.processor_id != second.processor_id
        assert (first.display_name, second.display_name) == ("First", "Second")
    finally:
        runtime.shutdown()


def test_two_adds_of_one_class_get_distinct_display_names():
    """`rt.add(Blur)` twice must not label both nodes `Blur`.

    The engine is the only place that defaults a name and the only place that
    disambiguates, so the handle reports what it assigned rather than what the
    wheel asked for. Mental-revert: pre-computing the default in the wheel and
    handing the engine a `Some(...)` — the engine then never sees an absent
    name, both nodes come back `GraphBuildingFilter`, and this fails.
    """
    runtime = streamlib.Runtime()
    try:
        first = runtime.add(GraphBuildingFilter)
        second = runtime.add(GraphBuildingFilter)
        third = runtime.add(GraphBuildingFilter)
        assert first.display_name == "GraphBuildingFilter"
        assert second.display_name == "GraphBuildingFilter 2"
        assert third.display_name == "GraphBuildingFilter 3"
    finally:
        runtime.shutdown()


def test_a_duplicate_requested_display_name_is_disambiguated_too():
    """Two `display_name="Front"` calls are as ambiguous as two defaults."""
    runtime = streamlib.Runtime()
    try:
        first = runtime.add(GraphBuildingFilter, display_name="Front")
        second = runtime.add(GraphBuildingFilter, display_name="Front")
        assert (first.display_name, second.display_name) == ("Front", "Front 2")
    finally:
        runtime.shutdown()


def test_a_display_name_that_cannot_be_an_address_chunk_is_refused_naming_the_character():
    """The display name is the processor's part of its mesh address.

    So `add` refuses one that cannot be a single address chunk, naming the
    character rather than quietly re-addressing the processor's ports.
    """
    runtime = streamlib.Runtime()
    try:
        for forbidden in ["/", "*", "$", "#", "?"]:
            with pytest.raises(RuntimeError) as refusal:
                runtime.add(GraphBuildingFilter, display_name=f"front{forbidden}left")
            assert repr(forbidden) in str(refusal.value), (
                f"the refusal must name {forbidden!r}: {refusal.value}"
            )
        with pytest.raises(RuntimeError) as refusal:
            runtime.add(GraphBuildingFilter, display_name="@front")
        assert "@" in str(refusal.value)
    finally:
        runtime.shutdown()


def test_a_display_name_carrying_spaces_or_unicode_is_still_accepted():
    runtime = streamlib.Runtime()
    try:
        assert runtime.add(GraphBuildingFilter, display_name="front left").display_name == (
            "front left"
        )
        assert runtime.add(GraphBuildingFilter, display_name="カメラ").display_name == "カメラ"
    finally:
        runtime.shutdown()


def test_connecting_a_port_that_does_not_exist_is_refused():
    runtime = streamlib.Runtime()
    try:
        source = runtime.add(GraphBuildingFilter)
        destination = runtime.add(GraphBuildingFilter)
        with pytest.raises(RuntimeError):
            runtime.connect(
                source.output("no_such_port"),
                destination.input("frames_from_upstream"),
            )
    finally:
        runtime.shutdown()


def test_adding_something_that_is_not_a_processor_says_so():
    class NotAProcessor:
        pass

    runtime = streamlib.Runtime()
    try:
        with pytest.raises(RuntimeError, match="is not a processor"):
            runtime.add(NotAProcessor)
        # An instance rather than the class is the likely slip, and it gets the
        # same answer.
        with pytest.raises(RuntimeError, match="is not a processor"):
            runtime.add(GraphBuildingFilter())
    finally:
        runtime.shutdown()


def test_the_graph_cannot_be_built_after_the_runtime_is_shut_down():
    runtime = streamlib.Runtime()
    runtime.shutdown()
    with pytest.raises(RuntimeError, match="has been shut down"):
        runtime.add(GraphBuildingFilter)


def test_a_port_on_another_runtime_is_named_by_its_mesh_address():
    """A remote reference carries the address and shows it — processor ids and
    channel names never appear on the mesh, so the middle part is the display
    name."""
    runtime = streamlib.Runtime()
    try:
        source = runtime.remote_processor_output(
            "bench-cam-a1b2", "CameraSource", "video"
        )
        assert isinstance(source, streamlib.RemoteProcessorOutputPortReference)
        assert repr(source) == (
            "RemoteProcessorOutputPortReference(bench-cam-a1b2/CameraSource/video)"
        )
    finally:
        runtime.shutdown()


@pytest.mark.parametrize(
    ("runtime_name", "display_name", "port_name", "offending_part"),
    [
        ("bench/cam", "CameraSource", "video", "runtime name"),
        ("bench-cam", "Camera*Source", "video", "processor display name"),
        ("bench-cam", "CameraSource", "@video", "port name"),
        ("bench-cam", "", "video", "processor display name"),
    ],
)
def test_an_address_the_mesh_cannot_carry_is_refused_where_it_was_written(
    runtime_name: str, display_name: str, port_name: str, offending_part: str
):
    """Refused at the mint rather than at `connect`, so the traceback points at
    the line the author wrote rather than at a wiring call several lines on."""
    runtime = streamlib.Runtime()
    try:
        with pytest.raises(ValueError, match=offending_part):
            runtime.remote_processor_output(runtime_name, display_name, port_name)
    finally:
        runtime.shutdown()


def test_a_remote_source_wires_into_a_local_input_before_run():
    """`connect` takes either end's reference. Nothing about the mesh is waited
    on here — the link is applied now and resolves later, which is what lets a
    graph naming an absent runtime finish building."""
    runtime = streamlib.Runtime()
    try:
        destination = runtime.add(GraphBuildingFilter)
        runtime.connect(
            runtime.remote_processor_output("bench-cam-a1b2", "CameraSource", "video"),
            destination.input("frames_from_upstream"),
        )
    finally:
        runtime.shutdown()


def test_a_remote_source_naming_a_processor_this_runtime_lacks_is_refused_by_name():
    """An address naming this runtime's own name is a local reference, so it
    meets the local refusal — which lists what this runtime does display."""
    runtime = streamlib.Runtime(runtime_name="graph-building-under-test")
    try:
        destination = runtime.add(GraphBuildingFilter, display_name="Destination")
        with pytest.raises(RuntimeError, match="NoSuchProcessor"):
            runtime.connect(
                runtime.remote_processor_output(
                    "graph-building-under-test", "NoSuchProcessor", "video"
                ),
                destination.input("frames_from_upstream"),
            )
    finally:
        runtime.shutdown()


def test_a_source_that_is_neither_reference_names_both_spellings_that_would_work():
    """The refusal is a Python author's to act on, so it names the two calls
    that mint a source rather than the binding's own Rust types."""
    runtime = streamlib.Runtime()
    try:
        destination = runtime.add(GraphBuildingFilter)
        with pytest.raises(TypeError) as refused:
            runtime.connect(
                "camera.video",  # pyright: ignore[reportArgumentType]
                destination.input("frames_from_upstream"),
            )
        assert "processor.output(port_name)" in str(refused.value)
        assert "runtime.remote_processor_output(" in str(refused.value)
    finally:
        runtime.shutdown()


def test_an_input_port_on_another_runtime_is_named_by_its_mesh_address():
    """The destination mirror of the source reference: one address grammar
    serves both ends, so a push is spelled the way a pull is."""
    runtime = streamlib.Runtime()
    try:
        destination = runtime.remote_processor_input(
            "studio-display-9f3c", "DisplayWindow", "video"
        )
        assert isinstance(destination, streamlib.RemoteProcessorInputPortReference)
        assert repr(destination) == (
            "RemoteProcessorInputPortReference(studio-display-9f3c/DisplayWindow/video)"
        )
    finally:
        runtime.shutdown()


@pytest.mark.parametrize(
    ("runtime_name", "display_name", "port_name", "offending_part"),
    [
        ("studio/display", "DisplayWindow", "video", "runtime name"),
        ("studio-display", "Display*Window", "video", "processor display name"),
        ("studio-display", "DisplayWindow", "@video", "port name"),
        ("studio-display", "", "video", "processor display name"),
    ],
)
def test_a_destination_address_the_mesh_cannot_carry_is_refused_where_it_was_written(
    runtime_name: str, display_name: str, port_name: str, offending_part: str
):
    """Refused at the mint, like the source mirror, so the traceback points at
    the line the author wrote."""
    runtime = streamlib.Runtime()
    try:
        with pytest.raises(ValueError, match=offending_part):
            runtime.remote_processor_input(runtime_name, display_name, port_name)
    finally:
        runtime.shutdown()


def test_a_push_into_another_runtime_is_asked_for_and_never_waited_on():
    """The runtime that owns an input applies every link into it, so this only
    asks. Nothing here waits on the mesh — the runtime named is not on one, and
    building the graph still finishes."""
    runtime = streamlib.Runtime(runtime_name="graph-building-pushing")
    try:
        source = runtime.add(GraphBuildingFilter)
        runtime.connect(
            source.output("frames_to_downstream"),
            runtime.remote_processor_input(
                "studio-display-9f3c", "DisplayWindow", "video"
            ),
        )
    finally:
        runtime.shutdown()


def test_a_third_party_wiring_names_neither_end_on_this_runtime():
    """An agent runtime wires two others, and neither end has to be here —
    this runtime adds no processor at all and the call still returns."""
    runtime = streamlib.Runtime(runtime_name="graph-building-agent")
    try:
        runtime.connect(
            runtime.remote_processor_output("bench-cam-a1b2", "CameraSource", "video"),
            runtime.remote_processor_input(
                "studio-display-9f3c", "DisplayWindow", "video"
            ),
        )
    finally:
        runtime.shutdown()


def test_a_destination_naming_this_runtime_takes_the_local_path_and_its_refusals():
    """An address naming this runtime's own name is a local reference, so it
    meets the local refusal — which is also the proof it took that path: a
    destination on another runtime is only asked for, and asking never
    refuses on a display name this runtime cannot see.
    """
    runtime = streamlib.Runtime(runtime_name="graph-building-destination")
    try:
        source = runtime.add(GraphBuildingFilter, display_name="Source")
        with pytest.raises(RuntimeError, match="NoSuchProcessor"):
            runtime.connect(
                source.output("frames_to_downstream"),
                runtime.remote_processor_input(
                    "graph-building-destination", "NoSuchProcessor", "video"
                ),
            )
        # And the same address naming a processor it does hold wires.
        runtime.add(GraphBuildingFilter, display_name="Destination")
        runtime.connect(
            source.output("frames_to_downstream"),
            runtime.remote_processor_input(
                "graph-building-destination", "Destination", "frames_from_upstream"
            ),
        )
    finally:
        runtime.shutdown()


def test_a_destination_that_is_neither_reference_names_both_spellings_that_would_work():
    """The destination mirror of the source refusal: it names the two calls
    that mint one rather than the binding's own Rust types."""
    runtime = streamlib.Runtime()
    try:
        source = runtime.add(GraphBuildingFilter)
        with pytest.raises(TypeError) as refused:
            runtime.connect(
                source.output("frames_to_downstream"),
                "display.video",  # pyright: ignore[reportArgumentType]
            )
        assert "processor.input(port_name)" in str(refused.value)
        assert "runtime.remote_processor_input(" in str(refused.value)
    finally:
        runtime.shutdown()

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
from streamlib import RuntimeContextLimitedAccess, input, node, output

GRAPH_BUILDING_APP = Path(__file__).parent / "graph_building_app.py"


@pytest.fixture
def graph_building_app(start_app_under_test):
    """Starts this suite's app; the shared fixture owns the cleanup."""
    return lambda scenario: start_app_under_test(GRAPH_BUILDING_APP, scenario)


@node
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
        assert (first.display_name, second.display_name) == ("first", "second")
    finally:
        runtime.shutdown()


def test_two_adds_of_one_class_get_distinct_display_names():
    """`rt.add(Blur)` twice must not name both nodes `blur`.

    The engine is the only place that defaults a name and the only place that
    suffixes one, so the handle reports what it assigned rather than what the
    wheel asked for. Mental-revert: pre-computing the default in the wheel and
    handing the engine a `Some(...)` — the engine then refuses the second as a
    typed duplicate, and this fails.
    """
    runtime = streamlib.Runtime()
    try:
        first = runtime.add(GraphBuildingFilter)
        second = runtime.add(GraphBuildingFilter)
        third = runtime.add(GraphBuildingFilter)
        assert first.display_name == "graphbuildingfilter"
        assert second.display_name == "graphbuildingfilter-2"
        assert third.display_name == "graphbuildingfilter-3"
    finally:
        runtime.shutdown()


def test_a_duplicate_requested_display_name_is_refused_by_name():
    """A name the author typed is an address, so a second node whose name
    casts alike is refused naming it rather than suffixed."""
    runtime = streamlib.Runtime()
    try:
        first = runtime.add(GraphBuildingFilter, display_name="FrontCam")
        assert first.display_name == "frontcam"
        with pytest.raises(RuntimeError, match="frontcam"):
            runtime.add(GraphBuildingFilter, display_name="frontcam")
    finally:
        runtime.shutdown()


def test_a_display_name_is_cast_rather_than_refused_for_its_spelling():
    """The display name is the node's part of its mesh address, so it is cast
    to lowercase URL-safe — never refused for a character it carries."""
    runtime = streamlib.Runtime()
    try:
        for typed, cast in [
            ("Front Left", "front-left"),
            ("front/left/two", "front-left-two"),
            ("@front", "front"),
            ("Café", "cafe"),
        ]:
            assert runtime.add(GraphBuildingFilter, display_name=typed).display_name == cast
    finally:
        runtime.shutdown()


def test_a_display_name_that_casts_to_nothing_is_refused_by_name():
    runtime = streamlib.Runtime()
    try:
        for names_nothing in ["カメラ", "..", "///"]:
            with pytest.raises(RuntimeError, match="casts to"):
                runtime.add(GraphBuildingFilter, display_name=names_nothing)
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


def test_a_source_that_is_not_an_output_reference_names_the_spelling_that_would_work():
    """The refusal is a Python author's to act on, so it names the call that
    mints a source rather than the binding's own Rust types."""
    runtime = streamlib.Runtime()
    try:
        destination = runtime.add(GraphBuildingFilter)
        with pytest.raises(TypeError) as refused:
            runtime.connect(
                "camera.video",  # pyright: ignore[reportArgumentType]
                destination.input("frames_from_upstream"),
            )
        assert "processor.output(port_name)" in str(refused.value)
    finally:
        runtime.shutdown()


def test_a_destination_that_is_not_an_input_reference_names_the_spelling_that_would_work():
    """The destination mirror of the source refusal: it names the call that
    mints one rather than the binding's own Rust types."""
    runtime = streamlib.Runtime()
    try:
        source = runtime.add(GraphBuildingFilter)
        with pytest.raises(TypeError) as refused:
            runtime.connect(
                source.output("frames_to_downstream"),
                "display.video",  # pyright: ignore[reportArgumentType]
            )
        assert "processor.input(port_name)" in str(refused.value)
    finally:
        runtime.shutdown()

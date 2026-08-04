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
    @input()
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

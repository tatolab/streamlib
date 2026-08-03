# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Building a graph — the half of the authoring surface that needs no device.

`Runtime()` boots the engine but does not start it, and `add` / `connect` only
mutate the graph, so nothing here reaches the GPU context that `run()` brings
up. That is what lets these run on every pull request rather than only on the
rig.
"""

import queue
import threading

import pytest

import streamlib
from streamlib import LinkInputDataPort, LinkOutputDataPort, processor


@processor
class GraphBuildingFilter:
    frames_from_upstream = LinkInputDataPort()
    frames_to_downstream = LinkOutputDataPort()

    def process(self) -> None:
        frame = self.frames_from_upstream.read()
        if frame is not None:
            self.frames_to_downstream.write(frame)


CONCURRENT_GRAPH_BUILD_ADDS = 200
# A wedged pair never completes, so any finite bound turns the deadlock into a
# failure. Generous enough that a slow runner cannot trip it: 400 adds finish in
# well under a second when nothing is wedged.
CONCURRENT_GRAPH_BUILD_TIMEOUT_SECONDS = 60.0


def test_building_the_graph_from_two_threads_does_not_deadlock():
    """`add` must not hold the lifecycle lock while it releases the GIL.

    The deadly embrace this locks out: a thread inside `add` holds the lifecycle
    mutex and, having detached, waits to re-attach, while another thread holds
    the GIL and blocks on that same mutex.

    Honest about how it fails: the wedge takes the GIL down with it, so this
    does not report a failed assertion — the whole interpreter stops, including
    the join below and anything after it. It goes red as a job that runs out of
    time, which is why the workflow puts a `timeout-minutes` on the job.
    Verified by reverting the fix: killed at the 150s mark having printed
    nothing, versus 0.4s green with the fix in place.
    """
    runtime = streamlib.Runtime()
    failures: "queue.Queue[BaseException]" = queue.Queue()

    def add_repeatedly() -> None:
        try:
            for _ in range(CONCURRENT_GRAPH_BUILD_ADDS):
                runtime.add(GraphBuildingFilter)
        except BaseException as add_failure:  # noqa: BLE001 — surfaced below
            failures.put(add_failure)

    builders = [
        threading.Thread(target=add_repeatedly, name=f"graph-builder-{index}", daemon=True)
        for index in range(2)
    ]
    for builder in builders:
        builder.start()
    for builder in builders:
        builder.join(timeout=CONCURRENT_GRAPH_BUILD_TIMEOUT_SECONDS)

    still_running = [builder.name for builder in builders if builder.is_alive()]
    assert not still_running, (
        f"{still_running} never finished building the graph — `add` deadlocked against "
        f"the lifecycle lock"
    )
    runtime.shutdown()

    try:
        raise failures.get_nowait()
    except queue.Empty:
        pass


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

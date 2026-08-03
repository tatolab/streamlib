# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that build a graph, run as a real `python app.py`.

Driven from outside because the failure being ruled out takes the GIL down with
it: an in-process assertion can never run, so only a parent holding the clock
can turn the wedge into a failure rather than a stuck test run.
"""

import sys
import threading

import streamlib
from streamlib import LinkInputDataPort, LinkOutputDataPort, processor

MARKER_PREFIX = "MARKER:"

# Enough concurrent work to interleave a detach with another thread's lock
# acquisition many times over. The wedge, when present, lands within the first
# few dozen.
ADDS_PER_THREAD = 200
GRAPH_BUILDING_THREADS = 2


@processor
class GraphBuildingFilter:
    frames_from_upstream = LinkInputDataPort()
    frames_to_downstream = LinkOutputDataPort()

    def process(self) -> None:
        frame = self.frames_from_upstream.read()
        if frame is not None:
            self.frames_to_downstream.write(frame)


def marker(name: str) -> None:
    print(f"{MARKER_PREFIX}{name}", flush=True)


def scenario_concurrent_graph_building() -> None:
    """Two threads calling `add` at once must not deadlock against each other.

    The deadly embrace: a thread inside `add` holds the lifecycle mutex and,
    having released the GIL, waits to re-acquire it, while another thread holds
    the GIL and blocks on that same mutex. Neither proceeds, and the interpreter
    stops responding — which is why this scenario reports its own completion
    rather than asserting anything.
    """
    runtime = streamlib.Runtime()
    marker("RUNTIME_CONSTRUCTED")

    def add_repeatedly() -> None:
        for _ in range(ADDS_PER_THREAD):
            runtime.add(GraphBuildingFilter)

    builders = [
        threading.Thread(target=add_repeatedly, name=f"graph-builder-{index}")
        for index in range(GRAPH_BUILDING_THREADS)
    ]
    for builder in builders:
        builder.start()
    for builder in builders:
        builder.join()

    marker("GRAPH_BUILT")
    runtime.shutdown()
    marker("CLEAN_EXIT")


def scenario_shutdown_racing_graph_building() -> None:
    """`shutdown()` from another thread while `add` is in flight.

    `add` releases the GIL around the engine call, and `shutdown` takes the same
    lifecycle lock holding it. Whichever order they land in, the app must reach
    its exit: adds either succeed or are refused with the shut-down error, and
    neither outcome may hang.
    """
    runtime = streamlib.Runtime()
    marker("RUNTIME_CONSTRUCTED")

    refusals = 0
    shutdown_requested = threading.Event()

    def shut_down_once_building() -> None:
        shutdown_requested.wait()
        runtime.shutdown()

    threading.Thread(target=shut_down_once_building, daemon=True).start()

    for index in range(ADDS_PER_THREAD):
        if index == 10:
            shutdown_requested.set()
        try:
            runtime.add(GraphBuildingFilter)
        except RuntimeError:
            refusals += 1

    marker(f"REFUSED_AFTER_SHUTDOWN={refusals}")
    marker("CLEAN_EXIT")


SCENARIOS = {
    "concurrent_graph_building": scenario_concurrent_graph_building,
    "shutdown_racing_graph_building": scenario_shutdown_racing_graph_building,
}


if __name__ == "__main__":
    SCENARIOS[sys.argv[1]]()

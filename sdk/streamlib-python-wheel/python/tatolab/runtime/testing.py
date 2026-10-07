# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Drive one node from a test: feed its inputs, assert on its outputs.

The node under test runs in a real graph on a real engine — in its own
helper process, like every Python node — so what a test exercises is what
production runs: the same construction, the same lifecycle hooks, the same
links. What it does not need is hardware: the frames come from this module's
feeder rather than a camera, and the output lands in a queue rather than a
window.

The graph is a `StreamBuilder` built here and loaded with `Runtime.load`. The
node under test takes its class's default node name, and each port's
endpoint is named for it: `test-bag-feeder-<port>` on an input,
`test-bag-collector-<port>` on an output.

The feeder and the collector are native endpoints, and that is the load-bearing
part. A test asserts from the app process, and a queue this module could reach
is the app's — one process away from any child that tried to read it. Native
endpoints run where the queues are.
"""

from __future__ import annotations

import itertools
import queue
import sys
import threading
from pathlib import Path
from typing import Any, Dict, Mapping, Optional

from tatolab.stream._exposed_name_cast import (
    ExposedNameCastsToNothingError,
    cast_exposed_name_to_url_safe,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)
from tatolab.stream._stream_graph_builder import StreamBuilder

from . import Runtime
from ._engine import (
    TestBagCollector,
    TestBagFeeder,
    await_test_harness_bag,
    close_test_harness_channel,
    feed_test_harness_bag,
    open_test_harness_channel,
)

__all__ = ["SingleNodeTestPipeline"]

SINGLE_NODE_TEST_PIPELINE_STREAM_NAME = "single-node-test-pipeline"

# Long enough that a cold engine's first frame is not mistaken for a failure,
# short enough that a genuinely stalled pipeline fails rather than hangs.
DEFAULT_BAG_TIMEOUT_SECONDS = 30.0

# How long to wait for the engine to finish tearing down before deciding it
# hung. A hang here is the defect; a timeout turns it into a failed test with a
# diagnostic rather than a wedged test run.
ENGINE_TEARDOWN_TIMEOUT_SECONDS = 60.0

# How long `__enter__` waits for every node's helper process to attach.
# Generous because a cold first spawn pays for a child interpreter's startup and
# imports; a graph that has not come up by now is broken, not slow.
GRAPH_READY_TIMEOUT_SECONDS = 60.0

# Channel names are minted here and travel to the endpoints as configuration —
# a queue cannot travel through `config`, but the name of one can.
_next_channel_number = itertools.count()
_channel_lock = threading.Lock()

# Shutdown signals are owned by one run loop per process, so two pipelines
# cannot run at once. Caught here rather than left to the engine's
# "signals are already owned" error, which does not say what to do about it.
_running_pipeline_lock = threading.Lock()


class SingleNodeTestPipeline:
    """One node, with a feeder on every input and a collector on every output.

    `__enter__` returns only once every node is running — which for the
    node under test means its helper process has registered and wired its
    ports — so the first `feed` on the next line cannot be dropped by a link
    whose consumer has not attached.
    """

    def __init__(
        self,
        node_class: type,
        *,
        config: "Optional[Dict[str, Any]]" = None,
    ) -> None:
        self._node_class = node_class
        self._config = config
        self._input_channels: "Dict[str, str]" = {}
        self._output_channels: "Dict[str, str]" = {}
        self._runtime: Optional[Runtime] = None
        self._run_loop: Optional[threading.Thread] = None
        self._run_failure: "queue.Queue[BaseException]" = queue.Queue()

    def __enter__(self) -> "SingleNodeTestPipeline":
        if not _running_pipeline_lock.acquire(blocking=False):
            raise RuntimeError(
                "another SingleNodeTestPipeline is still running in this process: "
                "one engine owns the process's shutdown signals, so pipelines run one at "
                "a time. Close the first `with` block before opening the second."
            )
        try:
            self._build_and_start()
        except BaseException:
            # Nothing is running yet, so tear down what was built and hand the
            # slot back — a pipeline that failed to start must not lock every
            # later test in the process out of the engine.
            self.__exit__()
            raise
        return self

    def _build_and_start(self) -> None:
        stream_builder = StreamBuilder(SINGLE_NODE_TEST_PIPELINE_STREAM_NAME)
        node_under_test = stream_builder.add(self._node_class, config=self._config)

        for port in _declared_port_names(self._node_class, "input"):
            channel = _claim_channel()
            self._input_channels[port] = channel
            feeder = stream_builder.add(
                TestBagFeeder,
                name=f"test-bag-feeder-{port}",
                config={"channel": channel},
            )
            stream_builder.connect(
                feeder.output("bags_to_downstream"), node_under_test.input(port)
            )

        for port in _declared_port_names(self._node_class, "output"):
            channel = _claim_channel()
            self._output_channels[port] = channel
            collector = stream_builder.add(
                TestBagCollector,
                name=f"test-bag-collector-{port}",
                config={"channel": channel},
            )
            stream_builder.connect(
                node_under_test.output(port),
                collector.input("bags_from_upstream"),
            )

        runtime = Runtime()
        self._runtime = runtime
        # The graph is built at run time from the class under test, so there is
        # no module-level `@stream` function for `compile_stream_to_graph` to run.
        runtime.load(
            stream_builder._compiled_graph(),
            project_directory=_directory_the_node_class_is_imported_from(self._node_class),
            interpreter=sys.executable,
        )

        # `run()` blocks, and a test needs to stay in control of the main
        # thread. It is safe here because `__exit__` shuts the engine down and
        # joins this thread before the test returns, so interpreter
        # finalization never races the teardown running on it.
        self._run_loop = threading.Thread(
            target=self._run_until_shut_down, name="streamlib-test-pipeline", daemon=True
        )
        self._run_loop.start()
        self._await_every_node_running(runtime)

    def _await_every_node_running(self, runtime: Runtime) -> None:
        try:
            runtime.wait_until_every_node_is_running(
                timeout=GRAPH_READY_TIMEOUT_SECONDS
            )
        except BaseException:
            # A graph that never came up usually never started: the run loop
            # raised on another thread and left every node where it was.
            # That failure is the cause; this one is the symptom.
            try:
                run_failure = self._run_failure.get_nowait()
            except queue.Empty:
                run_failure = None
            if run_failure is not None:
                raise run_failure from None
            raise

    def _run_until_shut_down(self) -> None:
        try:
            assert self._runtime is not None
            self._runtime.run()
        except BaseException as run_failure:  # noqa: BLE001 — re-raised in __exit__
            self._run_failure.put(run_failure)

    def __exit__(self, *_exception_details: Any) -> bool:
        try:
            if self._runtime is not None:
                self._runtime.shutdown()
            if self._run_loop is not None:
                self._run_loop.join(timeout=ENGINE_TEARDOWN_TIMEOUT_SECONDS)
                if self._run_loop.is_alive():
                    raise AssertionError(
                        f"the engine did not tear down within "
                        f"{ENGINE_TEARDOWN_TIMEOUT_SECONDS}s — a node thread is still "
                        f"running, or teardown is blocked on one"
                    )
            for channel in self._input_channels.values():
                close_test_harness_channel(channel)
            for channel in self._output_channels.values():
                close_test_harness_channel(channel)

            try:
                raise self._run_failure.get_nowait()
            except queue.Empty:
                pass
        finally:
            _running_pipeline_lock.release()
        return False

    def feed(self, port_name: str, bag: "Mapping[str, Any]") -> None:
        """Queue one bag for delivery to the node's `port_name` input.

        A bag is a named map, same as anything a node writes.

        Safe from the first line of the `with` block: `__enter__` already
        waited for the node's helper to attach.
        """
        feed_test_harness_bag(
            self._channel_for(self._input_channels, port_name, "input"), bag
        )

    def await_bag(
        self, port_name: str, *, timeout: float = DEFAULT_BAG_TIMEOUT_SECONDS
    ) -> Any:
        """The next bag the node produced on `port_name`.

        Raises rather than blocking forever: a node that never produces is
        the failure a test is looking for.
        """
        channel = self._channel_for(self._output_channels, port_name, "output")
        bag = await_test_harness_bag(channel, timeout)
        if bag is None:
            raise AssertionError(
                f"{self._node_class.__name__} produced nothing on {port_name!r} "
                f"within {timeout}s"
            )
        return bag

    def await_bags(
        self, port_name: str, count: int, *, timeout: float = DEFAULT_BAG_TIMEOUT_SECONDS
    ) -> "list[Any]":
        """The next `count` bags on `port_name`, in order."""
        return [self.await_bag(port_name, timeout=timeout) for _ in range(count)]

    def _channel_for(
        self, channels: "Dict[str, str]", port_name: str, direction: str
    ) -> str:
        try:
            return channels[cast_exposed_name_to_url_safe(port_name)]
        except (KeyError, ExposedNameCastsToNothingError):
            raise KeyError(
                f"{self._node_class.__name__} declares no {direction} port "
                f"{port_name!r}; it declares {sorted(channels) or 'none'}"
            ) from None


def _directory_the_node_class_is_imported_from(node_class: type) -> Path:
    """The `sys.path` entry the node class's top-level package or module was found
    under — the project directory its processor interpreter imports it from.

    A class whose module has no file falls back to the working directory.
    """
    module = sys.modules.get(node_class.__module__)
    module_file = getattr(module, "__file__", None)
    if module is None or module_file is None:
        return Path.cwd()
    levels_below_the_import_root = node_class.__module__.count(".")
    if getattr(module, "__path__", None) is not None:
        levels_below_the_import_root += 1
    return Path(module_file).resolve().parents[levels_below_the_import_root]


def _declared_port_names(node_class: type, direction: str) -> "list[str]":
    declared = getattr(
        node_class,
        NODE_DECLARATION_INPUT_PORTS_STAMP
        if direction == "input"
        else NODE_DECLARATION_OUTPUT_PORTS_STAMP,
        None,
    )
    if declared is None:
        raise TypeError(
            f"{node_class.__name__} is not a node: decorate it with "
            f"@tatolab.stream.node"
        )
    return [port["name"] for port in declared]


def _claim_channel() -> str:
    """A fresh channel name, opened on the engine side before anything names it."""
    with _channel_lock:
        channel = f"channel-{next(_next_channel_number)}"
    open_test_harness_channel(channel)
    return channel

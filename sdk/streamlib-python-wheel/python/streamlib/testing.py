# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Drive one processor from a test: feed its inputs, assert on its outputs.

The processor under test runs in a real graph on a real engine thread, so what
a test exercises is what production runs — the same construction, the same
lifecycle hooks, the same links. What it does not need is hardware: the frames
come from this module's own feeder processor rather than a camera, and the
output lands in a queue rather than a window.
"""

from __future__ import annotations

import itertools
import queue
import threading
from typing import Any, Dict, Optional

from . import Runtime
from ._processor_declaration import (
    LinkInputDataPort,
    LinkOutputDataPort,
    processor,
)

__all__ = ["SingleProcessorTestPipeline"]

# Long enough that a cold engine's first frame is not mistaken for a failure,
# short enough that a genuinely stalled pipeline fails rather than hangs.
DEFAULT_BAG_TIMEOUT_SECONDS = 30.0

# How long to wait for the engine to finish tearing down before deciding it
# hung. A hang here is the defect; a timeout turns it into a failed test with a
# diagnostic rather than a wedged test run.
ENGINE_TEARDOWN_TIMEOUT_SECONDS = 60.0

# The queues the feeder and collector processors reach. They cannot travel as
# configuration — configuration is JSON on the graph node — so the graph carries
# a channel name and this module holds the queue it names. Safe because the
# processors run in this same interpreter, which is the whole point of
# in-process placement.
_fed_bags: "Dict[str, queue.Queue[Any]]" = {}
_collected_bags: "Dict[str, queue.Queue[Any]]" = {}
_next_channel_number = itertools.count()
_channel_lock = threading.Lock()

# Shutdown signals are owned by one run loop per process, so two pipelines
# cannot run at once. Caught here rather than left to the engine's
# "signals are already owned" error, which does not say what to do about it.
_running_pipeline_lock = threading.Lock()


@processor("@streamlib/testing/TestBagFeeder", execution="continuous", interval_ms=1)
class TestBagFeeder:
    """Publishes whatever a test hands it, in order."""

    bags_to_downstream = LinkOutputDataPort()

    def __init__(self, channel: str) -> None:
        self.channel = channel

    def process(self) -> None:
        try:
            bag = _fed_bags[self.channel].get_nowait()
        except queue.Empty:
            return
        self.bags_to_downstream.write(bag)


@processor("@streamlib/testing/TestBagCollector", execution="reactive")
class TestBagCollector:
    """Collects everything the processor under test produces.

    `every_sample` rather than the default: a test asserts on what was produced,
    so dropping a bag under a burst would make the assertion lie.
    """

    bags_from_upstream = LinkInputDataPort(delivery_profile="every_sample")

    def __init__(self, channel: str) -> None:
        self.channel = channel

    def process(self) -> None:
        bag = self.bags_from_upstream.read()
        if bag is not None:
            _collected_bags[self.channel].put(bag)


class SingleProcessorTestPipeline:
    """One processor, with a feeder on every input and a collector on every output."""

    def __init__(
        self,
        processor_class: type,
        *,
        config: "Optional[Dict[str, Any]]" = None,
    ) -> None:
        self._processor_class = processor_class
        self._config = config
        self._input_channels: "Dict[str, str]" = {}
        self._output_channels: "Dict[str, str]" = {}
        self._runtime: Optional[Runtime] = None
        self._run_loop: Optional[threading.Thread] = None
        self._run_failure: "queue.Queue[BaseException]" = queue.Queue()

    def __enter__(self) -> "SingleProcessorTestPipeline":
        if not _running_pipeline_lock.acquire(blocking=False):
            raise RuntimeError(
                "another SingleProcessorTestPipeline is still running in this process: "
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
        runtime = Runtime()
        self._runtime = runtime
        processor_under_test = runtime.add(self._processor_class, config=self._config)

        for port in _declared_port_names(self._processor_class, "input"):
            channel = _claim_channel(_fed_bags)
            self._input_channels[port] = channel
            feeder = runtime.add(
                TestBagFeeder,
                config={"channel": channel},
                display_name=f"TestBagFeeder({port})",
            )
            runtime.connect(
                feeder.output("bags_to_downstream"), processor_under_test.input(port)
            )

        for port in _declared_port_names(self._processor_class, "output"):
            channel = _claim_channel(_collected_bags)
            self._output_channels[port] = channel
            collector = runtime.add(
                TestBagCollector,
                config={"channel": channel},
                display_name=f"TestBagCollector({port})",
            )
            runtime.connect(
                processor_under_test.output(port),
                collector.input("bags_from_upstream"),
            )

        # `run()` blocks, and a test needs to stay in control of the main
        # thread. It is safe here because `__exit__` shuts the engine down and
        # joins this thread before the test returns, so interpreter
        # finalization never races the teardown running on it.
        self._run_loop = threading.Thread(
            target=self._run_until_shut_down, name="streamlib-test-pipeline", daemon=True
        )
        self._run_loop.start()

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
                        f"{ENGINE_TEARDOWN_TIMEOUT_SECONDS}s — a processor thread is still "
                        f"running, or teardown is blocked on one"
                    )
            for channel in self._input_channels.values():
                _fed_bags.pop(channel, None)
            for channel in self._output_channels.values():
                _collected_bags.pop(channel, None)

            try:
                raise self._run_failure.get_nowait()
            except queue.Empty:
                pass
        finally:
            _running_pipeline_lock.release()
        return False

    def feed(self, port_name: str, bag: Any) -> None:
        """Queue one bag for delivery to the processor's `port_name` input."""
        _fed_bags[self._channel_for(self._input_channels, port_name, "input")].put(bag)

    def await_bag(
        self, port_name: str, *, timeout: float = DEFAULT_BAG_TIMEOUT_SECONDS
    ) -> Any:
        """The next bag the processor produced on `port_name`.

        Raises rather than blocking forever: a processor that never produces is
        the failure a test is looking for.
        """
        channel = self._channel_for(self._output_channels, port_name, "output")
        try:
            return _collected_bags[channel].get(timeout=timeout)
        except queue.Empty:
            raise AssertionError(
                f"{self._processor_class.__name__} produced nothing on {port_name!r} "
                f"within {timeout}s"
            ) from None

    def await_bags(
        self, port_name: str, count: int, *, timeout: float = DEFAULT_BAG_TIMEOUT_SECONDS
    ) -> "list[Any]":
        """The next `count` bags on `port_name`, in order."""
        return [self.await_bag(port_name, timeout=timeout) for _ in range(count)]

    def _channel_for(
        self, channels: "Dict[str, str]", port_name: str, direction: str
    ) -> str:
        try:
            return channels[port_name]
        except KeyError:
            raise KeyError(
                f"{self._processor_class.__name__} declares no {direction} port "
                f"{port_name!r}; it declares {sorted(channels) or 'none'}"
            ) from None


def _declared_port_names(processor_class: type, direction: str) -> "list[str]":
    declared = getattr(
        processor_class, f"__streamlib_processor_{direction}_ports__", None
    )
    if declared is None:
        raise TypeError(
            f"{processor_class.__name__} is not a processor: decorate it with "
            f"@streamlib.processor"
        )
    return [port["name"] for port in declared]


def _claim_channel(channels: "Dict[str, queue.Queue[Any]]") -> str:
    with _channel_lock:
        channel = f"channel-{next(_next_channel_number)}"
    channels[channel] = queue.Queue()
    return channel

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Drive one processor from a test: feed its inputs, assert on its outputs.

The processor under test runs in a real graph on a real engine — in its own
helper process, like every Python processor — so what a test exercises is what
production runs: the same construction, the same lifecycle hooks, the same
links. What it does not need is hardware: the frames come from this module's
feeder rather than a camera, and the output lands in a queue rather than a
window.

The feeder and the collector are native endpoints, and that is the load-bearing
part. A test asserts from the app process, and a queue this module could reach
is the app's — one process away from any child that tried to read it. Native
endpoints run where the queues are.
"""

from __future__ import annotations

import itertools
import queue
import threading
from typing import Any, Dict, Mapping, Optional

from . import Runtime
from ._engine import (
    TestBagCollector,
    TestBagFeeder,
    await_test_harness_bag,
    close_test_harness_channel,
    feed_test_harness_bag,
    open_test_harness_channel,
)

__all__ = ["SingleProcessorTestPipeline"]

# Long enough that a cold engine's first frame is not mistaken for a failure,
# short enough that a genuinely stalled pipeline fails rather than hangs.
DEFAULT_BAG_TIMEOUT_SECONDS = 30.0

# How long to wait for the engine to finish tearing down before deciding it
# hung. A hang here is the defect; a timeout turns it into a failed test with a
# diagnostic rather than a wedged test run.
ENGINE_TEARDOWN_TIMEOUT_SECONDS = 60.0

# Channel names are minted here and travel to the endpoints as configuration —
# a queue cannot travel through `config`, but the name of one can.
_next_channel_number = itertools.count()
_channel_lock = threading.Lock()

# Shutdown signals are owned by one run loop per process, so two pipelines
# cannot run at once. Caught here rather than left to the engine's
# "signals are already owned" error, which does not say what to do about it.
_running_pipeline_lock = threading.Lock()


class SingleProcessorTestPipeline:
    """One processor, with a feeder on every input and a collector on every output.

    **Known gap (#1759): bags fed immediately after `__enter__` can be dropped.**
    A link discards what it publishes before its consumer has attached, and the
    processor under test attaches when its helper process finishes registering —
    tens of milliseconds after the graph compiles. Nothing in this API reports
    that attach yet, so the loss surfaces as `await_bag` reporting that the
    processor produced nothing. Until #1759 lands, feed after the graph has been
    running rather than in the first instants of the `with` block.
    """

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
            channel = _claim_channel()
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
            channel = _claim_channel()
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
        """Queue one bag for delivery to the processor's `port_name` input.

        A bag is a named map, same as anything a processor writes.

        Fed in the first instants after `__enter__`, a bag can be dropped before
        the processor's helper has attached — see this class's docstring and
        #1759.
        """
        feed_test_harness_bag(
            self._channel_for(self._input_channels, port_name, "input"), bag
        )

    def await_bag(
        self, port_name: str, *, timeout: float = DEFAULT_BAG_TIMEOUT_SECONDS
    ) -> Any:
        """The next bag the processor produced on `port_name`.

        Raises rather than blocking forever: a processor that never produces is
        the failure a test is looking for.
        """
        channel = self._channel_for(self._output_channels, port_name, "output")
        bag = await_test_harness_bag(channel, timeout)
        if bag is None:
            raise AssertionError(
                f"{self._processor_class.__name__} produced nothing on {port_name!r} "
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


def _claim_channel() -> str:
    """A fresh channel name, opened on the engine side before anything names it."""
    with _channel_lock:
        channel = f"channel-{next(_next_channel_number)}"
    open_test_harness_channel(channel)
    return channel

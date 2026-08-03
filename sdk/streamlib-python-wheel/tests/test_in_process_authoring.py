# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Python processors running in the app's own interpreter, on real engine threads.

Everything here boots a real engine, so it needs a GPU: `Runner::start()`
initializes a GPU context whose DMA-BUF pool pre-warm a software rasterizer
cannot satisfy. What none of it needs is a camera or a display — the frames are
synthetic, which is the "no hardware" the single-processor harness promises.
"""

import queue
import threading
import time

import pytest

import streamlib
from streamlib import LinkInputDataPort, LinkOutputDataPort, processor
from streamlib.testing import SingleProcessorTestPipeline

pytestmark = pytest.mark.requires_gpu

# Bounded like every other wait in this suite: a pipeline that never produces is
# the failure under test, and an unbounded wait would turn it into a hung run.
PIPELINE_TIMEOUT_SECONDS = 30.0


@processor
class BrightnessFilter:
    """The shape the scaffold teaches: ports as attributes, config as arguments."""

    frames_from_upstream = LinkInputDataPort()
    frames_to_downstream = LinkOutputDataPort()

    def __init__(self, gain: float = 1.0) -> None:
        self.gain = gain

    def process(self) -> None:
        frame = self.frames_from_upstream.read()
        if frame is None:
            return
        self.frames_to_downstream.write({"value": frame["value"] * self.gain})


def test_a_processor_runs_in_process_and_transforms_what_it_is_fed():
    with SingleProcessorTestPipeline(BrightnessFilter, config={"gain": 2.0}) as pipeline:
        pipeline.feed("frames_from_upstream", {"value": 21})
        assert pipeline.await_bag("frames_to_downstream", timeout=PIPELINE_TIMEOUT_SECONDS) == {
            "value": 42.0
        }


def test_config_reaches_the_class_as_ordinary_constructor_arguments():
    """No configuration object: `config={...}` is `BrightnessFilter(**config)`."""
    with SingleProcessorTestPipeline(BrightnessFilter) as pipeline:
        pipeline.feed("frames_from_upstream", {"value": 7})
        # The class's own default, because nothing overrode it.
        assert pipeline.await_bag("frames_to_downstream", timeout=PIPELINE_TIMEOUT_SECONDS) == {
            "value": 7.0
        }


# ---------------------------------------------------------------------------
# The demo: two Python processors passing bags to each other in one process.
# ---------------------------------------------------------------------------

_frames_reaching_the_sink: "queue.Queue[dict]" = queue.Queue()


@processor(execution="continuous", interval_ms=1)
class CountingFrameSource:
    frames_to_downstream = LinkOutputDataPort()

    def __init__(self) -> None:
        self.frames_produced = 0

    def process(self) -> None:
        self.frames_produced += 1
        self.frames_to_downstream.write({"frame_number": self.frames_produced})


@processor
class FrameCountingSink:
    frames_from_upstream = LinkInputDataPort(delivery_profile="every_sample")

    def process(self) -> None:
        frame = self.frames_from_upstream.read()
        if frame is not None:
            _frames_reaching_the_sink.put(frame)


class RunningPipeline:
    """A graph running on a worker thread, shut down and joined on exit.

    `run()` owns the thread it is called on, and a test needs to keep the main
    one. Joining before the block ends is what keeps interpreter finalization
    out of the race the contract is about.
    """

    def __init__(self) -> None:
        self.runtime = streamlib.Runtime()
        self.run_outcome: "queue.Queue[BaseException | None]" = queue.Queue()
        self._run_loop = threading.Thread(target=self._run, daemon=True)

    def _run(self) -> None:
        try:
            self.runtime.run()
        except BaseException as run_failure:  # noqa: BLE001 — surfaced by the caller
            self.run_outcome.put(run_failure)
        else:
            self.run_outcome.put(None)

    def start(self) -> None:
        self._run_loop.start()

    def shut_down_and_take_run_outcome(self) -> "BaseException | None":
        self.runtime.shutdown()
        self._run_loop.join(timeout=PIPELINE_TIMEOUT_SECONDS)
        assert not self._run_loop.is_alive(), "run() never returned after shutdown()"
        return self.run_outcome.get_nowait()


def test_two_python_processors_pass_bags_in_process():
    """The ticket's demo, and the teardown contract re-proved against live processors.

    #1707 could only prove "every engine thread joined, every anchored thread
    state released" against an empty graph, because no Python API could add a
    processor. It now runs with two Python processors holding engine threads:
    a reference surviving teardown makes `Arc::into_inner` return `None`, and
    `run()` raises rather than letting those threads outlive the interpreter.
    """
    pipeline = RunningPipeline()
    source = pipeline.runtime.add(CountingFrameSource)
    sink = pipeline.runtime.add(FrameCountingSink)
    pipeline.runtime.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )
    pipeline.start()

    first_frame = _frames_reaching_the_sink.get(timeout=PIPELINE_TIMEOUT_SECONDS)
    assert first_frame["frame_number"] >= 1

    run_failure = pipeline.shut_down_and_take_run_outcome()
    assert run_failure is None, (
        f"tearing down a graph with live Python processors failed: {run_failure}"
    )


_values_reaching_the_sink: "queue.Queue[dict]" = queue.Queue()


@processor(execution="continuous", interval_ms=1)
class ConstantValueSource:
    frames_to_downstream = LinkOutputDataPort()

    def __init__(self, value: float = 1.0) -> None:
        self.value = value

    def process(self) -> None:
        self.frames_to_downstream.write({"value": self.value})


@processor
class ValueCollectingSink:
    frames_from_upstream = LinkInputDataPort()

    def process(self) -> None:
        frame = self.frames_from_upstream.read()
        if frame is not None:
            _values_reaching_the_sink.put(frame)


def test_two_instances_of_one_class_run_side_by_side_with_their_own_config():
    """One registration, two live instances, two different configurations.

    Both filters are the same class in the same running graph, so a shared
    instance or shared configuration would show up as the wrong product.
    """
    pipeline = RunningPipeline()
    source = pipeline.runtime.add(ConstantValueSource, config={"value": 5.0})
    doubling = pipeline.runtime.add(
        BrightnessFilter, config={"gain": 2.0}, display_name="Doubling"
    )
    tripling = pipeline.runtime.add(
        BrightnessFilter, config={"gain": 3.0}, display_name="Tripling"
    )
    sink = pipeline.runtime.add(ValueCollectingSink)

    pipeline.runtime.connect(
        source.output("frames_to_downstream"), doubling.input("frames_from_upstream")
    )
    pipeline.runtime.connect(
        doubling.output("frames_to_downstream"), tripling.input("frames_from_upstream")
    )
    pipeline.runtime.connect(
        tripling.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )
    pipeline.start()

    try:
        assert _values_reaching_the_sink.get(timeout=PIPELINE_TIMEOUT_SECONDS) == {
            "value": 30.0
        }
    finally:
        pipeline.shut_down_and_take_run_outcome()


# ---------------------------------------------------------------------------
# The GIL-release contract.
# ---------------------------------------------------------------------------

_fast_processor_ticks = threading.Semaphore(0)


@processor(execution="continuous", interval_ms=1)
class FastTickingProcessor:
    def process(self) -> None:
        _fast_processor_ticks.release()


_writes_started = threading.Semaphore(0)
_writes_finished = threading.Semaphore(0)


@processor(execution="continuous", interval_ms=0)
class ProducerBlockedByBackpressure:
    """Writes as fast as it can into a link its consumer barely drains."""

    bags_to_downstream = LinkOutputDataPort()

    def process(self) -> None:
        _writes_started.release()
        self.bags_to_downstream.write({"payload": "x" * 1024})
        _writes_finished.release()


@processor
class GlacialConsumer:
    """`lossless`, so the producer blocks rather than the engine dropping bags."""

    bags_from_upstream = LinkInputDataPort(delivery_profile="lossless")

    def process(self) -> None:
        self.bags_from_upstream.read()
        time.sleep(GLACIAL_CONSUMER_DELAY_SECONDS)


GLACIAL_CONSUMER_DELAY_SECONDS = 1.0
BACKPRESSURE_OBSERVATION_WINDOW_SECONDS = 2.0
# A producer whose writes returned immediately would manage tens of thousands in
# the window; one held by backpressure manages tens. The ceiling sits far below
# the former and far above the latter.
MAXIMUM_WRITES_IF_BACKPRESSURE_HELD = 1000
# A ticker starved by a held GIL manages at most one or two ticks in the window;
# a free one manages thousands.
MINIMUM_TICKS_WHILE_A_WRITE_IS_BLOCKED = 100


def test_a_write_blocked_by_backpressure_stalls_no_other_python_processor():
    """The GIL-release contract on the path that genuinely blocks.

    A `lossless` link makes the producer's `write()` block inside the engine
    rather than drop the bag, so this is a Python processor parked in a native
    call for nearly the whole window. Another Python processor must keep
    running throughout.

    Mental-revert: dropping the `python.detach` around `write_raw` — the blocked
    producer then holds the GIL for the duration of every blocked write and the
    ticker's count collapses to single digits.
    """
    pipeline = RunningPipeline()
    producer = pipeline.runtime.add(ProducerBlockedByBackpressure)
    consumer = pipeline.runtime.add(GlacialConsumer)
    pipeline.runtime.add(FastTickingProcessor)
    pipeline.runtime.connect(
        producer.output("bags_to_downstream"), consumer.input("bags_from_upstream")
    )
    pipeline.start()

    while _fast_processor_ticks.acquire(blocking=False):
        pass
    time.sleep(BACKPRESSURE_OBSERVATION_WINDOW_SECONDS)

    writes_started = _drain(_writes_started)
    writes_finished = _drain(_writes_finished)
    ticks = _drain(_fast_processor_ticks)
    pipeline.shut_down_and_take_run_outcome()

    assert writes_started > writes_finished, (
        "no write was in flight when the window closed — the producer was never "
        "actually blocked, so this proves nothing about a blocked call"
    )
    assert writes_started <= MAXIMUM_WRITES_IF_BACKPRESSURE_HELD, (
        f"the producer completed {writes_started} writes in "
        f"{BACKPRESSURE_OBSERVATION_WINDOW_SECONDS}s — backpressure did not hold it, so "
        f"it spent the window running rather than blocked"
    )
    assert ticks >= MINIMUM_TICKS_WHILE_A_WRITE_IS_BLOCKED, (
        f"a Python processor managed only {ticks} ticks while another sat blocked inside "
        f"write() — the GIL was held across the blocking native call"
    )


def _drain(counter: threading.Semaphore) -> int:
    counted = 0
    while counter.acquire(blocking=False):
        counted += 1
    return counted


# ---------------------------------------------------------------------------
# Failure surfaces.
# ---------------------------------------------------------------------------

_frames_after_the_raising_one: "queue.Queue[dict]" = queue.Queue()


@processor
class ProcessorThatRaisesOnce:
    frames_from_upstream = LinkInputDataPort(delivery_profile="every_sample")

    def __init__(self) -> None:
        self.frames_seen = 0

    def process(self) -> None:
        frame = self.frames_from_upstream.read()
        if frame is None:
            return
        self.frames_seen += 1
        if self.frames_seen == 1:
            raise ValueError("the first frame is always trouble")
        _frames_after_the_raising_one.put(frame)


def test_an_exception_in_process_does_not_take_the_pipeline_down():
    """A raise is one bad frame, not a dead pipeline.

    The traceback reaches the log; the processor is called again on the next
    frame. An author iterating on a filter should not have to restart for a
    typo that only some frames reach.
    """
    with SingleProcessorTestPipeline(ProcessorThatRaisesOnce) as pipeline:
        pipeline.feed("frames_from_upstream", {"value": 1})
        pipeline.feed("frames_from_upstream", {"value": 2})
        assert _frames_after_the_raising_one.get(timeout=PIPELINE_TIMEOUT_SECONDS) == {
            "value": 2
        }

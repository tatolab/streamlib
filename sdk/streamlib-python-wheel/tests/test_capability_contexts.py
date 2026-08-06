# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The capability-typed contexts hooks receive, proven against a running engine.

Full-access hooks (`setup`/`teardown`/`start`/`stop`) get
`RuntimeContextFullAccess`; limited hooks (`process`/`on_pause`/`on_resume`)
get `RuntimeContextLimitedAccess` with no `gpu_full_access` at all. Lease-bound
members die with the hook that received them; `ctx.outputs` survives, which is
what the manual-source pattern is built on.
"""

import queue
import threading
import time
from pathlib import Path

import pytest

import streamlib
from streamlib import (
    GpuContextFullAccess,
    GpuContextLimitedAccess,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    input,
    output,
    processor,
)

# Every processor now runs in its own child process, and this suite drives its
# processors through `SingleProcessorTestPipeline`, whose feeder and collector
# reach module-global queues — the app's globals, which a child cannot see. The
# harness keeps its API and gains a real parent-owned IPC transport as part of
# #1714; until it does, these assert against a placement that no longer exists.
# Strict, so the marker fails loudly the moment the transport lands and has to
# be removed rather than quietly outliving its reason.
pytestmark = [
    pytest.mark.requires_gpu,
    pytest.mark.xfail(
        strict=True,
        reason=(
            "SingleProcessorTestPipeline's module-global queues cannot reach a "
            "helper process; the harness transport is owed by #1714"
        ),
    ),
]

PIPELINE_TIMEOUT_SECONDS = 30.0

# What a hook observed, drained by the test after the graph runs. Module-level
# because configuration is JSON on the graph node — a queue cannot travel
# through `config`.
_hook_observations: "queue.Queue[dict]" = queue.Queue()


def _drain_observations() -> None:
    while True:
        try:
            _hook_observations.get_nowait()
        except queue.Empty:
            return


@pytest.fixture(autouse=True)
def clean_observation_queue():
    _drain_observations()
    yield


class RunningGraph:
    """A graph running on a worker thread, shut down and joined on exit."""

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


def _await_observation(matching: str) -> dict:
    deadline = time.monotonic() + PIPELINE_TIMEOUT_SECONDS
    while True:
        remaining = deadline - time.monotonic()
        assert remaining > 0, f"no {matching!r} observation arrived within the timeout"
        try:
            observation = _hook_observations.get(timeout=min(0.5, remaining))
        except queue.Empty:
            continue
        if observation.get("observed") == matching:
            return observation


# ---------------------------------------------------------------------------
# Which context type reaches which hook.
# ---------------------------------------------------------------------------


@processor(execution="manual")
class SetupContextProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _hook_observations.put(
            {
                "observed": "setup_context",
                "context_type": type(ctx),
                "gpu_full_access_type": type(ctx.gpu_full_access),
                "gpu_limited_access_type": type(ctx.gpu_limited_access),
            }
        )


def test_setup_receives_the_full_access_context_with_gpu_full_access():
    graph = RunningGraph()
    graph.runtime.add(SetupContextProbe)
    graph.start()
    try:
        observation = _await_observation("setup_context")
    finally:
        graph.shut_down_and_take_run_outcome()
    assert observation["context_type"] is RuntimeContextFullAccess
    assert observation["gpu_full_access_type"] is GpuContextFullAccess
    assert observation["gpu_limited_access_type"] is GpuContextLimitedAccess


@processor(execution="continuous", interval_ms=1)
class ProcessContextProbe:
    def __init__(self) -> None:
        self.reported = False

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.reported:
            return
        self.reported = True
        try:
            ctx.gpu_full_access  # noqa: B018 — the attribute error is the point  # pyright: ignore[reportAttributeAccessIssue]
            full_access_outcome = "reachable"
        except AttributeError:
            full_access_outcome = "attribute_error"
        _hook_observations.put(
            {
                "observed": "process_context",
                "context_type": type(ctx),
                "gpu_full_access": full_access_outcome,
                "gpu_limited_access_type": type(ctx.gpu_limited_access),
            }
        )


def test_process_receives_the_limited_context_without_gpu_full_access():
    """The capability split: reaching for the privileged GPU view from
    `process` is an `AttributeError`, not a quietly-working escape hatch."""
    graph = RunningGraph()
    graph.runtime.add(ProcessContextProbe)
    graph.start()
    try:
        observation = _await_observation("process_context")
    finally:
        graph.shut_down_and_take_run_outcome()
    assert observation["context_type"] is RuntimeContextLimitedAccess
    assert observation["gpu_full_access"] == "attribute_error"
    assert observation["gpu_limited_access_type"] is GpuContextLimitedAccess


# ---------------------------------------------------------------------------
# ctx.config and ctx.time.
# ---------------------------------------------------------------------------


@processor(execution="manual")
class ConfigProbe:
    def __init__(self, gain: float = 0.0, label: str = "") -> None:
        self.gain = gain
        self.label = label

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _hook_observations.put({"observed": "config", "config": ctx.config})


def test_ctx_config_is_the_dict_the_processor_was_added_with():
    graph = RunningGraph()
    graph.runtime.add(ConfigProbe, config={"gain": 2.5, "label": "left"})
    graph.start()
    try:
        observation = _await_observation("config")
    finally:
        graph.shut_down_and_take_run_outcome()
    assert observation["config"] == {"gain": 2.5, "label": "left"}


def test_ctx_config_is_an_empty_dict_when_nothing_was_passed():
    graph = RunningGraph()
    graph.runtime.add(ConfigProbe)
    graph.start()
    try:
        observation = _await_observation("config")
    finally:
        graph.shut_down_and_take_run_outcome()
    assert observation["config"] == {}


@processor(execution="manual")
class TimeProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        before = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
        context_time = ctx.time
        after = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
        _hook_observations.put(
            {"observed": "time", "before": before, "context_time": context_time, "after": after}
        )


def test_ctx_time_is_kernel_monotonic_nanoseconds():
    """Two kernel reads bracket `ctx.time`, so the value is provably the raw
    `CLOCK_MONOTONIC` domain — not the engine's media clock."""
    graph = RunningGraph()
    graph.runtime.add(TimeProbe)
    graph.start()
    try:
        observation = _await_observation("time")
    finally:
        graph.shut_down_and_take_run_outcome()
    assert observation["before"] <= observation["context_time"] <= observation["after"]


# ---------------------------------------------------------------------------
# Write timestamps, explicit and default.
# ---------------------------------------------------------------------------

EXPLICIT_TIMESTAMP_NS = 123_456_789_000


@processor(execution="continuous", interval_ms=1)
class ExplicitlyStampedSource:
    @output()
    def bags_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.outputs.write(
            "bags_to_downstream", {"value": 1}, timestamp_ns=EXPLICIT_TIMESTAMP_NS
        )


@processor(execution="continuous", interval_ms=1)
class DefaultStampedSource:
    @output()
    def bags_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.outputs.write("bags_to_downstream", {"value": 1})


@processor
class TimestampCollectingSink:
    @input(delivery_profile="every_sample")
    def bags_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag, timestamp_ns = ctx.inputs.read_with_timestamp("bags_from_upstream")
        if bag is not None:
            _hook_observations.put(
                {"observed": "stamped_bag", "bag": bag, "timestamp_ns": timestamp_ns}
            )


def _run_stamped_graph(source_class: type) -> dict:
    graph = RunningGraph()
    source = graph.runtime.add(source_class)
    sink = graph.runtime.add(TimestampCollectingSink)
    graph.runtime.connect(
        source.output("bags_to_downstream"), sink.input("bags_from_upstream")
    )
    graph.start()
    try:
        return _await_observation("stamped_bag")
    finally:
        graph.shut_down_and_take_run_outcome()


def test_an_explicit_write_timestamp_reaches_the_reader_unchanged():
    observation = _run_stamped_graph(ExplicitlyStampedSource)
    assert observation["timestamp_ns"] == EXPLICIT_TIMESTAMP_NS


def test_a_default_write_timestamp_is_kernel_monotonic():
    before_run = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
    observation = _run_stamped_graph(DefaultStampedSource)
    after_run = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
    assert before_run <= observation["timestamp_ns"] <= after_run


# ---------------------------------------------------------------------------
# Lease expiry and what deliberately survives it.
# ---------------------------------------------------------------------------

_stashed_contexts: "queue.Queue[RuntimeContextFullAccess]" = queue.Queue()


@processor(execution="manual")
class ContextStasher:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _stashed_contexts.put(ctx)
        _hook_observations.put({"observed": "context_stashed"})


def test_a_context_stashed_from_setup_expires_with_the_hook():
    """Lease-bound members are only usable inside the hook that received the
    context — a stashed context answers later calls with the lease error."""
    graph = RunningGraph()
    graph.runtime.add(ContextStasher)
    graph.start()
    try:
        _await_observation("context_stashed")
    finally:
        graph.shut_down_and_take_run_outcome()

    stashed_context = _stashed_contexts.get_nowait()
    with pytest.raises(RuntimeError, match="only valid during the lifecycle hook or escalate"):
        stashed_context.is_paused()


_manual_source_stop = threading.Event()


@processor(execution="manual")
class WorkerThreadSource:
    """The manual-source pattern: `ctx.outputs` captured in setup, written from
    a thread the processor owns."""

    def __init__(self) -> None:
        self._worker: "threading.Thread | None" = None

    @output()
    def bags_to_downstream(self) -> None: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        captured_outputs = ctx.outputs

        def produce_from_worker_thread() -> None:
            while not _manual_source_stop.is_set():
                captured_outputs.write("bags_to_downstream", {"origin": "worker-thread"})
                _manual_source_stop.wait(0.01)

        self._worker = threading.Thread(target=produce_from_worker_thread, daemon=True)
        self._worker.start()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        _manual_source_stop.set()
        if self._worker is not None:
            self._worker.join(timeout=5.0)


@processor
class WorkerThreadBagSink:
    @input(delivery_profile="every_sample")
    def bags_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("bags_from_upstream")
        if bag is not None:
            _hook_observations.put({"observed": "worker_thread_bag", "bag": bag})


def test_outputs_captured_in_setup_still_write_from_a_worker_thread():
    """`ctx.outputs` is deliberately not lease-bound: a manual source hands it
    to its own thread and keeps producing between hooks."""
    _manual_source_stop.clear()
    graph = RunningGraph()
    source = graph.runtime.add(WorkerThreadSource)
    sink = graph.runtime.add(WorkerThreadBagSink)
    graph.runtime.connect(
        source.output("bags_to_downstream"), sink.input("bags_from_upstream")
    )
    graph.start()
    try:
        observation = _await_observation("worker_thread_bag")
    finally:
        graph.shut_down_and_take_run_outcome()
        _manual_source_stop.set()
    assert observation["bag"] == {"origin": "worker-thread"}


# ---------------------------------------------------------------------------
# Hook arity: the ctx parameter is required.
# ---------------------------------------------------------------------------


ZERO_ARGUMENT_PROCESS_APP = Path(__file__).parent / "zero_argument_process_app.py"


def test_a_zero_argument_process_hook_fails_loudly_with_a_type_error(
    start_app_under_test,
):
    """Hooks require the ctx parameter; a zero-arg `process` must TypeError by
    name in the log, never be silently invoked without a context.

    Driven out of process because the failure is a log line: the engine's
    tracing writer binds stdout at the process's first engine boot, so only a
    parent reading the pipe observes it no matter which test booted an engine
    first.
    """
    app = start_app_under_test(ZERO_ARGUMENT_PROCESS_APP)
    app.await_output_containing("raised in process()", "the hook named in the failure")
    app.await_output_containing("TypeError", "the zero-arg TypeError")
    app.interrupt()
    app.await_clean_exit()
    assert "HOOK_BODY_RAN" not in app.markers(), (
        "the zero-arg hook body ran — the host invoked it without a context"
    )


# ---------------------------------------------------------------------------
# GPU surfaces from a hook.
# ---------------------------------------------------------------------------


@processor(execution="manual")
class PixelBufferAcquirer:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        with ctx.gpu_limited_access.acquire_pixel_buffer(64, 32) as surface_handle:
            surface_handle.lock()
            pixel_access_shape = surface_handle.as_numpy().shape
            surface_handle.unlock()
            _hook_observations.put(
                {
                    "observed": "pixel_buffer",
                    "surface_id": surface_handle.surface_id,
                    "width": surface_handle.width,
                    "height": surface_handle.height,
                    "pixel_access_shape": pixel_access_shape,
                }
            )


def test_a_hook_acquires_a_pixel_buffer_and_reaches_its_pixels():
    graph = RunningGraph()
    graph.runtime.add(PixelBufferAcquirer)
    graph.start()
    try:
        observation = _await_observation("pixel_buffer")
    finally:
        graph.shut_down_and_take_run_outcome()
    assert isinstance(observation["surface_id"], str) and observation["surface_id"]
    assert (observation["width"], observation["height"]) == (64, 32)
    # The exchange surface itself is covered by `test_pixel_exchange.py`;
    # here it only has to be reachable from a hook.
    assert observation["pixel_access_shape"] == (32, 64, 4)


@processor(execution="manual")
class RepeatedPixelBufferAcquirer:
    """Acquires and closes the same shape more times than the pool has slots."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        outcomes = []
        for _ in range(8):
            try:
                with ctx.gpu_limited_access.acquire_pixel_buffer(96, 96) as surface_handle:
                    outcomes.append("ok" if surface_handle.surface_id else "no-id")
            except RuntimeError as acquire_failure:
                outcomes.append(f"failed: {acquire_failure}")
        _hook_observations.put({"observed": "repeated_acquires", "outcomes": outcomes})


def test_a_closed_pixel_buffer_returns_its_pool_slot():
    """Acquire and close beyond the pool depth — every acquire must succeed.

    Regression lock on a real leak: the acquire's surface-store check-in parks
    a strong clone of the buffer in the store, and the pool frees a slot only
    once the strong count returns to 1. Before the fix, `close()` dropped only
    the handle's own clone, so the fifth acquire of any shape failed with
    "All pixel buffers are currently in use" — a per-tick acquirer was dead
    after one pool depth's worth of frames.

    Mental-revert: dropping the `store.release(surface_id)` from the handle's
    `release_owned_engine_value`, or the cache eviction from
    `SurfaceStore::release` — either brings the exhaustion back.
    """
    graph = RunningGraph()
    graph.runtime.add(RepeatedPixelBufferAcquirer)
    graph.start()
    try:
        observation = _await_observation("repeated_acquires")
    finally:
        graph.shut_down_and_take_run_outcome()
    assert observation["outcomes"] == ["ok"] * 8, (
        f"a closed pixel buffer did not return its pool slot: {observation['outcomes']}"
    )


@processor(execution="manual")
class WorkerThreadEscalator:
    """The native camera's shape: stash Limited in setup, escalate once on a worker.

    `camera_linux.rs` is the reference — its capture thread holds only
    `GpuContextLimitedAccess` and upgrades exactly once at thread start for
    privileged construction. This proves a Python processor can follow the
    same pattern rather than reaching past the type system the way
    `avatar_character.py`'s `# type: ignore[attr-defined]` had to.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self._stashed_gpu_limited = ctx.gpu_limited_access

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        self._worker = threading.Thread(target=self._construct_resources, daemon=True)
        self._worker.start()

    def _construct_resources(self) -> None:
        stashed_escalated_capability = []

        def privileged_construction(full):
            with full.acquire_pixel_buffer(48, 48) as surface_handle:
                stashed_escalated_capability.append(full)
                return {"surface_id": surface_handle.surface_id}

        try:
            construction_outcome = self._stashed_gpu_limited.escalate(privileged_construction)
            # The escalated capability expired with the callback; using it
            # afterwards must raise, not silently keep privileged access.
            try:
                stashed_escalated_capability[0].wait_device_idle()
                escape_outcome = "granted"
            except RuntimeError as expiry_refusal:
                escape_outcome = str(expiry_refusal)
            _hook_observations.put(
                {
                    "observed": "worker_escalate",
                    "construction_outcome": construction_outcome,
                    "escape_outcome": escape_outcome,
                }
            )
        except BaseException as escalate_failure:  # noqa: BLE001 — surfaced by the test
            _hook_observations.put(
                {"observed": "worker_escalate", "failure": repr(escalate_failure)}
            )

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        self._worker.join(timeout=30.0)


def test_a_worker_thread_escalates_once_like_the_native_camera():
    graph = RunningGraph()
    graph.runtime.add(WorkerThreadEscalator)
    graph.start()
    try:
        observation = _await_observation("worker_escalate")
    finally:
        graph.shut_down_and_take_run_outcome()
    assert "failure" not in observation, (
        f"the camera pattern failed from Python: {observation.get('failure')}"
    )
    assert observation["construction_outcome"]["surface_id"], (
        "escalate did not hand back the callback's return value"
    )
    assert "only valid during" in observation["escape_outcome"], (
        f"an escalated capability stashed past its callback must expire, got: "
        f"{observation['escape_outcome']!r}"
    )

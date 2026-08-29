# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes for the capability-typed contexts, run from where hooks really run.

Full-access hooks (`setup` / `teardown` / `start` / `stop`) get
`RuntimeContextFullAccess`; limited hooks (`process` / `on_pause` / `on_resume`)
get `RuntimeContextLimitedAccess` with no `gpu_full_access` at all.

Each probe runs in its own helper process and reports one
`MARKER:PROBE_RESULT` JSON line over the child→parent log forwarding. Types
travel as their names: the observation crosses a process boundary, so nothing
in it can be a live object.
"""

import json
import os
import threading
import time
import traceback

from streamlib import (
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    input,
    log,
    output,
    processor,
)

RESULT_MARKER = "MARKER:PROBE_RESULT "

EXPLICIT_TIMESTAMP_NS = 123_456_789_000

SURFACE_WIDTH = 64
SURFACE_HEIGHT = 32


def _report(probe_body) -> None:
    """One result line per probe, success or failure — the failure carries the
    traceback so the test fails on the cause rather than a missing marker."""
    try:
        observation = probe_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(RESULT_MARKER + json.dumps({"pid": os.getpid(), **observation}))


# ---------------------------------------------------------------------------
# Which context type reaches which hook
# ---------------------------------------------------------------------------


@processor(execution="manual")
class SetupContextProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report(
            lambda: {
                "context_type": type(ctx).__name__,
                "gpu_full_access_type": type(ctx.gpu_full_access).__name__,
                "gpu_limited_access_type": type(ctx.gpu_limited_access).__name__,
            }
        )


@processor(execution="continuous", interval_ms=1)
class ProcessContextProbe:
    def __init__(self) -> None:
        self.reported = False

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.reported:
            return
        self.reported = True

        def observe() -> dict:
            try:
                ctx.gpu_full_access  # noqa: B018 — the attribute error is the point  # pyright: ignore[reportAttributeAccessIssue]
                full_access_outcome = "reachable"
            except AttributeError:
                full_access_outcome = "attribute_error"
            return {
                "context_type": type(ctx).__name__,
                "gpu_full_access": full_access_outcome,
                "gpu_limited_access_type": type(ctx.gpu_limited_access).__name__,
            }

        _report(observe)


# ---------------------------------------------------------------------------
# ctx.config and ctx.time
# ---------------------------------------------------------------------------


@processor(execution="manual")
class ConfigProbe:
    def __init__(self, gain: float = 0.0, label: str = "") -> None:
        self.gain = gain
        self.label = label

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report(lambda: {"config": ctx.config})


@processor(execution="manual")
class TimeProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        def observe() -> dict:
            before = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
            context_time = ctx.time
            after = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
            return {"before": before, "context_time": context_time, "after": after}

        _report(observe)


# ---------------------------------------------------------------------------
# Write timestamps, explicit and default
# ---------------------------------------------------------------------------


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
    @input(delivery_profile="ordered")
    def bags_from_upstream(self) -> None: ...

    def __init__(self) -> None:
        self.reported = False

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.reported:
            return
        bag, timestamp_ns = ctx.inputs.read_with_timestamp("bags_from_upstream")
        if bag is not None:
            self.reported = True
            _report(lambda: {"bag": bag, "timestamp_ns": timestamp_ns})


# ---------------------------------------------------------------------------
# What a context stashed past its hook still answers
# ---------------------------------------------------------------------------


@processor(execution="manual")
class ContextStasher:
    """Keeps the setup context and reads it again from `start`."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self._stashed_context = ctx

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        def observe() -> dict:
            return {
                "stashed_is_paused": self._stashed_context.is_paused(),
                "stashed_config": self._stashed_context.config,
                "stashed_processor_id": self._stashed_context.processor_id,
            }

        _report(observe)


# ---------------------------------------------------------------------------
# The manual-source pattern
# ---------------------------------------------------------------------------


@processor(execution="manual")
class WorkerThreadSource:
    """`ctx.outputs` captured in setup, written from a thread the processor owns."""

    def __init__(self) -> None:
        self._stop = threading.Event()
        self._worker: "threading.Thread | None" = None

    @output()
    def bags_to_downstream(self) -> None: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        captured_outputs = ctx.outputs

        def produce_from_worker_thread() -> None:
            while not self._stop.is_set():
                captured_outputs.write("bags_to_downstream", {"origin": "worker-thread"})
                self._stop.wait(0.01)

        self._worker = threading.Thread(target=produce_from_worker_thread, daemon=True)
        self._worker.start()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        self._stop.set()
        if self._worker is not None:
            self._worker.join(timeout=5.0)


@processor
class WorkerThreadBagSink:
    @input(delivery_profile="ordered")
    def bags_from_upstream(self) -> None: ...

    def __init__(self) -> None:
        self.reported = False

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.reported:
            return
        bag = ctx.inputs.read("bags_from_upstream")
        if bag is not None:
            self.reported = True
            _report(lambda: {"bag": bag})


# ---------------------------------------------------------------------------
# GPU surfaces from a hook
# ---------------------------------------------------------------------------


@processor(execution="manual")
class PixelBufferAcquirer:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        def observe() -> dict:
            with ctx.gpu_limited_access.acquire_pixel_buffer(
                SURFACE_WIDTH, SURFACE_HEIGHT
            ) as surface_handle:
                surface_handle.lock()
                pixel_access_shape = list(surface_handle.as_numpy().shape)
                surface_handle.unlock()
                return {
                    "surface_id": surface_handle.surface_id,
                    "width": surface_handle.width,
                    "height": surface_handle.height,
                    "pixel_access_shape": pixel_access_shape,
                }

        _report(observe)


@processor(execution="manual")
class RepeatedPixelBufferAcquirer:
    """Acquires and closes the same shape more times than the pool has slots."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        def observe() -> dict:
            outcomes = []
            for _ in range(8):
                try:
                    with ctx.gpu_limited_access.acquire_pixel_buffer(
                        96, 96
                    ) as surface_handle:
                        outcomes.append("ok" if surface_handle.surface_id else "no-id")
                except RuntimeError as acquire_failure:
                    outcomes.append(f"failed: {acquire_failure}")
            return {"outcomes": outcomes}

        _report(observe)


@processor(execution="manual")
class WorkerThreadPrivilegedConstructor:
    """The native camera's shape: stash the capabilities in setup, construct
    privileged resources from a thread the processor owns.

    `camera_linux.rs` is the reference — its capture thread holds only
    `GpuContextLimitedAccess` and reaches for privileged construction once at
    thread start. A Python processor follows the same pattern; what differs is
    that each privileged call is its own round trip to the parent rather than
    one callback holding an escalate scope, which cannot cross a process.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self._stashed_gpu_limited = ctx.gpu_limited_access
        self._stashed_gpu_full = ctx.gpu_full_access

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        self._worker = threading.Thread(target=self._construct_resources, daemon=True)
        self._worker.start()

    def _construct_resources(self) -> None:
        def observe() -> dict:
            observation = {}
            with self._stashed_gpu_full.acquire_pixel_buffer(48, 48) as surface_handle:
                observation["privileged_surface_id"] = surface_handle.surface_id
            self._stashed_gpu_full.wait_device_idle()
            observation["waited_for_device_idle"] = True
            with self._stashed_gpu_limited.acquire_pixel_buffer(48, 48) as from_limited:
                observation["limited_surface_id"] = from_limited.surface_id
            try:
                self._stashed_gpu_limited.escalate(lambda full: None)
            except RuntimeError as refusal:
                observation["escalate_refusal"] = str(refusal)
            return observation

        _report(observe)

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        self._worker.join(timeout=30.0)

# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Probes for the capability-typed contexts, run from where hooks really run.

Full-access hooks (`setup` / `teardown` / `start` / `stop`) get
`RuntimeContextFullAccess`; limited hooks (`process` / `on_pause` / `on_resume`)
get `RuntimeContextLimitedAccess` with no `gpu_full_access` at all.

Each probe runs in its own helper process and reports one
`MARKER:PROBE_RESULT` JSON line over the child→parent log forwarding. Types
travel as their names: the observation crosses a process boundary, so nothing
in it can be a live object.
"""

from typing import Callable
import json
import os
import threading
import traceback

from engine_media_clock import engine_media_clock_now_ns
from json_messages import JsonMessagesInOrder, json_bytes, json_read
from tatolab.stream import (
    EncodedVideo,
    GpuContextFullAccess,
    GpuContextLimitedAccess,
    InputPin,
    OutputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    computed,
    log,
    node,
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


@node(execution="manual")
def SetupContextProbe(
    *,
    ctx: RuntimeContextFullAccess,
) -> None:
    _report(
        lambda: {
            "context_type": type(ctx).__name__,
            "gpu_full_access_type": type(ctx.gpu_full_access).__name__,
            "gpu_limited_access_type": type(ctx.gpu_limited_access).__name__,
        }
    )


@node(execution="continuous", interval_ms=1)
def ProcessContextProbe() -> Callable[[RuntimeContextLimitedAccess], None]:
    reported = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal reported
        if reported:
            return
        reported = True

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

    return each_frame


# ---------------------------------------------------------------------------
# A node's fields and ctx.time
# ---------------------------------------------------------------------------


@node(execution="manual")
class FieldsProbe:
    """Reports the values its fields held when `__init__` ran."""

    gain: float = 0.0
    label: str = ""

    def __init__(self) -> None:
        self._fields_in_init = {"gain": self.gain, "label": self.label}

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report(lambda: {"fields_in_init": self._fields_in_init})


@node(execution="manual")
def TimeProbe(
    *,
    ctx: RuntimeContextFullAccess,
) -> None:
    def observe() -> dict:
        before = engine_media_clock_now_ns()
        context_time = ctx.time
        after = engine_media_clock_now_ns()
        return {"before": before, "context_time": context_time, "after": after}

    _report(observe)


# ---------------------------------------------------------------------------
# Write timestamps, explicit and default
# ---------------------------------------------------------------------------


@node(execution="continuous", interval_ms=1)
def ExplicitlyStampedSource(
    *,
    bags_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        bags_to_downstream.write(json_bytes({"value": 1}), timestamp_ns=EXPLICIT_TIMESTAMP_NS)

    return each_frame


@node(execution="continuous", interval_ms=1)
def DefaultStampedSource(
    *,
    bags_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        bags_to_downstream.write(json_bytes({"value": 1}))

    return each_frame


@node(execution="reactive")
def TimestampCollectingSink(
    *,
    bags_from_upstream: InputPin[JsonMessagesInOrder],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    reported = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal reported
        if reported:
            return
        read = bags_from_upstream.read_message()
        if read is not None:
            reported = True
            bag = json_read(read.payload)
            _report(lambda: {"message": bag, "timestamp_ns": read.timestamp_ns})

    return each_frame


# ---------------------------------------------------------------------------
# What a context stashed past its hook still answers
# ---------------------------------------------------------------------------


@node(execution="manual")
def ContextStasher(*, ctx: RuntimeContextFullAccess) -> None:
    """Keeps the context its body is handed and reads it again from a build, once the body has
    returned."""

    def read_the_kept_context() -> bool:
        _report(
            lambda: {
                "stashed_is_paused": ctx.is_paused(),
                "stashed_node_id": ctx.node_id,
            }
        )
        return True

    computed(read_the_kept_context)


# ---------------------------------------------------------------------------
# The manual-source pattern
# ---------------------------------------------------------------------------


class _WorkerThreadWriting:
    """A thread writing `bags_to_downstream` until the value is closed."""

    def __init__(self, bags_to_downstream: OutputPin[EncodedVideo]) -> None:
        self._stop = threading.Event()

        def produce_from_worker_thread() -> None:
            while not self._stop.is_set():
                bags_to_downstream.write(json_bytes({"origin": "worker-thread"}))
                self._stop.wait(0.01)

        self._worker = threading.Thread(target=produce_from_worker_thread, daemon=True)
        self._worker.start()

    def close(self) -> None:
        self._stop.set()
        self._worker.join(timeout=5.0)


@node(execution="manual")
def WorkerThreadSource(*, bags_to_downstream: OutputPin[EncodedVideo]) -> None:
    """Its output pin, written from a thread a computed value of the node starts."""
    computed(lambda: _WorkerThreadWriting(bags_to_downstream))


@node(execution="reactive")
def WorkerThreadBagSink(
    *,
    bags_from_upstream: InputPin[JsonMessagesInOrder],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    reported = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal reported
        if reported:
            return
        unit = bags_from_upstream.read()
        if unit is not None:
            reported = True
            bag = json_read(unit)
            _report(lambda: {"message": bag})

    return each_frame


# ---------------------------------------------------------------------------
# GPU surfaces from a hook
# ---------------------------------------------------------------------------


@node(execution="manual")
def PixelBufferAcquirer(
    *,
    ctx: RuntimeContextFullAccess,
) -> None:
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


@node(execution="manual")
def RepeatedPixelBufferAcquirer(
    *,
    ctx: RuntimeContextFullAccess,
) -> None:
    """Acquires and closes the same shape more times than the pool has slots."""

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


class _ResourcesConstructedFromAWorkerThread:
    """A thread that constructs privileged resources from the capabilities it was handed,
    joined when the value is closed."""

    def __init__(
        self, gpu_full_access: GpuContextFullAccess, gpu_limited_access: GpuContextLimitedAccess
    ) -> None:
        def construct_resources() -> None:
            def observe() -> dict:
                observation = {}
                with gpu_full_access.acquire_pixel_buffer(48, 48) as surface_handle:
                    observation["privileged_surface_id"] = surface_handle.surface_id
                gpu_full_access.wait_device_idle()
                observation["waited_for_device_idle"] = True
                with gpu_limited_access.acquire_pixel_buffer(48, 48) as from_limited:
                    observation["limited_surface_id"] = from_limited.surface_id
                try:
                    gpu_limited_access.escalate(lambda full: None)
                except RuntimeError as refusal:
                    observation["escalate_refusal"] = str(refusal)
                return observation

            _report(observe)

        self._worker = threading.Thread(target=construct_resources, daemon=True)
        self._worker.start()

    def close(self) -> None:
        self._worker.join(timeout=30.0)


@node(execution="manual")
def WorkerThreadPrivilegedConstructor(*, ctx: RuntimeContextFullAccess) -> None:
    """The native camera's shape: keep the capabilities the body is handed,
    construct privileged resources from a thread the node owns.

    `camera_linux.rs` is the reference — its capture thread holds only
    `GpuContextLimitedAccess` and reaches for privileged construction once at
    thread start. A Python node follows the same pattern; what differs is
    that each privileged call is its own round trip to the parent rather than
    one callback holding an escalate scope, which cannot cross a process.
    """
    gpu_full_access, gpu_limited_access = ctx.gpu_full_access, ctx.gpu_limited_access
    computed(
        lambda: _ResourcesConstructedFromAWorkerThread(gpu_full_access, gpu_limited_access)
    )

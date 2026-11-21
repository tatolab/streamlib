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

import json
import os
import threading
import traceback
from typing import Annotated

from engine_media_clock import engine_media_clock_now_ns
from tatolab.stream import (
    EncodedVideo,
    InputPin,
    OutputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    StreamDelivery,
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


#: An input that takes every bag in order. Each bag is one JSON object, carried
#: as the bytes of a plain byte stream.
BagsInOrder = Annotated[EncodedVideo, StreamDelivery("ordered")]


def _bag_bytes(bag: dict) -> bytes:
    return json.dumps(bag).encode()


def _bag_of(unit) -> dict:
    return json.loads(bytes(unit.payload))


# ---------------------------------------------------------------------------
# Which context type reaches which hook
# ---------------------------------------------------------------------------


@node(execution="manual")
class SetupContextProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report(
            lambda: {
                "context_type": type(ctx).__name__,
                "gpu_full_access_type": type(ctx.gpu_full_access).__name__,
                "gpu_limited_access_type": type(ctx.gpu_limited_access).__name__,
            }
        )


@node(execution="continuous", interval_ms=1)
class ProcessContextProbe:
    def __init__(self) -> None:
        self._reported = False

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self._reported:
            return
        self._reported = True

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
class TimeProbe:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
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
class ExplicitlyStampedSource:
    bags_to_downstream: OutputPin[EncodedVideo]

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        self.bags_to_downstream.write(_bag_bytes({"value": 1}), timestamp_ns=EXPLICIT_TIMESTAMP_NS)


@node(execution="continuous", interval_ms=1)
class DefaultStampedSource:
    bags_to_downstream: OutputPin[EncodedVideo]

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        self.bags_to_downstream.write(_bag_bytes({"value": 1}))


@node
class TimestampCollectingSink:
    bags_from_upstream: InputPin[BagsInOrder]

    def __init__(self) -> None:
        self._reported = False

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self._reported:
            return
        read = self.bags_from_upstream.read_message()
        if read is not None:
            self._reported = True
            bag = _bag_of(read.payload)
            _report(lambda: {"message": bag, "timestamp_ns": read.timestamp_ns})


# ---------------------------------------------------------------------------
# What a context stashed past its hook still answers
# ---------------------------------------------------------------------------


@node(execution="manual")
class ContextStasher:
    """Keeps the setup context and reads it again from `start`."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self._stashed_context = ctx

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        def observe() -> dict:
            return {
                "stashed_is_paused": self._stashed_context.is_paused(),
                "stashed_config": self._stashed_context.config,
                "stashed_node_id": self._stashed_context.node_id,
            }

        _report(observe)


# ---------------------------------------------------------------------------
# The manual-source pattern
# ---------------------------------------------------------------------------


@node(execution="manual")
class WorkerThreadSource:
    """Its output pin, written from a thread the node starts in setup."""

    def __init__(self) -> None:
        self._stop = threading.Event()
        self._worker: "threading.Thread | None" = None

    bags_to_downstream: OutputPin[EncodedVideo]

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        def produce_from_worker_thread() -> None:
            while not self._stop.is_set():
                self.bags_to_downstream.write(_bag_bytes({"origin": "worker-thread"}))
                self._stop.wait(0.01)

        self._worker = threading.Thread(target=produce_from_worker_thread, daemon=True)
        self._worker.start()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        self._stop.set()
        if self._worker is not None:
            self._worker.join(timeout=5.0)


@node
class WorkerThreadBagSink:
    bags_from_upstream: InputPin[BagsInOrder]

    def __init__(self) -> None:
        self._reported = False

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self._reported:
            return
        unit = self.bags_from_upstream.read()
        if unit is not None:
            self._reported = True
            bag = _bag_of(unit)
            _report(lambda: {"message": bag})


# ---------------------------------------------------------------------------
# GPU surfaces from a hook
# ---------------------------------------------------------------------------


@node(execution="manual")
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


@node(execution="manual")
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


@node(execution="manual")
class WorkerThreadPrivilegedConstructor:
    """The native camera's shape: stash the capabilities in setup, construct
    privileged resources from a thread the node owns.

    `camera_linux.rs` is the reference — its capture thread holds only
    `GpuContextLimitedAccess` and reaches for privileged construction once at
    thread start. A Python node follows the same pattern; what differs is
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

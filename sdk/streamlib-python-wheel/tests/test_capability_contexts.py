# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The capability-typed contexts hooks receive, proven against a running engine.

Full-access hooks (`setup`/`teardown`/`start`/`stop`) get
`RuntimeContextFullAccess`; limited hooks (`process`/`on_pause`/`on_resume`) get
`RuntimeContextLimitedAccess` with no `gpu_full_access` at all. `ctx.outputs`
survives the hook that produced it, which is what the manual-source pattern is
built on.

Every probe runs in its own helper process and reports one
`MARKER:PROBE_RESULT` JSON line; the tests drive the app out of process and
assert on that line. Nothing in an observation is a live object — types travel
as their names, because the observation crosses a process boundary.
"""

import json
import re
import time
from pathlib import Path

import pytest

from capability_context_probes import (
    EXPLICIT_TIMESTAMP_NS,
    SURFACE_HEIGHT,
    SURFACE_WIDTH,
)

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "capability_context_app.py"

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")


def run_probe(start_app_under_test, scenario: str) -> dict:
    """One scenario, one observation dict — or a failure carrying the probe's
    own traceback, which names the cause better than a missing marker."""
    app = start_app_under_test(APP, scenario)
    app.await_output_containing("MARKER:PROBE_RESULT", f"the {scenario} probe's result")
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    match = PROBE_RESULT.search(app.output)
    assert match is not None, f"no parseable probe result:\n{app.output}"
    observation = json.loads(match.group(1))
    if "failure" in observation:
        pytest.fail(f"the probe raised in its helper process:\n{observation['failure']}")
    return observation


# ---------------------------------------------------------------------------
# Which context type reaches which hook
# ---------------------------------------------------------------------------


def test_setup_receives_the_full_access_context_with_gpu_full_access(
    start_app_under_test,
):
    observation = run_probe(start_app_under_test, "SetupContextProbe")
    assert observation["context_type"] == "RuntimeContextFullAccess"
    assert observation["gpu_full_access_type"] == "GpuContextFullAccess"
    assert observation["gpu_limited_access_type"] == "GpuContextLimitedAccess"


def test_process_receives_the_limited_context_without_gpu_full_access(
    start_app_under_test,
):
    """The capability split: reaching for the privileged GPU view from
    `process` is an `AttributeError`, not a quietly-working escape hatch."""
    observation = run_probe(start_app_under_test, "ProcessContextProbe")
    assert observation["context_type"] == "RuntimeContextLimitedAccess"
    assert observation["gpu_full_access"] == "attribute_error"
    assert observation["gpu_limited_access_type"] == "GpuContextLimitedAccess"


# ---------------------------------------------------------------------------
# ctx.config and ctx.time
# ---------------------------------------------------------------------------


def test_ctx_config_is_the_dict_the_processor_was_added_with(start_app_under_test):
    observation = run_probe(start_app_under_test, "configured_probe")
    assert observation["config"] == {"gain": 2.5, "label": "left"}


def test_ctx_config_is_an_empty_dict_when_nothing_was_passed(start_app_under_test):
    observation = run_probe(start_app_under_test, "ConfigProbe")
    assert observation["config"] == {}


def test_ctx_time_is_kernel_monotonic_nanoseconds(start_app_under_test):
    """Two kernel reads bracket `ctx.time`, so the value is provably the raw
    `CLOCK_MONOTONIC` domain — not the engine's media clock.

    The bracket is taken inside the helper process, which is the point: the
    clock has to be the machine's, comparable across processes, not each
    interpreter's own epoch.
    """
    observation = run_probe(start_app_under_test, "TimeProbe")
    assert observation["before"] <= observation["context_time"] <= observation["after"]


# ---------------------------------------------------------------------------
# Write timestamps, explicit and default
# ---------------------------------------------------------------------------


def test_an_explicit_write_timestamp_reaches_the_reader_unchanged(
    start_app_under_test,
):
    observation = run_probe(start_app_under_test, "explicit_timestamp")
    assert observation["timestamp_ns"] == EXPLICIT_TIMESTAMP_NS


def test_a_default_write_timestamp_is_kernel_monotonic(start_app_under_test):
    """The stamp a writer defaults to is the machine's monotonic clock, so it
    is comparable against one a reader takes in a different process."""
    before_run = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
    observation = run_probe(start_app_under_test, "default_timestamp")
    after_run = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
    assert before_run <= observation["timestamp_ns"] <= after_run


# ---------------------------------------------------------------------------
# What a stashed context still answers
# ---------------------------------------------------------------------------


def test_a_context_stashed_from_setup_keeps_answering(start_app_under_test):
    """A context kept past its hook still answers, because a helper process has
    no engine view to lease.

    This inverts what the in-process arrangement guaranteed: there, a stashed
    context held an erased borrow of the engine's own view and had to be
    revoked when the hook returned. A child owns its state outright — its
    pause flag is a local announcement from the parent, its config a local
    value — so there is nothing to expire, and nothing that could dangle.
    """
    observation = run_probe(start_app_under_test, "ContextStasher")
    assert observation["stashed_is_paused"] is False
    assert observation["stashed_config"] == {}
    assert observation["stashed_processor_id"]


# ---------------------------------------------------------------------------
# The manual-source pattern
# ---------------------------------------------------------------------------


def test_outputs_captured_in_setup_still_write_from_a_worker_thread(
    start_app_under_test,
):
    """`ctx.outputs` is deliberately not hook-bound: a manual source hands it to
    its own thread and keeps producing between hooks."""
    observation = run_probe(start_app_under_test, "worker_thread_source")
    assert observation["bag"] == {"origin": "worker-thread"}


# ---------------------------------------------------------------------------
# Hook arity: the ctx parameter is required
# ---------------------------------------------------------------------------


ZERO_ARGUMENT_PROCESS_APP = Path(__file__).parent / "zero_argument_process_app.py"


def test_a_zero_argument_process_hook_fails_loudly_with_a_type_error(
    start_app_under_test,
):
    """Hooks require the ctx parameter; a zero-arg `process` must TypeError by
    name in the log, never be silently invoked without a context."""
    app = start_app_under_test(ZERO_ARGUMENT_PROCESS_APP)
    app.await_output_containing("process() raised", "the hook named in the failure")
    app.await_output_containing("TypeError", "the zero-arg TypeError")
    app.interrupt()
    app.await_clean_exit()
    assert "HOOK_BODY_RAN" not in app.markers(), (
        "the zero-arg hook body ran — the host invoked it without a context"
    )


# ---------------------------------------------------------------------------
# GPU surfaces from a hook
# ---------------------------------------------------------------------------


def test_a_hook_acquires_a_pixel_buffer_and_reaches_its_pixels(start_app_under_test):
    observation = run_probe(start_app_under_test, "PixelBufferAcquirer")
    assert isinstance(observation["surface_id"], str) and observation["surface_id"]
    assert (observation["width"], observation["height"]) == (
        SURFACE_WIDTH,
        SURFACE_HEIGHT,
    )
    # The exchange surface itself is covered by `test_pixel_exchange.py`;
    # here it only has to be reachable from a hook.
    assert observation["pixel_access_shape"] == [SURFACE_HEIGHT, SURFACE_WIDTH, 4]


def test_a_closed_pixel_buffer_returns_its_pool_slot(start_app_under_test):
    """Acquire and close beyond the pool depth — every acquire must succeed.

    Regression lock on a real leak: the acquire's surface-store check-in parks a
    strong clone of the buffer in the store, and the pool frees a slot only once
    the strong count returns to 1. Before the fix, `close()` dropped only the
    handle's own clone, so the fifth acquire of any shape failed with "All pixel
    buffers are currently in use" — a per-tick acquirer was dead after one pool
    depth's worth of frames.

    Cross-process it also locks the release round trip: a child's close owes the
    parent a `release_handle`, and a dropped one leaks the slot just the same.
    """
    observation = run_probe(start_app_under_test, "RepeatedPixelBufferAcquirer")
    assert observation["outcomes"] == ["ok"] * 8, (
        f"a closed pixel buffer did not return its pool slot: {observation['outcomes']}"
    )


def test_a_worker_thread_constructs_privileged_resources_like_the_native_camera(
    start_app_under_test,
):
    """The camera's shape, from Python: stash the capabilities in setup, build
    privileged resources from a thread the processor owns.

    `camera_linux.rs` is the reference — its capture thread holds only
    `GpuContextLimitedAccess` and reaches for privileged construction once at
    thread start. What differs here is the spelling, not the reach: each
    privileged call is its own round trip to the parent, and `escalate`'s
    callback refuses because a scope is the one thing that cannot cross.
    """
    observation = run_probe(
        start_app_under_test, "WorkerThreadPrivilegedConstructor"
    )
    assert observation["privileged_surface_id"], (
        "the privileged capability did not produce a surface from a worker thread"
    )
    assert observation["waited_for_device_idle"]
    assert observation["limited_surface_id"]
    assert "atomic" in observation["escalate_refusal"], (
        f"the escalate refusal should say what actually cannot cross: "
        f"{observation['escalate_refusal']!r}"
    )

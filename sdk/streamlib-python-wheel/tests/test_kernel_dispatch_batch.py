# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Batched kernel dispatch, from Python.

Dispatching on its own is synchronous: every call submits and blocks until the
GPU work retires. That is the contract, and it does not change here — what
changes is that a multi-pass filter stops paying it per pass. Two passes inside
one `kernel_dispatch_batch()` scope cost one round trip, one submission and one
fence wait, and the scope still returns with every write visible.

What is worth breaking a build over is that the scope really is all-or-nothing:
a raise inside it submits nothing rather than publishing a half-processed
frame, and the engine is left able to run the next batch. The submission and
stall counts themselves are asserted engine-side, where they are observable —
`cargo test -p streamlib-engine
a_batch_costs_one_submission_and_one_stall_where_separate_dispatches_cost_n`.

Every probe runs in its own helper process and reports one
`MARKER:PROBE_RESULT` JSON line; the tests drive the app out of process and
assert on that line.
"""

import json
import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "compute_kernel_app.py"

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")


def run_probe(start_app_under_test, probe_class_name: str) -> dict:
    """One probe, one observation dict — or a failure carrying the probe's own
    traceback, which names the cause better than a missing marker."""
    app = start_app_under_test(APP, probe_class_name)
    app.await_output_containing(
        "MARKER:PROBE_RESULT", f"the {probe_class_name} result"
    )
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    match = PROBE_RESULT.search(app.output)
    assert match is not None, f"no parseable probe result:\n{app.output}"
    observation = json.loads(match.group(1))
    if "failure" in observation:
        pytest.fail(f"the probe raised in its helper process:\n{observation['failure']}")
    return observation


def test_a_two_pass_filter_runs_as_one_batch(start_app_under_test):
    """The change file's own demo, written the way a user writes it: two
    kernels, an intermediate surface, one scope."""
    observed = run_probe(start_app_under_test, "TwoPassBatchProbe")

    assert observed["first_scope_returned"] is True
    assert (
        observed["source_surface_id"]
        != observed["intermediate_surface_id"]
        != observed["output_surface_id"]
    ), "a two-pass chain runs over three distinct surfaces"


def test_a_batch_scope_leaves_the_engines_recorder_ready_for_the_next_one(
    start_app_under_test,
):
    """The engine's batch recorder is shared and long-lived, and `begin()`
    refuses while a recording is in progress. So a second scope over the same
    surfaces is what catches a first scope that failed to close one — the
    probe reports the refusal rather than raising, so a regression names it."""
    observed = run_probe(start_app_under_test, "TwoPassBatchProbe")
    assert observed["second_scope_error"] is None, (
        "the second batch must run; a 'recording is already in progress' here "
        f"means the first scope stranded the recorder: {observed['second_scope_error']}"
    )


def test_a_raise_inside_a_batch_propagates_unsuppressed(start_app_under_test):
    """Discarding the batch is not swallowing the exception — `__exit__`
    returns False, so the raise reaches the author."""
    observed = run_probe(start_app_under_test, "BatchExceptionProbe")
    assert observed["propagated"] == "the block did not finish", (
        "the exception that discarded the batch must reach the caller"
    )


def test_a_batch_discarded_by_a_raise_leaves_the_engine_usable(start_app_under_test):
    """Nothing was submitted, and nothing was stranded: the probe runs a fresh
    batch after the discarded one and it completes."""
    observed = run_probe(start_app_under_test, "BatchExceptionProbe")
    assert observed["dispatched_after_the_raise"] is True


def test_a_binding_the_shader_does_not_declare_is_refused_at_the_dispatch_line(
    start_app_under_test,
):
    """Checked where it is written, not when the scope closes — a batch that
    only failed at `__exit__` would point at the wrong line."""
    observed = run_probe(start_app_under_test, "BatchRefusalProbe")

    unknown = observed["unknown"]
    assert "sharpen_amount" in unknown, f"must name the unknown binding: {unknown}"
    assert "unbrightened_image" in unknown and "brightened_image" in unknown, (
        f"must name what the shader does declare: {unknown}"
    )


def test_dispatching_one_kernel_twice_in_a_batch_is_refused_saying_why(
    start_app_under_test,
):
    """A kernel owns one descriptor set, so the second bind would hand the
    first dispatch these bindings — silently, since nothing has run yet."""
    observed = run_probe(start_app_under_test, "BatchRefusalProbe")

    twice = observed["same_kernel_twice"]
    assert "descriptor set" in twice, (
        f"must say why one kernel cannot appear twice: {twice}"
    )
    assert "dispatch 0" in twice, f"must name the dispatch it repeats: {twice}"


def test_a_batch_that_has_already_run_refuses_a_further_dispatch(
    start_app_under_test,
):
    """The scope is the batch's whole life. Holding the object past the block
    and dispatching into it says so rather than quietly collecting work that
    will never run."""
    observed = run_probe(start_app_under_test, "BatchRefusalProbe")

    after = observed["after_the_scope"]
    assert "already run" in after, f"must say the batch is spent: {after}"
    assert "kernel_dispatch_batch()" in after, (
        f"must name what to open for the next one: {after}"
    )


def test_a_batch_that_was_never_entered_refuses_rather_than_swallowing_the_work(
    start_app_under_test,
):
    """`__exit__` is the only thing that sends, so dispatching into a batch
    nobody entered would collect GPU work that silently never runs — the shape
    the ADR rejected an explicit `publish()` over. It refuses instead, naming
    the `with` form."""
    observed = run_probe(start_app_under_test, "BatchRefusalProbe")

    never_entered = observed["never_entered"]
    assert "never entered" in never_entered, (
        f"must say the scope was never entered: {never_entered}"
    )
    assert "with ctx.gpu_full_access.kernel_dispatch_batch()" in never_entered, (
        f"must show the form that works: {never_entered}"
    )

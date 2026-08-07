# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`Runtime.wait_until_every_processor_is_running`, around and outside its window.

The wait is legal before `run()` as well as during it, which is not a
convenience: a caller that starts the run loop on another thread cannot know
when that thread reached `run()`, and refusing the early call would make the
harness depend on the scheduling order it exists to remove. What makes it sound
is that a processor carries its state from the moment it is added, so an early
wait is already watching the states `run()` will move.

After teardown there is nothing left to watch and it has to say so, because the
alternative — returning as though the graph were up — is the false ready the
signal was added to remove. A timeout no `Duration` can hold has to be refused
rather than panic across the binding.

`Runtime()` boots the engine without starting it, so nothing here reaches a
device. The in-window behaviour needs a real graph and lives with the harness
that uses it (`test_single_processor_pipeline.py`).
"""

import pytest

import streamlib
from streamlib import RuntimeContextLimitedAccess, output, processor


@processor(execution="continuous", interval_ms=10)
class NeverStartedSource:
    @output()
    def bags_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None: ...


def test_waiting_on_a_graph_that_was_never_run_times_out_naming_the_state():
    """Not an error, a wait — and when nothing ever starts it, the timeout says
    every processor is still `Pending` rather than blaming the caller."""
    runtime = streamlib.Runtime()
    try:
        runtime.add(NeverStartedSource)
        with pytest.raises(RuntimeError, match="Pending"):
            runtime.wait_until_every_processor_is_running(timeout=0.5)
    finally:
        runtime.shutdown()


def test_waiting_on_an_empty_graph_that_was_never_run_returns():
    """No processors, nothing to wait for — the same answer before `run()` as
    after it."""
    runtime = streamlib.Runtime()
    try:
        runtime.wait_until_every_processor_is_running(timeout=5.0)
    finally:
        runtime.shutdown()


def test_waiting_after_shutdown_says_the_runtime_is_gone():
    runtime = streamlib.Runtime()
    runtime.shutdown()

    with pytest.raises(RuntimeError, match="has been shut down"):
        runtime.wait_until_every_processor_is_running(timeout=1.0)


@pytest.mark.parametrize("rejected_timeout", [-1.0, float("nan"), float("inf"), 1e30])
def test_a_timeout_python_can_express_but_a_duration_cannot_is_refused(rejected_timeout):
    """`Duration::from_secs_f64` panics on all of these, and a panic crossing
    the binding aborts the interpreter rather than failing the call."""
    runtime = streamlib.Runtime()
    try:
        with pytest.raises(ValueError, match="finite, non-negative"):
            runtime.wait_until_every_processor_is_running(timeout=rejected_timeout)
    finally:
        runtime.shutdown()

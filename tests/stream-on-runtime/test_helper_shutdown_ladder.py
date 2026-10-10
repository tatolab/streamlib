# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The helper shutdown ladder, proven against a `tatolabd` from outside it.

A terminal's Ctrl-C reaches `tatolabd` alone — every helper leads a process
group of its own — and `tatolabd` walks each helper down the ladder
`docs/plan/ARCHITECTURE.md` §Processor model decides: interrupt the callback,
`stop()`, `teardown()`, then the group. A second interrupt forces the ladder, a
third exits at once. Every assertion is made from outside, because the
failures being ruled out — a surviving helper, a hang, a non-zero exit — are
only visible to a parent.
"""

from __future__ import annotations

import contextlib
import os
import signal
import sys
import threading
import time
from collections.abc import Callable
from pathlib import Path

import pytest

from conftest import TatolabdUnderTest, environment_reaching_no_vulkan_driver
from helper_process_observation import (
    a_process_is_gone_within,
    every_process_still_alive_after,
    helper_process_ids_started_in,
)
from interpreter_lifecycle_processors import (
    TEARDOWN_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE,
    AsleepInItsCallbackAndSlowToTearDownProbe,
    AsleepInItsCallbackProbe,
    AsleepInItsCallbackRecordingItsTeardownProbe,
    StartsAProcessThatOutlivesItProbe,
    ThirtySecondImportProbe,
    WorkerKeepingTeardownGoingProbe,
)
from node_module_whose_describe_holds_the_load import NodeModuleWhoseDescribeHoldsTheLoad
from runtime_process_under_test import (
    ENGINE_STARTED_LOG_LINE,
    STREAM_NEVER_STARTED_LOG_LINE_FRAGMENT,
    STREAM_START_BEGAN_LOG_LINE_PATTERN,
    RuntimeProcessUnderTest,
)
from tatolab.stream import StreamBuilder, stream

StartTatolabd = Callable[..., RuntimeProcessUnderTest]

# How long `tatolabd` with a helper asleep in its callback may take to end after
# a Ctrl-C: the one-second callback budget, the helper's own `stop()` and
# `teardown()`, and the engine's drop. The plan's "about two seconds or less",
# with room for a loaded rig.
ASLEEP_HELPER_EXIT_BUDGET_SECONDS = 3.0

#: How long a helper may outlive a `tatolabd` killed outright. Linux's
#: parent-death signal ends it at once; a macOS helper walks the engine's ladder
#: itself — a second for its callback, five for `teardown()`, half a second to
#: leave — and the rest is room for a loaded rig.
HELPER_OUTLIVING_A_KILLED_RUNTIME_BUDGET_SECONDS = 10.0

# How long `tatolabd` may take to be reaped once its output has ended.
CLEAN_EXIT_BUDGET_AFTER_OUTPUT_ENDS_SECONDS = 10.0

# How long `tatolabd` may take to exit after a Ctrl-C while a survivor it
# started sleeps on: far under the survivor's thirty seconds, so a teardown
# that waits on the survivor's hold of a helper's output cannot pass.
EXIT_BUDGET_WHILE_A_SURVIVOR_SLEEPS_SECONDS = 10.0

STREAM_LISTING_POLL_INTERVAL_SECONDS = 0.01

#: How long the describing interpreter of a load given up to an interrupt may outlive it.
DESCRIBING_INTERPRETER_OUTLIVING_AN_INTERRUPTED_LOAD_BUDGET_SECONDS = 5.0


@stream
def one_processor_asleep_in_its_callback(stream_builder: StreamBuilder) -> None:
    """One processor that sleeps in `process()`."""
    stream_builder.add(AsleepInItsCallbackProbe)


@stream
def two_processors_asleep_recording_their_teardown(stream_builder: StreamBuilder) -> None:
    """Two processors asleep in `process()`, each recording its own teardown."""
    for _ in range(2):
        stream_builder.add(AsleepInItsCallbackRecordingItsTeardownProbe)


@stream
def three_processors_slow_to_tear_down(stream_builder: StreamBuilder) -> None:
    """Three processors asleep in `process()`, each three seconds over its teardown."""
    for _ in range(3):
        stream_builder.add(AsleepInItsCallbackAndSlowToTearDownProbe)


@stream
def a_teardown_only_a_forced_shutdown_cuts_short(stream_builder: StreamBuilder) -> None:
    """One processor with a thirty-second teardown and a forked worker ignoring SIGTERM."""
    stream_builder.add(WorkerKeepingTeardownGoingProbe)


@stream
def a_process_the_stream_started_outlives_it(stream_builder: StreamBuilder) -> None:
    """One processor that starts a process outliving `tatolabd`, holding every descriptor it can."""
    stream_builder.add(StartsAProcessThatOutlivesItProbe)


@stream
def a_helper_still_importing(stream_builder: StreamBuilder) -> None:
    """One processor whose module takes thirty seconds to import in its helper."""
    stream_builder.add(ThirtySecondImportProbe)


@pytest.mark.requires_gpu
def test_ctrl_c_with_a_processor_asleep_in_its_callback_exits_in_about_two_seconds(
    start_tatolabd: StartTatolabd,
):
    """A helper asleep in `process()` costs one interrupt, and its `teardown()` runs."""
    tatolabd = start_tatolabd(one_processor_asleep_in_its_callback)
    tatolabd.await_marker("ASLEEP_IN_PROCESS")
    interrupted_at = time.monotonic()
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    ended_in = time.monotonic() - interrupted_at

    assert tatolabd.marker_payloads("ASLEEP_PROBE_TORE_DOWN"), (
        f"`teardown()` did not run after the interrupt:\n{tatolabd.recent_stderr()}"
    )
    assert ended_in < ASLEEP_HELPER_EXIT_BUDGET_SECONDS, (
        f"tatolabd took {ended_in:.1f}s to end after Ctrl-C:\n{tatolabd.recent_stderr()}"
    )


@pytest.mark.requires_gpu
def test_three_helpers_slow_to_stop_cost_about_one_ladder(start_tatolabd: StartTatolabd):
    """Every helper walks its ladder at the same time.

    Each takes the one-second callback budget and three seconds of teardown, so
    one after another is over twelve seconds and at once is about four.
    """
    tatolabd = start_tatolabd(three_processors_slow_to_tear_down)
    tatolabd.await_marker("SLOW_TO_TEAR_DOWN_ASLEEP", occurrence=3)
    interrupted_at = time.monotonic()
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    ended_in = time.monotonic() - interrupted_at

    assert len(tatolabd.marker_payloads("SLOW_TEARDOWN_FINISHED")) == 3, (
        f"every helper's `teardown()` must still run:\n{tatolabd.recent_stderr()}"
    )
    assert ended_in < 8.0, (
        f"three helpers took {ended_in:.1f}s to stop, which is one after another:\n"
        f"{tatolabd.recent_stderr()}"
    )


@pytest.mark.requires_gpu
def test_a_second_ctrl_c_forces_the_shutdown_past_a_long_teardown(start_tatolabd: StartTatolabd):
    """The second interrupt terminates a helper still inside its `teardown()`,
    and `tatolabd` stops gracefully, having abandoned nothing."""
    tatolabd = start_tatolabd(a_teardown_only_a_forced_shutdown_cuts_short)
    tatolabd.await_marker("TEARDOWN_WORKER_PID")
    tatolabd.interrupt()
    tatolabd.await_marker("LONG_TEARDOWN_BEGAN")
    forced_at = time.monotonic()
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    ended_in = time.monotonic() - forced_at

    assert "MARKER:LONG_TEARDOWN_FINISHED" not in tatolabd.stderr_text, (
        f"the teardown ran to its end, so the second interrupt forced nothing:\n"
        f"{tatolabd.recent_stderr()}"
    )
    assert ended_in < 3.0, (
        f"tatolabd took {ended_in:.1f}s to end after the second Ctrl-C:\n{tatolabd.recent_stderr()}"
    )


@pytest.mark.requires_gpu
def test_a_third_ctrl_c_kills_every_helper_process_group_and_exits_130(
    start_tatolabd: StartTatolabd,
):
    """The third interrupt exits at once, taking the helper's group with it.

    The helper and its worker both ignore SIGTERM, so the forced ladder waits
    out its half-second grace before it sends the group SIGKILL. The third
    interrupt lands well inside that grace, so the worker being gone is the third
    interrupt's doing: the kernel's parent-death signal reaches the helper, never
    a process the helper forked.
    """
    tatolabd = start_tatolabd(a_teardown_only_a_forced_shutdown_cuts_short)
    worker_pid = tatolabd.await_marker("TEARDOWN_WORKER_PID")["pid"]
    tatolabd.interrupt()
    tatolabd.await_marker("LONG_TEARDOWN_BEGAN")
    tatolabd.interrupt()
    time.sleep(0.05)
    tatolabd.interrupt()
    exit_status = tatolabd.await_exit()

    assert exit_status == 130, (
        f"a third Ctrl-C must exit with status 130, got {exit_status}:\n{tatolabd.recent_stderr()}"
    )
    assert a_process_is_gone_within(worker_pid, 2.0), (
        f"the helper's SIGTERM-deaf worker outlived the third interrupt:\n{tatolabd.recent_stderr()}"
    )


@pytest.mark.linux_only_capability(reason="only Linux owns SIGHUP")
@pytest.mark.requires_gpu
def test_sighup_tears_the_graph_down_gracefully(start_tatolabd: StartTatolabd):
    """A closed terminal is a graceful shutdown, `teardown()` included.

    A test suite run under `nohup` hands every child an ignored SIGHUP, which
    the engine keeps ignored — so `tatolabd` is started with SIGHUP at its
    default, the disposition this test is about.
    """
    inherited_hangup_disposition = signal.signal(signal.SIGHUP, signal.SIG_DFL)
    try:
        tatolabd = start_tatolabd(one_processor_asleep_in_its_callback)
    finally:
        signal.signal(signal.SIGHUP, inherited_hangup_disposition)
    tatolabd.await_marker("ASLEEP_IN_PROCESS")
    tatolabd.send_signal(signal.SIGHUP)
    tatolabd.await_clean_exit()

    assert tatolabd.marker_payloads("ASLEEP_PROBE_TORE_DOWN"), (
        f"`teardown()` did not run after SIGHUP:\n{tatolabd.recent_stderr()}"
    )


def kill_a_tatolabd_holding_two_helpers_asleep_in_their_callbacks(
    start_tatolabd: StartTatolabd, teardown_record_directory: Path
) -> "list[int]":
    """SIGKILL `tatolabd`, never its helpers, and name the helpers that were alive."""
    tatolabd = start_tatolabd(
        two_processors_asleep_recording_their_teardown,
        extra_environment={TEARDOWN_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE: str(teardown_record_directory)},
    )
    tatolabd.await_marker("ASLEEP_IN_PROCESS", occurrence=2)
    helper_pids = sorted({report["pid"] for report in tatolabd.marker_payloads("ASLEEP_IN_PROCESS")})
    assert len(helper_pids) == 2, f"expected two helpers asleep:\n{tatolabd.recent_stderr()}"
    tatolabd.send_signal(signal.SIGKILL)
    tatolabd.await_exit()
    return helper_pids


@pytest.mark.requires_gpu
def test_no_helper_outlives_an_app_killed_outright(start_tatolabd: StartTatolabd, tmp_path: Path):
    """A `SIGKILL`ed `tatolabd` runs no teardown, and still leaves no helper behind."""
    teardown_record_directory = tmp_path / "teardown-records"
    teardown_record_directory.mkdir()
    helper_pids = kill_a_tatolabd_holding_two_helpers_asleep_in_their_callbacks(
        start_tatolabd, teardown_record_directory
    )

    survivors = every_process_still_alive_after(
        helper_pids, HELPER_OUTLIVING_A_KILLED_RUNTIME_BUDGET_SECONDS
    )
    assert not survivors, (
        f"helper(s) {survivors} outlived tatolabd by "
        f"{HELPER_OUTLIVING_A_KILLED_RUNTIME_BUDGET_SECONDS}s"
    )


@pytest.mark.skipif(
    sys.platform != "darwin",
    reason="Linux's parent-death signal is SIGKILL, which runs no teardown()",
)
@pytest.mark.requires_gpu
def test_a_helper_whose_app_was_killed_still_runs_its_teardown_on_macos(
    start_tatolabd: StartTatolabd, tmp_path: Path
):
    """The macOS watch ends the channel, so the helper runs the `teardown()` the
    engine can no longer ask for — its callback interrupted first."""
    teardown_record_directory = tmp_path / "teardown-records"
    teardown_record_directory.mkdir()
    helper_pids = kill_a_tatolabd_holding_two_helpers_asleep_in_their_callbacks(
        start_tatolabd, teardown_record_directory
    )
    survivors = every_process_still_alive_after(
        helper_pids, HELPER_OUTLIVING_A_KILLED_RUNTIME_BUDGET_SECONDS
    )
    assert not survivors, f"helper(s) {survivors} outlived tatolabd"

    tore_down = sorted(int(record.name) for record in teardown_record_directory.iterdir())
    assert tore_down == helper_pids, (
        f"helpers {helper_pids} were asleep; only {tore_down} ran `teardown()`"
    )


@pytest.mark.requires_gpu
def test_a_process_the_app_started_never_holds_the_apps_output_past_its_exit(
    start_tatolabd: StartTatolabd,
):
    """Whatever a stream starts inherits no copy of `tatolabd`'s own output.

    A processor starts a survivor that sleeps thirty seconds in a session of its
    own, holding every descriptor it could inherit. Reading `tatolabd`'s output
    to its end must finish within a second of `tatolabd`'s own exit.

    Fail-without-fix: a copy of `tatolabd`'s output a helper could inherit lets
    the survivor hold this pipe for its whole thirty seconds; and a reader of a
    helper's output that `tatolabd` joins without bound hangs its teardown for
    as long.
    """
    tatolabd = start_tatolabd(a_process_the_stream_started_outlives_it)
    survivor_pid = tatolabd.await_marker("SURVIVOR_PID")["pid"]
    exited_at: "list[float]" = []

    def record_when_tatolabd_exits() -> None:
        tatolabd.process.wait()
        exited_at.append(time.monotonic())

    waiter = threading.Thread(target=record_when_tatolabd_exits, daemon=True)
    try:
        tatolabd.await_stderr_containing(ENGINE_STARTED_LOG_LINE)
        waiter.start()
        interrupted_at = time.monotonic()
        tatolabd.interrupt()
        output_ended_at = tatolabd.await_end_of_output()
        waiter.join(timeout=CLEAN_EXIT_BUDGET_AFTER_OUTPUT_ENDS_SECONDS)

        assert exited_at, f"tatolabd never exited:\n{tatolabd.recent_stderr()}"
        assert exited_at[0] - interrupted_at < EXIT_BUDGET_WHILE_A_SURVIVOR_SLEEPS_SECONDS, (
            f"tatolabd took {exited_at[0] - interrupted_at:.1f}s to exit after the interrupt — "
            f"its teardown waited on the survivor:\n{tatolabd.recent_stderr()}"
        )
        assert tatolabd.process.returncode == 0, (
            f"tatolabd exited with {tatolabd.process.returncode}:\n{tatolabd.recent_stderr()}"
        )
        assert output_ended_at - exited_at[0] < 1.0, (
            f"tatolabd's output ended {output_ended_at - exited_at[0]:.1f}s after it exited — "
            f"something it started held it open:\n{tatolabd.recent_stderr()}"
        )
    finally:
        with contextlib.suppress(ProcessLookupError):
            os.kill(survivor_pid, signal.SIGKILL)


@pytest.mark.requires_gpu
def test_ctrl_c_while_a_helper_is_still_importing_exits_promptly(start_tatolabd: StartTatolabd):
    """A helper thirty seconds into importing its processor holds `tatolabd`
    for one interrupt, not for its import."""
    tatolabd = start_tatolabd(a_helper_still_importing)
    tatolabd.await_stderr_containing("helper process started")
    time.sleep(1.0)
    interrupted_at = time.monotonic()
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    ended_in = time.monotonic() - interrupted_at

    assert ended_in < 5.0, (
        f"tatolabd took {ended_in:.1f}s to end, which is the import rather than the "
        f"interrupt:\n{tatolabd.recent_stderr()}"
    )


def test_a_ctrl_c_while_a_slow_describe_loads_ends_tatolabd_before_anything_starts(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
    held_node_module: NodeModuleWhoseDescribeHoldsTheLoad,
    tmp_path: Path,
):
    """#2657: a stop during a load never leaves the load succeeding on a stopped runtime.

    The describe of the stream's one node parks the load `tatolab run` asked
    for. A Ctrl-C to `tatolabd` then ends it cleanly — the load given up, not
    reported as loaded; no engine started; the stream never listed by the
    runtime; and no processor interpreter left behind, the describing one
    included.
    """
    tatolabd = start_tatolabd_running_stream(
        {
            "stream": "held",
            "nodes": [{"name": "held", "type": f"{held_node_module.name}:LoadedFrameRelay", "config": {}}],
        },
        project_directory=held_node_module.project_directory,
        extra_environment=environment_reaching_no_vulkan_driver(tmp_path),
    )
    local_api = tatolabd.local_api_client()
    streams_ever_listed: "list[dict[str, object]]" = []

    def record_every_stream_listed_until_tatolabd_stops_answering() -> None:
        while tatolabd.process.poll() is None:
            try:
                streams_ever_listed.extend(local_api.list_streams())
            except Exception:
                # The local API stops answering as tatolabd shuts down.
                return
            time.sleep(STREAM_LISTING_POLL_INTERVAL_SECONDS)

    stream_listing_watcher = threading.Thread(
        target=record_every_stream_listed_until_tatolabd_stops_answering, daemon=True
    )
    stream_listing_watcher.start()
    assert held_node_module.wait_until_the_load_reaches_the_import(), (
        f"the load never reached the describe:\n{tatolabd.recent_stderr()}"
    )
    describing_interpreter_process_id = held_node_module.describing_interpreter_process_id
    assert describing_interpreter_process_id is not None
    tatolabd.interrupt()
    exit_status = tatolabd.await_exit(timeout=15)
    stream_listing_watcher.join(timeout=5)
    stderr_text = tatolabd.stderr_text

    assert exit_status == 0, tatolabd.recent_stderr()
    assert STREAM_NEVER_STARTED_LOG_LINE_FRAGMENT in stderr_text, tatolabd.recent_stderr()
    assert tatolabd.refusal() is None, tatolabd.recent_stderr()
    assert STREAM_START_BEGAN_LOG_LINE_PATTERN.search(stderr_text) is None, (
        f"the stream began to start after the interrupt:\n{tatolabd.recent_stderr()}"
    )
    assert ENGINE_STARTED_LOG_LINE not in stderr_text, tatolabd.recent_stderr()
    assert not stream_listing_watcher.is_alive()
    assert streams_ever_listed == [], (
        f"tatolabd listed {streams_ever_listed} while the load was being given up"
    )
    assert helper_process_ids_started_in(stderr_text) == [], tatolabd.recent_stderr()
    assert a_process_is_gone_within(
        describing_interpreter_process_id,
        DESCRIBING_INTERPRETER_OUTLIVING_AN_INTERRUPTED_LOAD_BUDGET_SECONDS,
    ), (
        f"the describing interpreter {describing_interpreter_process_id} outlived the "
        f"interrupted load by {DESCRIBING_INTERPRETER_OUTLIVING_AN_INTERRUPTED_LOAD_BUDGET_SECONDS}s"
    )

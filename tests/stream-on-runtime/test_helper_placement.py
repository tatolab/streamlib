# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The behavioural gate on where a Python processor runs.

Every `@node` class runs in its own child process of `tatolabd` — own
interpreter, own GIL. That is the library's reason to exist, so it is asserted
behaviourally rather than trusted: `tatolabd` never imports the processor's
module, and the pid a bag was produced in is not `tatolabd`'s.

`cargo xtask check-no-in-process-placement` gates the vocabulary; this gates
the behaviour. A change that reintroduces the banned model without using any
of the banned words still turns these red.

Rig-only: every scenario here runs a stream, which initialises a GPU context
whose DMA-BUF pool pre-warm needs a driver that can allocate exportable device
memory.
"""

from __future__ import annotations

import time
from collections.abc import Callable
from pathlib import Path

import pytest

from helper_placement_processors import (
    MODULE_IMPORT_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE,
    DiesAbruptlyProbe,
    ForksAWorkerThatOutlivesItProbe,
    ReportsItsOwnProcessSource,
    ReportsItsOwnProcessVideoSink,
    ReportsUpstreamProcessSink,
    SleepsThroughItsOwnSetupProbe,
    SleepsThroughItsOwnShutdownProbe,
)
from helper_process_observation import (
    a_process_is_gone_within,
    assert_runs_in_a_process_of_its_own_beneath,
    helper_process_ids_started_in,
    helper_process_is_still_alive,
)
from local_api_client import RUNNING_NODE_STATE
from runtime_process_under_test import RuntimeProcessUnderTest
from runtime_unit_under_test import STREAM_ON_RUNTIME_SUITE_DIRECTORY
from tatolab.stream import StreamBuilder, TestPatternSource, stream

pytestmark = pytest.mark.requires_gpu

StartTatolabd = Callable[..., RuntimeProcessUnderTest]

# The ladder's own worst case is a second of interrupt plus five of teardown
# plus the group's grace; a helper that answers at once is far inside it. This
# is what separates "the ladder ran" from "the thirty-second callback did".
LADDER_BUDGET_SECONDS = 15.0

#: How `tatolabd` refuses a processor whose helper exited during its setup.
HELPER_DIED_SETTING_UP_LOG_LINE_FRAGMENT = "its helper process died before it finished setting up"

#: A build id no build of this checkout mints: its nonce is all zeros.
ENGINE_BUILD_ID_OF_ANOTHER_BUILD = (
    "0.0.1+0123456789abcdef0123456789abcdef01234567.00000000000000000000000000000000"
)


def _labelled_source_into_sink(
    stream_builder: StreamBuilder, label: str, *, sink_name: "str | None" = None
) -> None:
    source = stream_builder.add(ReportsItsOwnProcessSource, config={"label": label})
    sink = stream_builder.add(ReportsUpstreamProcessSink, name=sink_name)
    stream_builder.connect(source.output("frames_to_downstream"), sink.input("frames_from_upstream"))


@stream
def first_labelled_source_into_sink(stream_builder: StreamBuilder) -> None:
    """A source labelled `first` into a sink reporting where its bags came from."""
    _labelled_source_into_sink(stream_builder, "first")


@stream
def only_labelled_source_into_sink(stream_builder: StreamBuilder) -> None:
    """A source labelled `only` into a sink reporting where its bags came from."""
    _labelled_source_into_sink(stream_builder, "only")


@stream
def reaped_labelled_source_into_sink(stream_builder: StreamBuilder) -> None:
    """A source labelled `reaped` into a sink reporting where its bags came from."""
    _labelled_source_into_sink(stream_builder, "reaped")


@stream
def two_labelled_sources_each_into_its_own_sink(stream_builder: StreamBuilder) -> None:
    """Two instances of one source class, each into a sink named for its label."""
    for label in ("first", "second"):
        _labelled_source_into_sink(stream_builder, label, sink_name=f"{label}Sink")


@stream
def dies_abruptly_beside_a_survivor_pair(stream_builder: StreamBuilder) -> None:
    """A processor that takes its own process down, beside a source-sink pair."""
    stream_builder.add(DiesAbruptlyProbe)
    _labelled_source_into_sink(stream_builder, "survivor")


@stream
def stale_build_labelled_source(stream_builder: StreamBuilder) -> None:
    """A lone source labelled `stale`, for a helper made to see another build."""
    stream_builder.add(ReportsItsOwnProcessSource, config={"label": "stale"})


@stream
def native_test_pattern_into_python_video_sink(stream_builder: StreamBuilder) -> None:
    """A native 64x32 test pattern into a Python sink reporting its own process."""
    pattern = stream_builder.add(TestPatternSource, config={"width": 64, "height": 32})
    sink = stream_builder.add(ReportsItsOwnProcessVideoSink)
    stream_builder.connect(pattern.output("video"), sink.input("video_from_upstream"))


@stream
def one_probe_sleeping_through_its_own_shutdown(stream_builder: StreamBuilder) -> None:
    """A processor parked in `process()` when shutdown arrives."""
    stream_builder.add(SleepsThroughItsOwnShutdownProbe)


@stream
def one_probe_forking_a_worker_that_outlives_it(stream_builder: StreamBuilder) -> None:
    """A processor that forks a worker meant to outlive its helper."""
    stream_builder.add(ForksAWorkerThatOutlivesItProbe)


@stream
def one_probe_sleeping_through_its_own_setup(stream_builder: StreamBuilder) -> None:
    """A processor still inside `setup()` when shutdown arrives."""
    stream_builder.add(SleepsThroughItsOwnSetupProbe)


def run_until_marker_then_interrupt(
    start_tatolabd: StartTatolabd, stream_function: Callable[[StreamBuilder], None], marker_name: str
) -> RuntimeProcessUnderTest:
    """Run a stream until `marker_name` shows up, then Ctrl-C and require a clean exit.

    Sequencing on the marker rather than a timer is what keeps the wait as
    short as the event and as long as the machine needs — and the interrupt
    exercises the same teardown a terminal Ctrl-C does.
    """
    tatolabd = start_tatolabd(stream_function)
    tatolabd.await_marker(marker_name)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    return tatolabd


def importing_process_ids_recorded_in(module_import_record_directory: Path) -> "dict[int, int]":
    """Each process that imported the placement module, mapped to its parent's pid."""
    return {
        int(record.name): int(record.read_text() or 0)
        for record in module_import_record_directory.iterdir()
    }


def test_adding_a_processor_loads_nothing_into_the_app(start_tatolabd: StartTatolabd, tmp_path: Path):
    """The test process's compile import is the only load outside a processor interpreter.

    Every other import of the processor's module — the describe that learns its
    ports at load, and the helper hosting it — is a process `tatolabd` started,
    never `tatolabd` itself. Checked once the stream has loaded and again once
    bags are flowing: a host that constructed the class lazily, on its first
    frame, would pass the first check and fail the second.

    `tatolabd` maps no Python at all; that half is asserted by
    `test_processor_interpreter_lend.py`.
    """
    module_import_record_directory = tmp_path / "module-import-records"
    module_import_record_directory.mkdir()
    tatolabd = start_tatolabd(
        first_labelled_source_into_sink,
        extra_environment={
            MODULE_IMPORT_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE: str(module_import_record_directory)
        },
    )
    tatolabd.await_stream_loaded()
    imported_by_the_load = importing_process_ids_recorded_in(module_import_record_directory)
    tatolabd.await_marker("SINK_PID")
    imported_while_running = importing_process_ids_recorded_in(module_import_record_directory)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert imported_by_the_load, (
        f"no processor interpreter recorded describing the module, so this proves nothing:\n"
        f"{tatolabd.recent_stderr()}"
    )
    assert tatolabd.pid not in imported_by_the_load, (
        f"`tatolabd` imported the processor's module while loading the stream: "
        f"{imported_by_the_load}"
    )
    assert tatolabd.pid not in imported_while_running, (
        f"`tatolabd` imported the processor's module while running the stream — a host "
        f"constructing the class on its first frame would show here: {imported_while_running}"
    )
    assert set(imported_while_running.values()) == {tatolabd.pid}, (
        f"every import of the processor's module is a processor interpreter `tatolabd` "
        f"started; the importers' parents were {imported_while_running}"
    )


def test_a_bag_is_produced_in_a_process_that_is_not_the_apps(start_tatolabd: StartTatolabd):
    """The pid rides in the bag, so the claim is about where `process` ran —
    not about what the engine logged it was going to do."""
    tatolabd = start_tatolabd(only_labelled_source_into_sink)
    sink_report = tatolabd.await_marker("SINK_PID")
    sink_pid, upstream_pid = sink_report["sink_pid"], sink_report["upstream_pid"]
    assert_runs_in_a_process_of_its_own_beneath(upstream_pid, tatolabd.pid)
    assert_runs_in_a_process_of_its_own_beneath(sink_pid, tatolabd.pid)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert upstream_pid != tatolabd.pid, (
        f"the source produced its bag in tatolabd's own process ({tatolabd.pid})"
    )
    assert sink_pid != tatolabd.pid, (
        f"the sink consumed the bag in tatolabd's own process ({tatolabd.pid})"
    )
    assert sink_pid != upstream_pid, (
        f"both processors shared one process ({sink_pid}) — one processor, one helper"
    )


def test_two_instances_of_one_class_get_two_processes(start_tatolabd: StartTatolabd):
    """Registration is per class; placement is per instance."""
    tatolabd = start_tatolabd(two_labelled_sources_each_into_its_own_sink)
    # Awaited twice without naming a label: the two instances report in
    # whichever order they finish booting.
    tatolabd.await_marker("SOURCE_PID", occurrence=2)
    reported_pids = {report["pid"] for report in tatolabd.marker_payloads("SOURCE_PID")}
    for reported_pid in reported_pids:
        assert_runs_in_a_process_of_its_own_beneath(reported_pid, tatolabd.pid)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert len(reported_pids) == 2, (
        f"two instances of one class reported {reported_pids} — expected two distinct pids:\n"
        f"{tatolabd.recent_stderr()}"
    )
    assert tatolabd.pid not in reported_pids


def test_a_native_builtin_stays_in_the_app_process(start_tatolabd: StartTatolabd):
    """The other side of the boundary, discriminated.

    Every Python processor is a child; a native built-in is not.
    `TestPatternSource` is statically linked into `tatolabd` and runs on an
    engine thread, so the frames the Python sink reads were produced in
    `tatolabd`'s own process — and the way to see that is that nothing was
    spawned for it. A two-processor stream starts exactly one helper, and it
    belongs to the sink.

    This is the clause that keeps the ban from reading as "nothing may run in
    the runtime process". Native built-ins do, by design — their per-frame path
    never enters an interpreter.
    """
    tatolabd = start_tatolabd(native_test_pattern_into_python_video_sink)
    sink_pid = tatolabd.await_marker("VIDEO_SINK_PID")["pid"]
    assert_runs_in_a_process_of_its_own_beneath(sink_pid, tatolabd.pid)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    helper_pids = helper_process_ids_started_in(tatolabd.stderr_text)

    assert helper_pids == [sink_pid], (
        f"a stream of one native built-in and one Python processor started {helper_pids} — "
        f"the built-in must not get a process of its own, and the sink must:\n"
        f"{tatolabd.recent_stderr()}"
    )
    assert sink_pid != tatolabd.pid, f"the Python sink ran in tatolabd's own process ({tatolabd.pid})"


def test_no_helper_survives_the_app(start_tatolabd: StartTatolabd):
    """`tatolabd` exiting means every helper was reaped.

    Asserted against the helpers' own pids, which `tatolabd` logs as it starts
    each one. Its process group cannot answer this: every helper leads a group
    of its own — that is what keeps a terminal Ctrl-C from reaching them
    directly — so an assertion on `tatolabd`'s group passes over a leak.

    A survivor holds this processor's iceoryx2 ports open, and the next run
    fails to open them — which reads as a transport bug rather than a leak.
    """
    tatolabd = run_until_marker_then_interrupt(
        start_tatolabd, reaped_labelled_source_into_sink, "SINK_PID"
    )
    helper_pids = helper_process_ids_started_in(tatolabd.stderr_text)
    assert len(helper_pids) == 2, (
        f"expected the source and the sink to each report a helper pid, got {helper_pids}:\n"
        f"{tatolabd.recent_stderr()}"
    )

    survivors = [pid for pid in helper_pids if helper_process_is_still_alive(pid)]
    assert not survivors, f"helper processes {survivors} outlived the tatolabd that spawned them"


def test_a_processor_asleep_in_its_callback_still_runs_its_teardown(start_tatolabd: StartTatolabd):
    """The ladder `docs/plan/ARCHITECTURE.md` §Processor model decides, end to end.

    Fail-without-fix: with the old pair of five-second reply deadlines the
    sleeping callback misses `stopped`, the helper is marked gone, and its
    `teardown()` is skipped outright — so `SLEEPER_TORE_DOWN` never arrives.
    """
    tatolabd = start_tatolabd(one_probe_sleeping_through_its_own_shutdown)
    tatolabd.await_marker("ASLEEP_IN_PROCESS")
    interrupted_at = time.monotonic()
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    ended_in = time.monotonic() - interrupted_at

    assert tatolabd.marker_payloads("SLEEPER_STOPPED"), (
        f"`stop()` did not run after the interrupt:\n{tatolabd.recent_stderr()}"
    )
    assert tatolabd.marker_payloads("SLEEPER_TORE_DOWN"), (
        f"`teardown()` did not run after the interrupt:\n{tatolabd.recent_stderr()}"
    )
    assert "MARKER:SLEPT_THE_WHOLE_WAY" not in tatolabd.stderr_text, (
        f"the callback returned on its own, so nothing interrupted it:\n{tatolabd.recent_stderr()}"
    )
    assert ended_in < LADDER_BUDGET_SECONDS, (
        f"tatolabd took {ended_in:.1f}s to end, which is the callback's thirty seconds rather "
        f"than the ladder's budget:\n{tatolabd.recent_stderr()}"
    )


def test_a_processor_interrupted_while_still_setting_up_still_tears_down(
    start_tatolabd: StartTatolabd,
):
    """The route onto the ladder the engine's `stop()` hook never reaches.

    A helper still inside `setup()` has never had `stop()` called on it, so the
    commands whose replies the ladder waits for are only on the wire because
    the ladder asks for them itself.

    Fail-without-fix: leave the ask in `stop()` alone and this helper is
    SIGINT'd, answers its refusal, and is then killed with its group — its
    `teardown()` never asked for and never run.
    """
    tatolabd = start_tatolabd(one_probe_sleeping_through_its_own_setup)
    tatolabd.await_marker("ASLEEP_IN_SETUP")
    interrupted_at = time.monotonic()
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    ended_in = time.monotonic() - interrupted_at

    assert tatolabd.marker_payloads("INTERRUPTED_SETUP_TORE_DOWN"), (
        f"an interrupted `setup()` was never given its `teardown()`:\n{tatolabd.recent_stderr()}"
    )
    assert "MARKER:SLEPT_THE_WHOLE_SETUP" not in tatolabd.stderr_text, (
        f"`setup()` returned on its own, so nothing interrupted it:\n{tatolabd.recent_stderr()}"
    )
    assert ended_in < LADDER_BUDGET_SECONDS, (
        f"tatolabd took {ended_in:.1f}s to end, which is the registration budget rather than "
        f"the ladder's:\n{tatolabd.recent_stderr()}"
    )


def test_a_worker_a_processor_forked_goes_down_with_the_apps_helper(start_tatolabd: StartTatolabd):
    """A processor's descendants die with it.

    Fail-without-fix: the kills target the helper's pid, the worker outlives
    `tatolabd` holding whatever it inherited, and this finds it still running.
    """
    tatolabd = run_until_marker_then_interrupt(
        start_tatolabd, one_probe_forking_a_worker_that_outlives_it, "WORKER_PID"
    )
    worker_pid = tatolabd.marker_payloads("WORKER_PID")[0]["worker_pid"]

    assert a_process_is_gone_within(worker_pid, 2.0), (
        f"the worker a processor forked outlived the tatolabd that spawned it:\n"
        f"{tatolabd.recent_stderr()}"
    )


def test_a_crashed_helper_is_surfaced_and_the_pipeline_keeps_running(start_tatolabd: StartTatolabd):
    """The owner's crash policy: surface, keep running.

    A processor that takes its own process down mid-run is reported in error,
    and the rest of the stream is unaffected. What notices is the Manual loop's
    hundred-millisecond poll asking the process itself; the bridge reader seeing
    EOF is the second signal, and a descendant holding that socket defers it
    indefinitely. Break both and the death goes unreported until shutdown, which
    is what this locks.
    """
    tatolabd = start_tatolabd(dies_abruptly_beside_a_survivor_pair)
    tatolabd.await_marker("ABOUT_TO_DIE")
    failure_line = tatolabd.await_stderr_containing("Processor failed unrecoverably")
    sink_reports_before_the_failure_was_surfaced = sum(
        1
        for line in tatolabd.stderr_lines[: tatolabd.stderr_lines.index(failure_line) + 1]
        if "MARKER:SINK_PID" in line
    )
    # The survivors were already producing before the crash and must still be
    # producing after it — the stream is not brought down with one processor.
    tatolabd.await_marker("SINK_PID", occurrence=sink_reports_before_the_failure_was_surfaced + 1)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()


def test_a_helper_that_imported_another_engine_build_is_refused_naming_both_builds(
    start_tatolabd: StartTatolabd, tmp_path: Path
):
    """The whole handshake through a real `tatolabd`: it hands its build id
    over, the child compares it with its own and refuses on raw stderr, and the
    processor's start is refused by name carrying what the child wrote — the
    only place an operator reads which two builds disagreed.

    The child is made to see another build the one way a test can reach it
    before `main()` runs: a `sitecustomize` in the stream's project directory —
    on the child's `PYTHONPATH` — rewrites the id `tatolabd` handed it, which is
    what a stale `tatolab.runtime` amounts to from the check's side. Describing
    sets no `STREAMLIB_ENTRYPOINT`, so the load itself is untouched. The id it
    rewrites is recorded first: `tatolabd`'s, which the runtime unit's lend
    shares.

    Fail-without-fix: drop the stderr tail from the refusal and the refusal
    names the processor but neither build.
    """
    child_startup_directory = tmp_path / "stale-build-project"
    child_startup_directory.mkdir()
    handed_engine_build_id_record = tmp_path / "engine-build-id-tatolabd-handed-over"
    (child_startup_directory / "sitecustomize.py").write_text(
        "import os\n"
        "import sys\n"
        f"sys.path.append({str(STREAM_ON_RUNTIME_SUITE_DIRECTORY)!r})\n"
        "if 'STREAMLIB_ENTRYPOINT' in os.environ:\n"
        f"    with open({str(handed_engine_build_id_record)!r}, 'w') as record:\n"
        "        record.write(os.environ.get('STREAMLIB_ENGINE_BUILD_ID', ''))\n"
        f"    os.environ['STREAMLIB_ENGINE_BUILD_ID'] = {ENGINE_BUILD_ID_OF_ANOTHER_BUILD!r}\n"
    )

    tatolabd = start_tatolabd(stale_build_labelled_source, project_directory=child_startup_directory)
    tatolabd.await_stderr_containing(HELPER_DIED_SETTING_UP_LOG_LINE_FRAGMENT)
    processor_states = {
        node["name"]: node.get("components", {}).get("state")
        for node in tatolabd.local_api_client().graph()["nodes"]
    }
    tatolabd.interrupt()
    tatolabd.await_exit()

    tatolabd_engine_build_id = handed_engine_build_id_record.read_text()
    assert tatolabd_engine_build_id, "tatolabd handed its helper no engine build id"
    refusal = (
        "[reportsitsownprocesssource] its helper process died before it finished setting "
        "up. Its standard error ended with:\n"
        f"[streamlib] this helper imported engine build {tatolabd_engine_build_id}"
    )
    assert refusal in tatolabd.stderr_text, tatolabd.recent_stderr()
    assert f"its parent is engine build {ENGINE_BUILD_ID_OF_ANOTHER_BUILD}" in tatolabd.stderr_text
    assert "reportsitsownprocesssource" in processor_states, processor_states
    assert processor_states["reportsitsownprocesssource"] != RUNNING_NODE_STATE, (
        f"the processor started anyway: {processor_states}"
    )


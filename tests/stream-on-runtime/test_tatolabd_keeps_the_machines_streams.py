# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`tatolabd` keeps the machine's streams: past the `tatolab` that loaded them,
and across its own restarts.

Each test drives the runtime unit's own `tatolabd` and `tatolab` under the
test's machine root, as a user at a terminal does: `tatolab open --play` from two
project directories, `tatolabd` restarted with a Ctrl-C and again after a
SIGKILL, `stop`, `rm` and `streams` one call at a time, and a `tatolab dev`
ended by a Ctrl-C and by a SIGKILL leaving its stream running.

A started stream needs a GPU context, so most tests here are `requires_gpu`;
their streams are a test pattern into a Python node that reads its frames, so
no camera and no display is needed. A stopped stream through restarts and `rm`
runs without one.
"""

from __future__ import annotations

import json
import os
import signal
import stat
import subprocess
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from conftest import (
    QUEUED_STREAM_ACTION_LINE_PATTERN,
    PrivateMachineDirectories,
    TatolabdUnderTest,
    TatolabRunOrDevOnTheRuntime,
)
from local_api_client import LocalApiClient
from stream_runs_on_tatolabd import (
    TatolabRunOfAProject,
    stream_id_of_the_stream_in,
)
from tatolabd_pattern_stream_projects import (
    ALPHA_STREAM_FUNCTION,
    BRAVO_STREAM_FUNCTION,
    PROJECT_STREAM_NODE_COUNT,
    STREAM_RUNNING_TIMEOUT_SECONDS,
    TATOLABD_EXIT_TIMEOUT_SECONDS,
    await_the_reloads_queued_at_the_start,
    listed_streams_by_id,
    listing_of,
    location_of_the_pattern_project,
    make_project_running_a_pattern_stream,
    restart_tatolabd,
    stop_tatolabd_cleanly,
    stream_id_of_the_pattern_project,
    stream_record_path,
)


STREAM_RECORD_FILE_MODE = 0o600
STREAM_RECORD_SCHEMA_VERSION = 9


def follow_every_action_the_verb_queued(
    tatolabd: TatolabdUnderTest,
    verb_standard_output: str,
    *,
    timeout: float = STREAM_RUNNING_TIMEOUT_SECONDS,
) -> "list[dict[str, Any]]":
    """Follow each stream action a `tatolab open --play`, `play`, `stop` or `rm` printed it
    queued to `done`, in order; return each as `describe_stream_action` ended it."""
    queued_actions = list(QUEUED_STREAM_ACTION_LINE_PATTERN.finditer(verb_standard_output))
    assert queued_actions, verb_standard_output
    local_api = tatolabd.local_api_client()
    described_actions = []
    for queued in queued_actions:
        described = local_api.stream_action_once_ended(queued.group("action"), timeout=timeout)
        assert described["state"] == "done", described
        described_actions.append(described)
    return described_actions


def follow_the_action_the_verb_queued(
    tatolabd: TatolabdUnderTest,
    verb_standard_output: str,
    *,
    timeout: float = STREAM_RUNNING_TIMEOUT_SECONDS,
) -> "dict[str, Any]":
    """The last stream action the verb printed it queued, once every one of them is `done`."""
    return follow_every_action_the_verb_queued(tatolabd, verb_standard_output, timeout=timeout)[-1]


def assert_the_pattern_is_wired_to_the_reader_inside_the_stream(
    local_api: LocalApiClient, stream_id: str
) -> None:
    """The stream's one link, the pattern into the reader, reaches `wired`: proof it runs,
    read from inside the stream, never by reading a port across its edge."""
    stream_links = local_api.graph(stream_id)["links"]
    assert len(stream_links) == 1, stream_links
    link_state = local_api.await_link_state(stream_links[0]["id"], "wired", stream=stream_id)
    assert link_state == "wired", f"the pattern-to-reader link of `{stream_id}` is {link_state}"


def stream_ids_in_the_machine_graph(local_api: LocalApiClient) -> "list[str]":
    """Each loaded stream's id, from `graph` with no stream named."""
    return sorted(stream_graph["stream"] for stream_graph in local_api.graph()["streams"])


def read_stream_record(tatolabd: TatolabdUnderTest, stream_id: str) -> "dict[str, Any]":
    """The stream record the runtime wrote, after checking it is its owner's alone."""
    record_path = stream_record_path(tatolabd.machine_directories, stream_id)
    assert stat.S_IMODE(os.stat(record_path).st_mode) == STREAM_RECORD_FILE_MODE, record_path
    return json.loads(record_path.read_text(encoding="utf-8"))


def assert_tatolab_succeeded(completed: "subprocess.CompletedProcess[str]") -> str:
    """Require exit 0 from a `tatolab` verb; return its standard output."""
    assert completed.returncode == 0, (
        f"`{' '.join(map(str, completed.args))}` exited {completed.returncode}; "
        f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
    )
    return completed.stdout


def write_the_record_run_leaves(
    private_machine_directories: PrivateMachineDirectories,
    project_directory: Path,
    *,
    stopped: bool,
) -> "dict[str, Any]":
    """Write the stream record `tatolab open --play` leaves for the project's
    one stream — and, `stopped`, the one a `tatolab stop` after it leaves —
    its location read from the project file and its graph document, under the id
    they compute to; return the record.

    Starting a stream needs a GPU context, so a GPU-free test cannot have the
    runtime write it; a restart holds a stopped stream loaded and stopped, never
    starting it, so from here on the runtime treats it exactly as one it stopped
    itself. A record not stopped is one a `tatolabd` start re-loads playing.
    """
    [graph_document_path] = sorted((project_directory / "streams").glob("*.graph.json"))
    stream_name = graph_document_path.name.removesuffix(".graph.json")
    record = {
        "schema_version": STREAM_RECORD_SCHEMA_VERSION,
        "stream_id": stream_id_of_the_stream_in(project_directory, stream_name),
        "location": location_of_the_pattern_project(project_directory.resolve(), stream_name),
        "interpreter": str(project_directory / ".venv" / "bin" / "python"),
        "stopped": stopped,
        "closed": False,
        "paused": False,
        "failed_because": None,
        "runtime_crashes_in_a_row_implicating_it": 0,
        "pending_stream_actions": [],
    }
    record_path = stream_record_path(
        private_machine_directories.machine_directories, record["stream_id"]
    )
    record_path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    record_path.write_text(json.dumps(record), encoding="utf-8")
    record_path.chmod(STREAM_RECORD_FILE_MODE)
    return record


def test_a_stopped_stream_stays_stopped_across_restarts_and_rm_forgets_it(
    private_machine_directories: PrivateMachineDirectories,
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    project_directory = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM_FUNCTION)
    alpha_stream_id = stream_id_of_the_pattern_project(project_directory, ALPHA_STREAM_FUNCTION)
    record = write_the_record_run_leaves(
        private_machine_directories, project_directory, stopped=True
    )
    assert record["stream_id"] == alpha_stream_id, record
    recorded_project_directory = record["location"]["project_directory"]
    stopped_listing = listing_of(
        alpha_stream_id,
        ALPHA_STREAM_FUNCTION,
        "stopped",
        Path(recorded_project_directory),
        PROJECT_STREAM_NODE_COUNT,
    )

    tatolabd = start_tatolabd()
    await_the_reloads_queued_at_the_start(tatolabd, 1)
    assert tatolabd.local_api_client().list_streams() == [stopped_listing]
    listed_table = assert_tatolab_succeeded(run_tatolab("streams", working_directory=project_directory))
    assert listed_table.splitlines()[0].split() == [
        "ID",
        "NAME",
        "STATE",
        "NODES",
        "PROJECT",
        "ASSET",
    ]
    assert listed_table.splitlines()[1].split() == [
        alpha_stream_id,
        ALPHA_STREAM_FUNCTION,
        "stopped",
        str(PROJECT_STREAM_NODE_COUNT),
        recorded_project_directory,
        record["location"]["stream_asset"],
    ]

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    await_the_reloads_queued_at_the_start(tatolabd, 1)
    assert tatolabd.local_api_client().list_streams() == [stopped_listing]
    restarted_record = read_stream_record(tatolabd, alpha_stream_id)
    assert restarted_record["stopped"] is True
    refused_stop = run_tatolab("stop", working_directory=project_directory)
    assert refused_stop.returncode == 1, refused_stop.stderr
    assert "none to stop" in refused_stop.stderr, refused_stop.stderr

    removed = assert_tatolab_succeeded(run_tatolab("rm", working_directory=project_directory))
    assert removed.endswith(f" queued: remove of {recorded_project_directory}\n"), removed
    removal = follow_the_action_the_verb_queued(tatolabd, removed)
    assert (removal["kind"], removal["project_directory"]) == ("remove", recorded_project_directory)
    assert not stream_record_path(tatolabd.machine_directories, alpha_stream_id).exists()
    assert assert_tatolab_succeeded(
        run_tatolab("streams", working_directory=project_directory)
    ) == "No streams in this runtime.\n"
    refused_rm = run_tatolab("rm", working_directory=project_directory)
    assert refused_rm.returncode == 1, refused_rm.stderr
    assert "holds no stream of the project" in refused_rm.stderr, refused_rm.stderr

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    await_the_reloads_queued_at_the_start(tatolabd, 0)
    assert tatolabd.local_api_client().list_streams() == []
    stop_tatolabd_cleanly(tatolabd)


@pytest.mark.requires_gpu
def test_two_projects_streams_run_come_back_after_a_restart_and_stop_start_rm_hold(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    alpha_project = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM_FUNCTION)
    alpha_stream_id = stream_id_of_the_pattern_project(alpha_project, ALPHA_STREAM_FUNCTION)
    bravo_project = make_project_running_a_pattern_stream(make_tatolab_project, BRAVO_STREAM_FUNCTION)
    bravo_stream_id = stream_id_of_the_pattern_project(bravo_project, BRAVO_STREAM_FUNCTION)
    tatolabd = start_tatolabd()

    for stream_id, stream_function_name, project_directory in (
        (alpha_stream_id, ALPHA_STREAM_FUNCTION, alpha_project),
        (bravo_stream_id, BRAVO_STREAM_FUNCTION, bravo_project),
    ):
        queued = assert_tatolab_succeeded(
            run_tatolab("open", "--play", working_directory=project_directory)
        )
        opened, played = follow_every_action_the_verb_queued(tatolabd, queued)
        assert queued == (
            f"{opened['action']} queued: open of {project_directory}\n"
            f"  follow it: tatolab logs -f\n"
            f"{played['action']} queued: start_playback of {project_directory}\n"
            f"  follow it: tatolab logs -f\n"
        )
        assert (opened["kind"], played["kind"]) == ("open", "start_playback")
        assert played["streams"] == [
            {"stream": stream_id, "display_name": stream_function_name, "state": "running"}
        ], played
    alpha_running = listing_of(alpha_stream_id, ALPHA_STREAM_FUNCTION, "running", alpha_project, PROJECT_STREAM_NODE_COUNT)
    bravo_running = listing_of(bravo_stream_id, BRAVO_STREAM_FUNCTION, "running", bravo_project, PROJECT_STREAM_NODE_COUNT)

    def assert_both_streams_run(tatolabd: TatolabdUnderTest) -> None:
        local_api = tatolabd.local_api_client()
        for stream_id in (alpha_stream_id, bravo_stream_id):
            local_api.await_every_node_running(
                stream=stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS
            )
        assert listed_streams_by_id(local_api) == {
            alpha_stream_id: alpha_running,
            bravo_stream_id: bravo_running,
        }
        assert stream_ids_in_the_machine_graph(local_api) == [alpha_stream_id, bravo_stream_id]
        for stream_id in (alpha_stream_id, bravo_stream_id):
            assert_the_pattern_is_wired_to_the_reader_inside_the_stream(local_api, stream_id)

    assert_both_streams_run(tatolabd)
    table_rows = [
        row.split()
        for row in assert_tatolab_succeeded(
            run_tatolab("streams", working_directory=alpha_project)
        ).splitlines()[1:]
    ]
    assert sorted(table_rows) == [
        [
            alpha_stream_id,
            ALPHA_STREAM_FUNCTION,
            "running",
            str(PROJECT_STREAM_NODE_COUNT),
            str(alpha_project),
            location_of_the_pattern_project(alpha_project, ALPHA_STREAM_FUNCTION)["stream_asset"],
        ],
        [
            bravo_stream_id,
            BRAVO_STREAM_FUNCTION,
            "running",
            str(PROJECT_STREAM_NODE_COUNT),
            str(bravo_project),
            location_of_the_pattern_project(bravo_project, BRAVO_STREAM_FUNCTION)["stream_asset"],
        ],
    ]
    for stream_id, project_directory in ((alpha_stream_id, alpha_project), (bravo_stream_id, bravo_project)):
        record = read_stream_record(tatolabd, stream_id)
        assert (record["stream_id"], record["location"]["project_directory"], record["stopped"]) == (
            stream_id,
            str(project_directory),
            False,
        )

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    await_the_reloads_queued_at_the_start(tatolabd, 2)
    assert_both_streams_run(tatolabd)

    stopped = assert_tatolab_succeeded(run_tatolab("stop", working_directory=alpha_project))
    assert stopped.endswith(f" queued: stop of {alpha_project}\n"), stopped
    assert follow_the_action_the_verb_queued(tatolabd, stopped)["kind"] == "stop"
    alpha_stopped = listing_of(
        alpha_stream_id, ALPHA_STREAM_FUNCTION, "stopped", alpha_project, PROJECT_STREAM_NODE_COUNT
    )
    local_api = tatolabd.local_api_client()
    assert listed_streams_by_id(local_api) == {alpha_stream_id: alpha_stopped, bravo_stream_id: bravo_running}
    assert stream_ids_in_the_machine_graph(local_api) == [alpha_stream_id, bravo_stream_id]

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    await_the_reloads_queued_at_the_start(tatolabd, 2)
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(stream=bravo_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_streams_by_id(local_api) == {alpha_stream_id: alpha_stopped, bravo_stream_id: bravo_running}
    assert stream_ids_in_the_machine_graph(local_api) == [alpha_stream_id, bravo_stream_id]

    started = assert_tatolab_succeeded(run_tatolab("play", working_directory=alpha_project))
    assert started.endswith(f" queued: start_playback of {alpha_project}\n  follow it: tatolab logs -f\n"), started
    start = follow_the_action_the_verb_queued(tatolabd, started)
    assert [(stream["stream"], stream["state"]) for stream in start["streams"]] == [
        (alpha_stream_id, "running")
    ], start
    assert_both_streams_run(tatolabd)
    assert read_stream_record(tatolabd, alpha_stream_id)["stopped"] is False

    removed = assert_tatolab_succeeded(run_tatolab("rm", working_directory=alpha_project))
    assert removed.endswith(f" queued: remove of {alpha_project}\n"), removed
    assert follow_the_action_the_verb_queued(tatolabd, removed)["kind"] == "remove"
    assert not stream_record_path(tatolabd.machine_directories, alpha_stream_id).exists()
    assert listed_streams_by_id(local_api) == {bravo_stream_id: bravo_running}
    assert stream_ids_in_the_machine_graph(local_api) == [bravo_stream_id]

    tatolabd = restart_tatolabd(tatolabd, start_tatolabd)
    await_the_reloads_queued_at_the_start(tatolabd, 1)
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(stream=bravo_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_streams_by_id(local_api) == {bravo_stream_id: bravo_running}
    stop_tatolabd_cleanly(tatolabd)


@pytest.mark.requires_gpu
def test_a_stream_dev_loaded_runs_on_past_a_ctrl_c_and_a_killed_cli(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """Every load persists: a `tatolab dev` ended by a Ctrl-C, and another
    killed outright, each leave the stream it loaded running and recorded,
    beside a stream `tatolab open --play` loaded."""
    alpha_project = make_project_running_a_pattern_stream(make_tatolab_project, ALPHA_STREAM_FUNCTION)
    alpha_stream_id = stream_id_of_the_pattern_project(alpha_project, ALPHA_STREAM_FUNCTION)
    bravo_project = make_project_running_a_pattern_stream(make_tatolab_project, BRAVO_STREAM_FUNCTION)
    bravo_stream_id = stream_id_of_the_pattern_project(bravo_project, BRAVO_STREAM_FUNCTION)
    tatolabd = start_tatolabd()
    follow_the_action_the_verb_queued(
        tatolabd,
        assert_tatolab_succeeded(run_tatolab("open", "--play", working_directory=bravo_project)),
    )
    local_api = tatolabd.local_api_client()
    bravo_running = listing_of(bravo_stream_id, BRAVO_STREAM_FUNCTION, "running", bravo_project, PROJECT_STREAM_NODE_COUNT)
    alpha_running = listing_of(alpha_stream_id, ALPHA_STREAM_FUNCTION, "running", alpha_project, PROJECT_STREAM_NODE_COUNT)

    def dev_alpha() -> TatolabRunOrDevOnTheRuntime:
        dev = tatolabd.run_stream(TatolabRunOfAProject(working_directory=alpha_project), verb="dev")
        loaded = dev.await_loaded(timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
        assert (loaded["stream_id"], loaded["display_name"]) == (alpha_stream_id, ALPHA_STREAM_FUNCTION)
        local_api.await_every_node_running(stream=alpha_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
        assert listed_streams_by_id(local_api) == {
            alpha_stream_id: alpha_running,
            bravo_stream_id: bravo_running,
        }
        return dev

    dev_ended_by_ctrl_c = dev_alpha()
    dev_ended_by_ctrl_c.interrupt()
    assert dev_ended_by_ctrl_c.await_exit(timeout=TATOLABD_EXIT_TIMEOUT_SECONDS) == 0, (
        dev_ended_by_ctrl_c.recent_stderr()
    )
    assert listed_streams_by_id(local_api) == {
        alpha_stream_id: alpha_running,
        bravo_stream_id: bravo_running,
    }, "a Ctrl-C to `dev` ends the following, never the stream"

    dev_ended_by_sigkill = dev_alpha()
    os.kill(dev_ended_by_sigkill.pid, signal.SIGKILL)
    assert dev_ended_by_sigkill.await_exit() == -signal.SIGKILL
    local_api.await_every_node_running(stream=alpha_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    local_api.await_every_node_running(stream=bravo_stream_id, timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert listed_streams_by_id(local_api) == {
        alpha_stream_id: alpha_running,
        bravo_stream_id: bravo_running,
    }, "a killed `dev` leaves its stream running"
    assert stream_ids_in_the_machine_graph(local_api) == [alpha_stream_id, bravo_stream_id]
    assert read_stream_record(tatolabd, alpha_stream_id)["stopped"] is False
    stop_tatolabd_cleanly(tatolabd)


#: The stream functions of the recorded streams a start re-loads ahead of the two
#: an owner changes while it re-loads; each is a GPU start of its own, which
#: holds the window open.
STREAM_FUNCTIONS_RE_LOADED_AHEAD = tuple(f"a{index}_pattern" for index in range(6))
#: Each named so its project folder, the leading part of its stream id, sorts
#: after every stream re-loaded ahead, as a start re-loads in record-file order.
STOPPED_WHILE_RE_LOADING_STREAM_FUNCTION = "zz_stopped_pattern"
REMOVED_WHILE_RE_LOADING_STREAM_FUNCTION = "zz_removed_pattern"
#: How long the re-loads a start queued for eight projects may take to end.
RE_LOAD_OF_SEVEN_STREAMS_TIMEOUT_SECONDS = 2 * STREAM_RUNNING_TIMEOUT_SECONDS


@pytest.mark.requires_gpu
def test_a_stop_or_rm_made_while_tatolabd_re_loads_its_recorded_streams_holds(
    private_machine_directories: PrivateMachineDirectories,
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """`tatolabd` serves before it re-loads its recorded streams. An owner who
    stops a stream and removes another the moment the socket answers — before
    the re-load reaches them — is never overruled by it: the stopped stream
    stays held stopped with its graph, and the removed one is not loaded."""
    projects_by_stream_function = {
        stream_function_name: make_project_running_a_pattern_stream(
            make_tatolab_project, stream_function_name
        )
        for stream_function_name in (
            *STREAM_FUNCTIONS_RE_LOADED_AHEAD,
            STOPPED_WHILE_RE_LOADING_STREAM_FUNCTION,
            REMOVED_WHILE_RE_LOADING_STREAM_FUNCTION,
        )
    }
    records_by_stream_function = {
        stream_function_name: write_the_record_run_leaves(
            private_machine_directories, project_directory, stopped=False
        )
        for stream_function_name, project_directory in projects_by_stream_function.items()
    }
    stream_id_of = {
        stream_function_name: record["stream_id"]
        for stream_function_name, record in records_by_stream_function.items()
    }
    stopped_stream_id = stream_id_of[STOPPED_WHILE_RE_LOADING_STREAM_FUNCTION]
    removed_stream_id = stream_id_of[REMOVED_WHILE_RE_LOADING_STREAM_FUNCTION]

    tatolabd = start_tatolabd()
    some_project = projects_by_stream_function[STOPPED_WHILE_RE_LOADING_STREAM_FUNCTION]
    stop_queued = assert_tatolab_succeeded(
        run_tatolab("stop", working_directory=some_project)
    )
    removal_queued = assert_tatolab_succeeded(
        run_tatolab(
            "rm",
            "--dir",
            str(projects_by_stream_function[REMOVED_WHILE_RE_LOADING_STREAM_FUNCTION]),
            working_directory=some_project,
        )
    )
    local_api = tatolabd.local_api_client()
    re_load_reached_them_first = any(
        listed["stream"] in (stopped_stream_id, removed_stream_id)
        and listed["state"] == "running"
        for listed in local_api.list_streams()
    )
    await_the_reloads_queued_at_the_start(
        tatolabd, len(projects_by_stream_function), timeout=RE_LOAD_OF_SEVEN_STREAMS_TIMEOUT_SECONDS
    )
    for verb_standard_output in (stop_queued, removal_queued):
        follow_the_action_the_verb_queued(
            tatolabd, verb_standard_output, timeout=RE_LOAD_OF_SEVEN_STREAMS_TIMEOUT_SECONDS
        )

    assert read_stream_record(tatolabd, stopped_stream_id)["stopped"] is True
    assert not stream_record_path(tatolabd.machine_directories, removed_stream_id).exists()

    def listing_of_the_recorded(stream_function_name: str, state: str, node_count: "int | None") -> "dict[str, Any]":
        return listing_of(
            stream_id_of[stream_function_name],
            stream_function_name,
            state,
            Path(records_by_stream_function[stream_function_name]["location"]["project_directory"]),
            node_count,
        )

    stream_functions_running = STREAM_FUNCTIONS_RE_LOADED_AHEAD
    assert listed_streams_by_id(local_api) == {
        **{
            stream_id_of[stream_function_name]: listing_of_the_recorded(
                stream_function_name, "running", PROJECT_STREAM_NODE_COUNT
            )
            for stream_function_name in stream_functions_running
        },
        stopped_stream_id: listing_of_the_recorded(
            STOPPED_WHILE_RE_LOADING_STREAM_FUNCTION, "stopped", PROJECT_STREAM_NODE_COUNT
        ),
    }, f"a re-load had already loaded one of them when the owner acted: {re_load_reached_them_first}"
    assert stream_ids_in_the_machine_graph(local_api) == sorted(
        [
            *(stream_id_of[stream_function_name] for stream_function_name in stream_functions_running),
            stopped_stream_id,
        ]
    )
    stop_tatolabd_cleanly(tatolabd)

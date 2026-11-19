# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The integration suite's fixtures that start `tatolabd` and `tatolab`, and run
streams on them; `conftest.py` registers them and says how a test uses them."""

from __future__ import annotations

import subprocess
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

import pytest

from conftest import (
    NO_VULKAN_DRIVER_REFUSAL,
    STREAM_UNLOAD_REMOVED_NODES_LOG_LINE_PATTERN,
    TATOLAB_RUN_TO_COMPLETION_TIMEOUT_SECONDS,
    PrivateMachineDirectories,
    StartedRuntimeProcesses,
    StreamRunWithNoVulkanDriverOutcome,
    TatolabCommandsOfThisTest,
    TatolabdUnderTest,
    environment_overlaid_with,
    environment_reaching_no_vulkan_driver,
)
from runtime_process_under_test import (
    ENGINE_STARTED_LOG_LINE,
    STREAM_START_BEGAN_LOG_LINE_PATTERN,
    RuntimeProcessUnderTest,
)
from runtime_unit_under_test import (
    RuntimeUnitUnderTest,
    run_tatolab_verb_in_environment,
)
from stream_runs_on_tatolabd import StreamOrGraph


@pytest.fixture
def started_runtime_processes() -> "Iterator[StartedRuntimeProcesses]":
    """The list every started `tatolabd` and `tatolab` joins, reaped however the test ends.

    Without the reaping a failed assertion strands a live engine holding a GPU
    context, a device, an iceoryx2 node and a socket, contaminating every later
    run on the rig.
    """
    started_runtime_processes = StartedRuntimeProcesses(started=[])
    try:
        yield started_runtime_processes
    finally:
        started_runtime_processes.kill_every_one()


@pytest.fixture
def tatolab_commands_of_this_test(
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
    started_runtime_processes: StartedRuntimeProcesses,
    tmp_path: Path,
) -> TatolabCommandsOfThisTest:
    """What `start_tatolab` and every `TatolabdUnderTest` start `tatolab` through."""
    return TatolabCommandsOfThisTest(
        runtime_unit=runtime_unit,
        private_machine_directories=private_machine_directories,
        started_runtime_processes=started_runtime_processes,
        scratch_directory=tmp_path,
    )


@pytest.fixture
def start_tatolabd(
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
    started_runtime_processes: StartedRuntimeProcesses,
    tatolab_commands_of_this_test: TatolabCommandsOfThisTest,
    tmp_path: Path,
) -> "Callable[..., TatolabdUnderTest]":
    """Start `tatolabd` under this test's machine root, in a session of its own.

    `start_tatolabd(*, extra_environment=None, working_directory=None,
    wait_until_serving=True) -> TatolabdUnderTest`:
    - `extra_environment` is laid over the test's environment, a `None` value
      unsetting that variable; every stream's node interpreters inherit it;
    - `working_directory` defaults to a fresh directory of the test's own;
    - with `wait_until_serving`, returns once the local API answers, and a
      `tatolabd` that exits first fails the test. A second `tatolabd` while one
      runs is refused by the machine lock — pass `wait_until_serving=False` to
      observe that refusal.
    """
    start_count = 0

    def start(
        *,
        extra_environment: "dict[str, str | None] | None" = None,
        working_directory: "Path | None" = None,
        wait_until_serving: bool = True,
    ) -> TatolabdUnderTest:
        nonlocal start_count
        start_count += 1
        if working_directory is None:
            working_directory = tmp_path / f"tatolabd-working-directory-{start_count}"
            working_directory.mkdir()
        process = subprocess.Popen(
            [str(runtime_unit.tatolabd_executable)],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            errors="replace",
            bufsize=1,
            start_new_session=True,
            cwd=working_directory,
            env=environment_overlaid_with(private_machine_directories.environment, extra_environment),
        )
        started_tatolabd = TatolabdUnderTest(process, tatolab_commands=tatolab_commands_of_this_test)
        started_runtime_processes.started.append(started_tatolabd)
        if wait_until_serving:
            started_tatolabd.await_serving()
        return started_tatolabd

    return start


@pytest.fixture
def start_tatolabd_running_stream(
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
) -> "Callable[..., TatolabdUnderTest]":
    """Start this test's `tatolabd`, then run `stream_or_graph` on it with `tatolab open --play`.

    `start_tatolabd_running_stream(stream_or_graph, *,
    project_directory=None, node_interpreter=None, extra_environment=None,
    working_directory=None) -> TatolabdUnderTest`. `extra_environment` and
    `working_directory` are `tatolabd`'s, so the stream's node interpreters
    inherit the environment. Returns as soon as `tatolab open --play` is started, its
    load not awaited: the run is `tatolabd.stream_runs[-1]`, and
    `await_loaded()` on it waits for the load, while a refused load ends that
    run with its refusal and leaves `tatolabd` serving.
    """

    def start(
        stream_or_graph: StreamOrGraph,
        *,
        project_directory: "Path | None" = None,
        node_interpreter: "Path | None" = None,
        extra_environment: "dict[str, str | None] | None" = None,
        working_directory: "Path | None" = None,
    ) -> TatolabdUnderTest:
        started_tatolabd = start_tatolabd(
            extra_environment=extra_environment, working_directory=working_directory
        )
        started_tatolabd.run_stream(
            stream_or_graph,
            project_directory=project_directory,
            node_interpreter=node_interpreter,
        )
        return started_tatolabd

    return start


@pytest.fixture
def run_stream_on_tatolabd_with_no_vulkan_driver(
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    tatolab_commands_of_this_test: TatolabCommandsOfThisTest,
    tmp_path: Path,
) -> "Callable[..., StreamRunWithNoVulkanDriverOutcome]":
    """Run a stream on a `tatolabd` that reaches no Vulkan driver, to the end of the run.

    `run_stream_on_tatolabd_with_no_vulkan_driver(stream_or_graph, *,
    project_directory=None, node_interpreter=None, extra_environment=None) -> StreamRunWithNoVulkanDriverOutcome`, its load
    followed to its end. Needs no device: a graph the compile or the load
    refuses ends the load with its stream failed and the reason; one it loads
    fails at its start, at the GPU, before any device opens. The runtime
    outlives either: `tatolabd` still serves, holding at most the run's own
    streams, recorded failed, whose projects are removed so the next run starts
    from nothing — anything else fails the test, as does a loaded stream whose
    load ended any other way, so a driver override that did not take cannot
    start a stream's devices unseen.

    One `tatolabd` serves every call in a test; a call with a different
    `extra_environment` (`tatolabd`'s) interrupts it and starts another.
    """
    tatolabd_and_its_extra_environment: "list[tuple[TatolabdUnderTest, dict[str, str | None]]]" = []

    def the_tatolabd_with_no_vulkan_driver(
        extra_environment: "dict[str, str | None]",
    ) -> TatolabdUnderTest:
        if tatolabd_and_its_extra_environment:
            running_tatolabd, its_extra_environment = tatolabd_and_its_extra_environment[-1]
            if its_extra_environment == extra_environment and running_tatolabd.process.poll() is None:
                return running_tatolabd
            running_tatolabd.interrupt()
            running_tatolabd.await_exit()
        started_tatolabd = start_tatolabd(extra_environment=extra_environment)
        tatolabd_and_its_extra_environment.append((started_tatolabd, extra_environment))
        return started_tatolabd

    def run(
        stream_or_graph: StreamOrGraph,
        *,
        project_directory: "Path | None" = None,
        node_interpreter: "Path | None" = None,
        extra_environment: "dict[str, str | None] | None" = None,
    ) -> StreamRunWithNoVulkanDriverOutcome:
        tatolabd = the_tatolabd_with_no_vulkan_driver(
            {**environment_reaching_no_vulkan_driver(tmp_path), **(extra_environment or {})}
        )
        tatolab_run = tatolab_commands_of_this_test.tatolab_run_of(
            stream_or_graph,
            project_directory=project_directory,
            node_interpreter=node_interpreter,
        )
        tatolabd_stderr_mark = tatolabd.stderr_line_count()
        stream_run = tatolabd.run_stream(tatolab_run)
        exit_status = stream_run.await_exit(timeout=TATOLAB_RUN_TO_COMPLETION_TIMEOUT_SECONDS)
        local_api = tatolabd.local_api_client()
        failed_or_refused_because: "str | None" = None
        ids_of_the_streams_the_run_took_in: "set[str]" = set()
        if exit_status != 0:
            failed_or_refused_because = stream_run.refusal()
        else:
            ended_actions = stream_run.actions_once_ended(
                timeout=TATOLAB_RUN_TO_COMPLETION_TIMEOUT_SECONDS
            )
            failed_stream = stream_run.failed_stream()
            ids_of_the_streams_the_run_took_in = {
                stream["stream"]
                for ended_action in ended_actions
                for stream in ended_action["streams"]
            }
            failed_action = next(
                (ended_action for ended_action in ended_actions if ended_action["state"] == "failed"),
                None,
            )
            if failed_stream is not None:
                failed_or_refused_because = failed_stream[1]
            elif failed_action is not None:
                failed_or_refused_because = failed_action["failed_because"]
            elif not ids_of_the_streams_the_run_took_in:
                failed_or_refused_because = f"the load took in no stream: {ended_actions}"
        listed_streams = local_api.list_streams()
        assert all(
            listed["state"] == "failed" and listed["stream"] in ids_of_the_streams_the_run_took_in
            for listed in listed_streams
        ), f"`{stream_run.command_description}` left the runtime holding {listed_streams}"
        graphs_of_the_streams_it_took_in = {
            listed["stream"]: local_api.graph(listed["stream"]) for listed in listed_streams
        }
        for project_directory_of_a_failed_stream in sorted(
            {listed["location"]["project_directory"] for listed in listed_streams if listed["location"]}
        ):
            removal = local_api.call_tool(
                "remove_project", {"project_directory": project_directory_of_a_failed_stream}
            )
            local_api.stream_action_once_ended(removal["action"])
        listed_streams = local_api.list_streams()
        tatolabd_stderr_text = tatolabd.stderr_text_since(tatolabd_stderr_mark)
        start_began = STREAM_START_BEGAN_LOG_LINE_PATTERN.search(tatolabd_stderr_text)
        unload = STREAM_UNLOAD_REMOVED_NODES_LOG_LINE_PATTERN.search(tatolabd_stderr_text)
        assert listed_streams == [], (
            f"`{stream_run.command_description}` ended {exit_status} and left the runtime "
            f"holding {listed_streams}; standard error:\n{stream_run.recent_stderr()}"
        )
        if start_began is not None:
            assert ENGINE_STARTED_LOG_LINE not in tatolabd_stderr_text, (
                f"the engine started a stream that was to end at the GPU:\n{tatolabd.recent_stderr()}"
            )
            assert (
                failed_or_refused_because is not None
                and NO_VULKAN_DRIVER_REFUSAL in failed_or_refused_because
            ), (
                f"a loaded stream was to be refused for the missing Vulkan driver; "
                f"`{stream_run.command_description}` ended {exit_status}, the stream "
                f"{failed_or_refused_because!r}:\n{stream_run.recent_stderr()}\ntatolabd:\n"
                f"{tatolabd.recent_stderr()}"
            )
        return StreamRunWithNoVulkanDriverOutcome(
            loaded=start_began is not None,
            loaded_stream_id=start_began.group("stream_id") if start_began else None,
            loaded_node_count=(
                int(unload.group("node_count")) if start_began and unload else None
            ),
            failed_or_refused=failed_or_refused_because is not None,
            failed_or_refused_because=failed_or_refused_because,
            tatolab_run_stderr_text=stream_run.stderr_text,
            tatolabd_stderr_text_during_the_run=tatolabd_stderr_text,
            tatolabd=tatolabd,
            tatolab_run=tatolab_run,
            graphs_of_the_streams_it_took_in=graphs_of_the_streams_it_took_in,
        )

    return run


@pytest.fixture
def start_tatolab(
    tatolab_commands_of_this_test: TatolabCommandsOfThisTest,
) -> "Callable[..., RuntimeProcessUnderTest]":
    """Start `tatolab <arguments...>` in a session of its own; returns its `RuntimeProcessUnderTest`.

    `start_tatolab(*arguments, working_directory, extra_environment=None)`, in
    this test's machine environment: every verb reaches the runtime this test
    started with `start_tatolabd`, or refuses naming the socket nothing answers
    at.
    """

    def start(
        *arguments: "str | Path",
        working_directory: Path,
        extra_environment: "dict[str, str | None] | None" = None,
    ) -> RuntimeProcessUnderTest:
        return tatolab_commands_of_this_test.start_tatolab(
            *arguments, working_directory=working_directory, extra_environment=extra_environment
        )

    return start


@pytest.fixture
def run_tatolab(
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
) -> "Callable[..., subprocess.CompletedProcess[str]]":
    """Run `tatolab <arguments...>` to completion; returns the completed process, output captured.

    `run_tatolab(*arguments, working_directory, extra_environment=None, timeout=120.0)`.
    Started through `start_tatolab`, so a run that outlives `timeout` fails the
    test and is killed with it.
    """

    def run(
        *arguments: "str | Path",
        working_directory: Path,
        extra_environment: "dict[str, str | None] | None" = None,
        timeout: float = TATOLAB_RUN_TO_COMPLETION_TIMEOUT_SECONDS,
    ) -> "subprocess.CompletedProcess[str]":
        started_tatolab = start_tatolab(
            *arguments, working_directory=working_directory, extra_environment=extra_environment
        )
        exit_status = started_tatolab.await_exit(timeout=timeout)
        return subprocess.CompletedProcess(
            args=started_tatolab.process.args,
            returncode=exit_status,
            stdout=started_tatolab.stdout_text,
            stderr=started_tatolab.stderr_text,
        )

    return run


@pytest.fixture
def run_tatolab_observation_verb(
    runtime_unit: RuntimeUnitUnderTest, private_machine_directories: PrivateMachineDirectories
) -> "Callable[..., subprocess.CompletedProcess[str]]":
    """Run a `tatolab` verb (`graph`, `streams`, `logs`, ...) against this test's runtime.

    `run_tatolab_observation_verb(*verb_arguments, timeout=60.0)`: the runtime
    unit's `bin/tatolab`, in this test's machine environment, to completion.
    """

    def run(*verb_arguments: str, **keyword_arguments: Any) -> "subprocess.CompletedProcess[str]":
        return run_tatolab_verb_in_environment(
            runtime_unit,
            private_machine_directories.environment,
            *verb_arguments,
            **keyword_arguments,
        )

    return run

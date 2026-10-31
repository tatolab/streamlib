# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The runtime makes a project's `.venv` with the uv its runtime unit carries.

A project with no `.venv` is loaded over the local API by a `tatolabd` whose
`PATH` holds no uv, and whose environment points uv elsewhere: the runtime
unit's own `bin/uv` makes the venv, from the project's `pyproject.toml` and
`.python-version`. The project's one `@stream` raises, so the compile runs in
the venv uv made and answers its traceback, and no stream starts: needs no GPU.
A load whose sync waits on a package index that never answers is cancelled with
`tatolab cancel`: uv is killed, no `.venv` is left, and the action ends
`cancelled`.
"""

from __future__ import annotations

import os
import shutil
import socket
import subprocess
import sys
import threading
import time
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest

from conftest import TatolabdUnderTest
from local_api_client import LocalApiClient
from runtime_unit_under_test import REPOSITORY_ROOT, RuntimeUnitUnderTest

VENV_INTERPRETER_PROBE_TIMEOUT_SECONDS = 30.0

RAISED_ON_PURPOSE = "the stream function raised on purpose"

STREAM_PY = f'''from tatolab.stream import StreamBuilder, stream


@stream
def main(stream_builder: StreamBuilder) -> None:
    raise ValueError("{RAISED_ON_PURPOSE}")
'''


def a_project_with_no_venv(project_directory: Path) -> Path:
    """A project depending on this checkout's `tatolab-stream`, pinned to this
    interpreter's Python, with no `.venv`."""
    project_directory.mkdir()
    (project_directory / "pyproject.toml").write_text(
        "[project]\n"
        'name = "venv-made-by-the-runtime-unit-uv"\n'
        'version = "0"\n'
        'requires-python = ">=3.10"\n'
        'dependencies = ["tatolab-stream"]\n'
        "\n"
        "[tool.uv]\n"
        "package = false\n"
        "\n"
        "[tool.uv.sources]\n"
        f'tatolab-stream = {{ path = "{REPOSITORY_ROOT / "sdk" / "tatolab-stream"}" }}\n',
        encoding="utf-8",
    )
    (project_directory / ".python-version").write_text(
        f"{sys.version_info.major}.{sys.version_info.minor}\n", encoding="utf-8"
    )
    (project_directory / "stream.py").write_text(STREAM_PY, encoding="utf-8")
    return project_directory


def path_without_any_uv() -> str:
    """This process's `PATH`, less every directory holding a `uv`."""
    return os.pathsep.join(
        directory
        for directory in os.environ.get("PATH", "").split(os.pathsep)
        if directory and not (Path(directory) / "uv").exists()
    )


def assert_uv_made_a_venv_whose_interpreter_runs(project_directory: Path) -> None:
    venv_configuration = (project_directory / ".venv" / "pyvenv.cfg").read_text(encoding="utf-8")
    assert "\nuv = " in venv_configuration, venv_configuration
    interpreter = project_directory / ".venv" / "bin" / "python"
    probed = subprocess.run(
        [str(interpreter), "-I", "-c", "import tatolab.stream"],
        capture_output=True,
        text=True,
        timeout=VENV_INTERPRETER_PROBE_TIMEOUT_SECONDS,
        check=False,
    )
    assert probed.returncode == 0, probed.stdout + probed.stderr


def test_a_project_with_no_venv_gets_one_from_the_runtime_unit_uv_on_a_load_and_on_a_start(
    runtime_unit: RuntimeUnitUnderTest,
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    tmp_path: Path,
):
    assert (runtime_unit.runtime_unit_directory / "bin" / "uv").is_file(), (
        "the runtime unit carries no bin/uv"
    )
    project_directory = a_project_with_no_venv(tmp_path / "project")
    tatolabd = start_tatolabd(
        extra_environment={
            "PATH": path_without_any_uv(),
            "VIRTUAL_ENV": str(tmp_path / "another-venv"),
            "UV_PROJECT_ENVIRONMENT": str(tmp_path / "another-venv"),
        }
    )
    local_api = tatolabd.local_api_client()

    queued_load = local_api.call_tool("run_project", {"project_directory": str(project_directory)})
    load_once_ended = local_api.stream_action_once_ended(queued_load["action"])

    assert load_once_ended["state"] == "done", load_once_ended
    [failed_stream] = load_once_ended["streams"]
    assert failed_stream["state"] == "failed", load_once_ended
    [listed_failed] = local_api.list_streams()
    assert RAISED_ON_PURPOSE in listed_failed["failed_because"], listed_failed
    assert_uv_made_a_venv_whose_interpreter_runs(project_directory)
    assert not (tmp_path / "another-venv").exists(), "uv made the venv VIRTUAL_ENV names"

    shutil.rmtree(project_directory / ".venv")
    queued_start = local_api.call_tool("start_stream", {"stream": failed_stream["stream"]})
    start_once_ended = local_api.stream_action_once_ended(queued_start["action"])

    assert start_once_ended["state"] == "done", start_once_ended
    assert [stream["state"] for stream in start_once_ended["streams"]] == ["failed"], (
        start_once_ended
    )
    [listed] = local_api.list_streams()
    assert RAISED_ON_PURPOSE in listed["failed_because"], listed
    assert_uv_made_a_venv_whose_interpreter_runs(project_directory)


#: What the lines `uv sync` writes are logged under, as `processor_id`.
PROJECT_VENV_UV_SYNC_LOG_LABEL = "project-venv-uv-sync"


def test_a_run_project_answers_at_once_and_its_action_makes_the_venv_compiles_and_logs_by_action(
    runtime_unit: RuntimeUnitUnderTest,
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    tmp_path: Path,
):
    project_directory = a_project_with_no_venv(tmp_path / "project")
    tatolabd = start_tatolabd(extra_environment={"PATH": path_without_any_uv()})
    local_api = tatolabd.local_api_client()

    queued = local_api.call_tool("run_project", {"project_directory": str(project_directory)})
    action = queued["action"]
    listed_while_it_runs = local_api.call_tool("list_streams", {})

    assert queued["project_directory"] == str(project_directory.resolve()), queued
    [preparing] = listed_while_it_runs["streams"]
    assert preparing["state"] == "preparing", listed_while_it_runs
    assert preparing["action"] == action, listed_while_it_runs

    described = local_api.stream_action_once_ended(action)

    assert described["state"] == "done", described
    [failed_stream] = described["streams"]
    assert failed_stream["state"] == "failed", described
    assert_uv_made_a_venv_whose_interpreter_runs(project_directory)
    [listed] = local_api.call_tool("list_streams", {})["streams"]
    assert listed["state"] == "failed" and RAISED_ON_PURPOSE in listed["failed_because"], listed
    assert listed["action"] is None and listed["step"] is None, listed
    project_streams = local_api.call_tool(
        "list_project_streams", {"project_directory": str(project_directory)}
    )
    assert [stream["stream_function"] for stream in project_streams["streams"]] == [
        "stream:main"
    ], project_streams

    records_of_the_action = local_api.call_tool(
        "logs", {"project_directory": str(project_directory), "action": action, "count": 4096}
    )["records"]
    assert records_of_the_action, "the action wrote no record"
    assert all(record["action"] == action for record in records_of_the_action), (
        records_of_the_action
    )
    assert any(
        record.get("processor_id") == PROJECT_VENV_UV_SYNC_LOG_LABEL
        for record in records_of_the_action
    ), records_of_the_action
    assert any(
        record.get("stream") == failed_stream["stream"] and RAISED_ON_PURPOSE in record["message"]
        for record in records_of_the_action
    ), records_of_the_action


#: How long a cancelled sync may take to be killed and its action to end.
A_CANCELLED_SYNC_ENDS_WITHIN_SECONDS = 15.0

#: How long the test waits for uv to ask the package index for the project's dependency.
UV_ASKS_THE_INDEX_WITHIN_SECONDS = 120.0


class PackageIndexThatNeverAnswers:
    """A loopback HTTP listener that takes every connection and answers none of them, so a
    `uv sync` against it waits on its first request."""

    def __init__(self) -> None:
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen()
        self.asked = threading.Event()
        self.connections_held: "list[socket.socket]" = []
        threading.Thread(target=self._hold_every_connection, daemon=True).start()

    @property
    def simple_index_url(self) -> str:
        return f"http://127.0.0.1:{self.listener.getsockname()[1]}/simple"

    def _hold_every_connection(self) -> None:
        while True:
            try:
                connection, _ = self.listener.accept()
            except OSError:
                return
            self.connections_held.append(connection)
            self.asked.set()

    def close(self) -> None:
        self.listener.close()
        for connection in self.connections_held:
            connection.close()


@pytest.fixture
def package_index_that_never_answers() -> "Iterator[PackageIndexThatNeverAnswers]":
    index = PackageIndexThatNeverAnswers()
    try:
        yield index
    finally:
        index.close()


def a_project_whose_sync_waits_on(index: PackageIndexThatNeverAnswers, project_directory: Path) -> Path:
    """A project with no `.venv` whose one dependency is resolved from `index` alone."""
    project_directory.mkdir()
    (project_directory / "pyproject.toml").write_text(
        "[project]\n"
        'name = "venv-sync-cancelled"\n'
        'version = "0"\n'
        'requires-python = ">=3.10"\n'
        'dependencies = ["a-package-only-the-index-that-never-answers-has"]\n'
        "\n"
        "[tool.uv]\n"
        "package = false\n"
        "\n"
        "[[tool.uv.index]]\n"
        'name = "never-answers"\n'
        f'url = "{index.simple_index_url}"\n'
        "default = true\n",
        encoding="utf-8",
    )
    (project_directory / ".python-version").write_text(
        f"{sys.version_info.major}.{sys.version_info.minor}\n", encoding="utf-8"
    )
    (project_directory / "stream.py").write_text(STREAM_PY, encoding="utf-8")
    return project_directory


def the_step_once_uv_asked_the_index(
    local_api: LocalApiClient, action: str, index: PackageIndexThatNeverAnswers
) -> "dict[str, object]":
    """`describe_stream_action` of `action` once uv's sync has asked `index`."""
    assert index.asked.wait(UV_ASKS_THE_INDEX_WITHIN_SECONDS), "uv never asked the package index"
    return local_api.call_tool("describe_stream_action", {"action": action})


def test_tatolab_cancel_during_a_sync_kills_uv_and_the_load_ends_cancelled_with_no_venv(
    runtime_unit: RuntimeUnitUnderTest,
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    run_tatolab_observation_verb: "Callable[..., subprocess.CompletedProcess[str]]",
    package_index_that_never_answers: PackageIndexThatNeverAnswers,
    tmp_path: Path,
):
    project_directory = a_project_whose_sync_waits_on(
        package_index_that_never_answers, tmp_path / "project"
    )
    tatolabd = start_tatolabd(
        extra_environment={"PATH": path_without_any_uv(), "UV_HTTP_TIMEOUT": "600"}
    )
    local_api = tatolabd.local_api_client()
    action = local_api.call_tool("run_project", {"project_directory": str(project_directory)})[
        "action"
    ]
    while_uv_waits = the_step_once_uv_asked_the_index(
        local_api, action, package_index_that_never_answers
    )
    assert (while_uv_waits["state"], while_uv_waits["step"]) == (
        "in_progress",
        "making_the_venv",
    ), while_uv_waits

    cancelled_at = time.monotonic()
    cancel = run_tatolab_observation_verb("cancel", action)
    ended = local_api.stream_action_once_ended(action, timeout=A_CANCELLED_SYNC_ENDS_WITHIN_SECONDS)

    assert cancel.returncode == 0, cancel.stderr
    assert cancel.stdout == (
        f"{action} cancelling: it stops at the step it is on, then ends cancelled\n"
    ), cancel.stdout
    assert (ended["state"], ended["step"], ended["failed_because"]) == (
        "cancelled",
        None,
        None,
    ), ended
    assert time.monotonic() - cancelled_at < A_CANCELLED_SYNC_ENDS_WITHIN_SECONDS
    assert not (project_directory / ".venv").exists(), "the half-made venv was left"
    assert local_api.list_streams() == [], "a cancelled load recorded a stream"
    again = run_tatolab_observation_verb("cancel", action)
    assert again.returncode == 1, again.stdout
    assert "is already cancelled" in again.stderr, again.stderr

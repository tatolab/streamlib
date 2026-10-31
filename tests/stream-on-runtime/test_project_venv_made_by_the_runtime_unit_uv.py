# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The runtime makes a project's `.venv` with the uv its runtime unit carries.

A project with no `.venv` is loaded over the local API by a `tatolabd` whose
`PATH` holds no uv, and whose environment points uv elsewhere: the runtime
unit's own `bin/uv` makes the venv, from the project's `pyproject.toml` and
`.python-version`. The project's one `@stream` raises, so the compile runs in
the venv uv made and answers its traceback, and no stream starts: needs no GPU.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path

from conftest import TatolabdUnderTest
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

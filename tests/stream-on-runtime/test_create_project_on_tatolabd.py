# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`create_project` end to end: an MCP host creates a project through `tatolab mcp`.

The runtime writes the scaffold `tatolab new` writes, loads its `stream:main`
under the folder's name, and records it, so a later connection finds it in
`list_streams`. A second `@stream` that raises is listed failed beside the
running one, with its traceback in the project's log, and a start of a stream
whose `.venv` was deleted has the runtime unit's uv make it again. With no
directory, the project is created in a folder `<adjective>-<noun>` drawn under
the machine's streams root. `tatolabd`'s `PATH` holds no uv.
"""

from __future__ import annotations

import asyncio
import json
import re
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest
from mcp.client.client import Client
from mcp.client.stdio import StdioServerParameters
from mcp.types import TextContent

from conftest import PrivateMachineDirectories, TatolabdUnderTest
from runtime_unit_under_test import REPOSITORY_ROOT, RuntimeUnitUnderTest
from test_project_venv_made_by_the_runtime_unit_uv import (
    assert_uv_made_a_venv_whose_interpreter_runs,
    path_without_any_uv,
)

pytestmark = pytest.mark.requires_gpu

MCP_SESSION_TIMEOUT_SECONDS = 60.0
SCAFFOLD_WRITTEN_TIMEOUT_SECONDS = 60.0

RAISED_ON_PURPOSE = "the second stream raised on purpose"

SECOND_STREAM_THAT_RAISES = f'''

@stream
def broken(stream_builder: StreamBuilder) -> None:
    raise ValueError("{RAISED_ON_PURPOSE}")
'''

FILES_A_NEW_PROJECT_STARTS_WITH = (
    "stream.py",
    "nodes/__init__.py",
    "nodes/halftone_lens_effect.py",
    "nodes/brightness_meter.py",
    "pyproject.toml",
    ".python-version",
    ".gitignore",
)

DRAWN_PROJECT_FOLDER_NAME = re.compile(r"[a-z]+-[a-z]+")


def create_project_through_tatolab_mcp(
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
    arguments: "dict[str, Any]",
) -> "dict[str, Any]":
    """`create_project` with `arguments`, over a `tatolab mcp` session of its own."""
    verb = StdioServerParameters(
        command=str(runtime_unit.tatolab_executable),
        args=["mcp"],
        env=private_machine_directories.environment,
    )

    async def create_through_the_verb() -> "dict[str, Any]":
        async with Client(verb) as client:
            result = await client.call_tool("create_project", arguments)
            assert result.is_error is False, result.content
            stated = result.content[0]
            assert isinstance(stated, TextContent), result.content
            return json.loads(stated.text)

    return asyncio.run(asyncio.wait_for(create_through_the_verb(), MCP_SESSION_TIMEOUT_SECONDS))


def point_tatolab_stream_at_this_checkout(project_directory: Path) -> None:
    """Have uv take `tatolab-stream` from this checkout, as the suite tests it."""
    with (project_directory / "pyproject.toml").open("a", encoding="utf-8") as pyproject:
        pyproject.write(
            "\n[tool.uv.sources]\n"
            f'tatolab-stream = {{ path = "{REPOSITORY_ROOT / "sdk" / "tatolab-stream"}" }}\n'
        )


def test_create_project_through_tatolab_mcp_runs_the_scaffold_under_its_folder_and_outlives_the_connection(
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    project_directory = make_tatolab_project(directory_name="bright-lens")
    resolved_project_directory = project_directory.resolve()
    tatolabd = start_tatolabd(extra_environment={"PATH": path_without_any_uv()})

    created = create_project_through_tatolab_mcp(
        runtime_unit, private_machine_directories, {"project_directory": str(project_directory)}
    )

    assert Path(created["project_directory"]) == resolved_project_directory, created
    local_api = tatolabd.local_api_client()
    create_once_ended = local_api.stream_action_once_ended(created["action"])
    assert create_once_ended["state"] == "done", create_once_ended
    assert create_once_ended["streams"] == [
        {"stream": created["stream"], "display_name": "bright-lens", "state": "running"}
    ], create_once_ended
    for file_a_new_project_starts_with in FILES_A_NEW_PROJECT_STARTS_WITH:
        assert (project_directory / file_a_new_project_starts_with).is_file(), (
            file_a_new_project_starts_with
        )

    [listed] = local_api.list_streams()
    assert listed["stream"] == created["stream"], listed
    assert listed["display_name"] == "bright-lens", listed
    assert listed["state"] == "running", listed
    assert listed["location"] == {
        "project_directory": str(resolved_project_directory),
        "stream_function": "stream:main",
    }, listed

    with (project_directory / "stream.py").open("a", encoding="utf-8") as stream_py:
        stream_py.write(SECOND_STREAM_THAT_RAISES)
    queued_run = local_api.call_tool("run_project", {"project_directory": str(project_directory)})
    run_once_ended = local_api.stream_action_once_ended(queued_run["action"])

    assert run_once_ended["state"] == "done", run_once_ended
    states_by_stream = {stream["stream"]: stream["state"] for stream in run_once_ended["streams"]}
    assert states_by_stream.pop(created["stream"]) == "running", run_once_ended
    [(broken_stream, broken_state)] = states_by_stream.items()
    assert broken_state == "failed", run_once_ended
    project_log = local_api.call_tool(
        "logs", {"project_directory": str(project_directory), "stream": broken_stream}
    )
    assert any(RAISED_ON_PURPOSE in json.dumps(record) for record in project_log["records"]), (
        project_log
    )

    queued_stop = local_api.call_tool("stop_stream", {"stream": created["stream"]})
    assert local_api.stream_action_once_ended(queued_stop["action"])["state"] == "done"
    (project_directory / ".venv").unlink()
    point_tatolab_stream_at_this_checkout(project_directory)
    queued_start = local_api.call_tool("start_stream", {"stream": created["stream"]})
    start_once_ended = local_api.stream_action_once_ended(queued_start["action"])

    assert start_once_ended["state"] == "done", start_once_ended
    assert [stream["state"] for stream in start_once_ended["streams"]] == ["running"], (
        start_once_ended
    )
    assert_uv_made_a_venv_whose_interpreter_runs(project_directory)


def test_create_project_with_no_directory_draws_an_adjective_noun_folder_under_the_streams_root(
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd(extra_environment={"PATH": path_without_any_uv()})

    created = create_project_through_tatolab_mcp(runtime_unit, private_machine_directories, {})

    project_directory = Path(created["project_directory"])
    streams_root = (private_machine_directories.machine_directories.root / "streams").resolve()
    assert project_directory.parent == streams_root, created
    assert DRAWN_PROJECT_FOLDER_NAME.fullmatch(project_directory.name), created
    deadline = time.monotonic() + SCAFFOLD_WRITTEN_TIMEOUT_SECONDS
    while not (project_directory / ".gitignore").is_file():
        assert time.monotonic() < deadline, f"no scaffold was written into {project_directory}"
        time.sleep(0.1)
    for file_a_new_project_starts_with in FILES_A_NEW_PROJECT_STARTS_WITH:
        assert (project_directory / file_a_new_project_starts_with).is_file(), (
            file_a_new_project_starts_with
        )
    local_api = tatolabd.local_api_client()
    local_api.answer_over_mcp(
        lambda client: client.call_tool("cancel_stream_action", {"action": created["action"]})
    )
    create_once_ended = local_api.stream_action_once_ended(created["action"])
    assert create_once_ended["state"] in ("cancelled", "done"), create_once_ended

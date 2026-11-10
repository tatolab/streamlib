# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`create_project` end to end: an MCP host creates a project through `tatolab mcp`.

The runtime writes the scaffold `tatolab new` writes, loads its graph document
`streams/main.graph.json` under the folder's name, and records it, so a later
connection finds it in `list_streams`. A second graph document whose node module
raises is listed failed beside the running one, with its traceback in the
project's log, and a start of a stream
whose `.venv` was deleted has the runtime unit's uv make it again. With no
directory, the project is created in a folder `<adjective>-<noun>` drawn under
the machine's streams root. `tatolabd`'s `PATH` holds no uv.
"""

from __future__ import annotations

import asyncio
import json
import re
import shutil
import time
import tomllib
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest
from mcp.client.client import Client
from mcp.client.stdio import StdioServerParameters
from mcp.types import TextContent

from conftest import PrivateMachineDirectories, TatolabdUnderTest
from runtime_unit_under_test import REPOSITORY_ROOT, RuntimeUnitUnderTest
from stream_runs_on_tatolabd import (
    PROJECT_FILE_NAME,
    graph_document_path_of,
    stream_asset_held_at,
    write_a_graph_document,
)
from test_project_venv_made_by_the_runtime_unit_uv import (
    assert_uv_made_a_venv_whose_interpreter_runs,
    path_without_any_uv,
)

pytestmark = pytest.mark.requires_gpu

MCP_SESSION_TIMEOUT_SECONDS = 60.0
SCAFFOLD_WRITTEN_TIMEOUT_SECONDS = 60.0

RAISED_ON_PURPOSE = "the second stream's node module raised on purpose"

NODE_MODULE_THAT_RAISES_AS_IT_IMPORTS = f'raise ValueError("{RAISED_ON_PURPOSE}")\n'

SECOND_STREAM_WHOSE_NODE_MODULE_RAISES = {
    "nodes": [{"name": "raiser", "type": "nodes.raising_node:RaisingNode", "config": {}}],
    "links": [],
}

FILES_A_NEW_PROJECT_STARTS_WITH = (
    "tatolab-project.json",
    "streams/main.graph.json",
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


UV_SOURCES_TABLE_HEADER = "[tool.uv.sources]"


def point_the_scaffolds_tatolab_distributions_at_this_checkout(project_directory: Path) -> None:
    """Have uv take `tatolab-stream` from this checkout, as the suite tests it, in the one
    `[tool.uv.sources]` table the project holds; its first-party modules stay the wheels the
    runtime unit carries."""
    checkout_path_of_each_distribution = {
        "tatolab-stream": str(REPOSITORY_ROOT / "sdk" / "tatolab-stream"),
    }
    pyproject_path = project_directory / "pyproject.toml"
    pyproject_lines = [
        line
        for line in pyproject_path.read_text(encoding="utf-8").splitlines()
        if line.partition("=")[0].strip() not in checkout_path_of_each_distribution
    ]
    if UV_SOURCES_TABLE_HEADER not in pyproject_lines:
        pyproject_lines += ["", UV_SOURCES_TABLE_HEADER]
    after_the_header = pyproject_lines.index(UV_SOURCES_TABLE_HEADER) + 1
    pyproject_lines[after_the_header:after_the_header] = [
        f'{distribution} = {{ path = "{checkout_path}" }}'
        for distribution, checkout_path in checkout_path_of_each_distribution.items()
    ]
    pyproject_path.write_text("\n".join(pyproject_lines) + "\n", encoding="utf-8")
    uv_sources = tomllib.loads(pyproject_path.read_text(encoding="utf-8"))["tool"]["uv"]["sources"]
    assert {
        distribution: uv_sources[distribution]["path"]
        for distribution in checkout_path_of_each_distribution
    } == checkout_path_of_each_distribution, uv_sources


FIRST_PARTY_MODULE_LINKS_DIRECTORY = ".tatolab/first-party-imports"


def assert_every_first_party_module_came_from_the_runtime_unit(project_directory: Path) -> None:
    """The project names no index, and its test-pattern and H.264 modules are path sources
    through links to the wheels the runtime unit carries, installed in its `.venv`."""
    pyproject = tomllib.loads((project_directory / "pyproject.toml").read_text(encoding="utf-8"))
    assert "index" not in pyproject.get("tool", {}).get("uv", {}), pyproject
    uv_sources = pyproject["tool"]["uv"]["sources"]
    site_packages = next((project_directory / ".venv" / "lib").glob("python3*/site-packages"))
    for escaped_distribution_name in ("tatolab_testpattern", "tatolab_h264"):
        source_path = uv_sources[escaped_distribution_name.replace("_", "-")]["path"]
        assert source_path.startswith(
            f"{FIRST_PARTY_MODULE_LINKS_DIRECTORY}/{escaped_distribution_name}-"
        ), uv_sources
        assert source_path.endswith(".whl"), uv_sources
        assert (project_directory / source_path).resolve().is_file(), source_path
        assert list(site_packages.glob(f"{escaped_distribution_name}-*.dist-info")), (
            escaped_distribution_name
        )


def test_create_project_through_tatolab_mcp_runs_the_scaffold_under_its_folder_and_outlives_the_connection(
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    project_directory = make_tatolab_project(
        directory_name="bright-lens", runs_in_the_suite_venv=False
    )
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
    assert_every_first_party_module_came_from_the_runtime_unit(project_directory)

    [listed] = local_api.list_streams()
    assert listed["stream"] == created["stream"], listed
    assert listed["display_name"] == "bright-lens", listed
    assert listed["state"] == "running", listed
    assert listed["location"] == {
        "project_directory": str(resolved_project_directory),
        "project": json.loads((project_directory / PROJECT_FILE_NAME).read_text())["project"],
        "stream_asset": stream_asset_held_at(graph_document_path_of(project_directory, "main")),
    }, listed

    (project_directory / "nodes" / "raising_node.py").write_text(
        NODE_MODULE_THAT_RAISES_AS_IT_IMPORTS, encoding="utf-8"
    )
    write_a_graph_document(project_directory, "broken", SECOND_STREAM_WHOSE_NODE_MODULE_RAISES)
    queued_run = local_api.call_tool("reload_project", {"project_directory": str(project_directory)})
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

    queued_stop = local_api.call_tool("stop_project", {"project_directory": str(project_directory)})
    assert local_api.stream_action_once_ended(queued_stop["action"])["state"] == "done"
    shutil.rmtree(project_directory / ".venv")
    point_the_scaffolds_tatolab_distributions_at_this_checkout(project_directory)
    queued_start = local_api.call_tool("start_project", {"project_directory": str(project_directory)})
    start_once_ended = local_api.stream_action_once_ended(queued_start["action"])

    assert start_once_ended["state"] == "done", start_once_ended
    states_after_the_run = {stream["stream"]: stream["state"] for stream in start_once_ended["streams"]}
    assert states_after_the_run[created["stream"]] == "running", start_once_ended
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

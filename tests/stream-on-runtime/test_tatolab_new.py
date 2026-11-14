# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""`tatolab new` end to end: the project the runtime writes for the runtime unit's `tatolab`.

The templates are checked as files in the stream suite; this checks what the
runtime renders from them, as a user receives it: the starter's files and no node
code, the project file and the stream's graph document holding the UUIDs the
runtime minted, and a venv holding `tatolab-stream` and no runtime. The create
loads the starter's stream, its one test pattern source.
"""

from __future__ import annotations

import json
import os
import subprocess
import uuid
from collections.abc import Callable
from pathlib import Path

import pytest

from conftest import TatolabdUnderTest

pytestmark = pytest.mark.requires_gpu

COMPILE_ENTRY_TIMEOUT_SECONDS = 60.0

SCAFFOLDED_FILE_PATHS = {
    "tatolab-project.json",
    "streams/main.graph.json",
    "pyproject.toml",
    ".python-version",
    ".gitignore",
    # The lockfile the runtime's sync of the project venv writes on the load.
    "uv.lock",
}

def scaffold_with_tatolab_new(
    make_scaffolded_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
) -> Path:
    """`tatolab new` into an empty project, whose `.venv` the runtime makes; returns the project."""
    return make_scaffolded_tatolab_project(start_tatolabd(), directory_name="demo")


def files_written_into(app_directory: Path) -> "dict[str, str]":
    """Every file under the project but its `.venv`, the bytecode its load
    compiled and what the runtime keeps in `.tatolab`, by its path in the project."""
    written_files: "dict[str, str]" = {}
    for directory, subdirectory_names, file_names in os.walk(app_directory):
        subdirectory_names[:] = [
            name
            for name in subdirectory_names
            if name not in (".venv", "__pycache__", ".tatolab")
        ]
        for file_name in file_names:
            path = Path(directory) / file_name
            written_files[path.relative_to(app_directory).as_posix()] = path.read_text(
                encoding="utf-8"
            )
    return written_files


def test_a_new_project_holds_the_starter_files_and_no_node_code(
    make_scaffolded_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    app_directory = scaffold_with_tatolab_new(make_scaffolded_tatolab_project, start_tatolabd)

    assert set(files_written_into(app_directory)) == SCAFFOLDED_FILE_PATHS


def test_the_scaffolded_venv_holds_no_runtime(
    make_scaffolded_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The project's own venv holds `tatolab-stream`; the runtime stays the runtime's."""
    app_directory = scaffold_with_tatolab_new(make_scaffolded_tatolab_project, start_tatolabd)

    runtime_probe = subprocess.run(
        [
            str(app_directory / ".venv" / "bin" / "python"),
            "-I",
            "-c",
            "import importlib.util; print(importlib.util.find_spec('tatolab.runtime') is None)",
        ],
        capture_output=True,
        text=True,
        timeout=COMPILE_ENTRY_TIMEOUT_SECONDS,
        check=True,
    )
    assert runtime_probe.stdout.strip() == "True", "the project's venv holds a runtime"


def test_the_scaffolded_graph_document_holds_its_stream_asset_and_a_uuid_on_each_node(
    make_scaffolded_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The runtime mints the project's, the stream asset's and every node's UUID into what
    `new` writes."""
    app_directory = scaffold_with_tatolab_new(make_scaffolded_tatolab_project, start_tatolabd)

    project_file = json.loads((app_directory / "tatolab-project.json").read_text())
    graph_document = json.loads((app_directory / "streams" / "main.graph.json").read_text())

    assert project_file["name"] == "demo"
    uuid.UUID(project_file["project"])
    uuid.UUID(graph_document["asset"])
    assert graph_document["asset"] != project_file["project"]
    node_uuids = [str(uuid.UUID(node["id"])) for node in graph_document["nodes"]]
    assert len(node_uuids) == 1
    assert [node["type"] for node in graph_document["nodes"]] == [
        "tatolab.testpattern:TestPatternSource",
    ]
    assert graph_document["links"] == []


def test_the_starter_stream_runs_no_camera_and_names_no_window(
    make_scaffolded_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The starter runs on a machine with no capture device and no display:
    nothing `new` writes mentions a window or runs a camera."""
    app_directory = scaffold_with_tatolab_new(make_scaffolded_tatolab_project, start_tatolabd)

    for path_in_project, contents in files_written_into(app_directory).items():
        assert "window" not in contents.lower(), (
            f"the scaffolded {path_in_project} names a window:\n{contents}"
        )
    graph_document = (app_directory / "streams" / "main.graph.json").read_text()
    assert "tatolab.testpattern:TestPatternSource" in graph_document
    assert "Camera" not in graph_document, graph_document

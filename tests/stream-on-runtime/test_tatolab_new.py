# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab new` end to end: the project the runtime writes for the runtime unit's `tatolab`.

The templates are checked as files in the stream suite; this checks what the
runtime renders from them, as a user receives it: every Python file lints and is
formatted, the project file and the stream's graph document hold the UUIDs the
runtime minted, and the venv holds `tatolab-stream` and no runtime. The create loads
the scaffold's stream, which runs a GPU effect.
"""

from __future__ import annotations

import ast
import json
import os
import subprocess
import uuid
from collections.abc import Callable
from pathlib import Path

import pytest

from conftest import TatolabdUnderTest
from runtime_unit_under_test import REPOSITORY_ROOT, SUITE_VENV_INTERPRETER

pytestmark = pytest.mark.requires_gpu

# ruff is pointed at the template tree for each rendered file, so it reads the
# `[tool.ruff]` the stream package checks the templates under.
SCAFFOLD_TEMPLATE_DIRECTORY = REPOSITORY_ROOT / "sdk" / "tatolab-stream" / "scaffold_template"
COMPILE_ENTRY_TIMEOUT_SECONDS = 60.0
RUFF_TIMEOUT_SECONDS = 60.0

SCAFFOLDED_FILE_PATHS = {
    "tatolab-project.json",
    "streams/main.graph.json",
    "nodes/__init__.py",
    "nodes/halftone_lens_effect.py",
    "nodes/brightness_meter.py",
    "pyproject.toml",
    ".python-version",
    ".gitignore",
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


@pytest.mark.parametrize("ruff_arguments", [("check",), ("format", "--check")])
def test_every_scaffolded_python_file_passes_ruff(
    make_scaffolded_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    ruff_arguments: "tuple[str, ...]",
):
    """Linted as rendered, not as templated: a render drops the licence header
    every template opens with."""
    app_directory = scaffold_with_tatolab_new(make_scaffolded_tatolab_project, start_tatolabd)
    written_files = files_written_into(app_directory)

    assert set(written_files) == SCAFFOLDED_FILE_PATHS
    for path_in_project, contents in sorted(written_files.items()):
        if not path_in_project.endswith(".py"):
            continue
        assert "noqa" not in contents, f"the scaffolded {path_in_project} suppresses a lint"
        finished = subprocess.run(
            [
                str(SUITE_VENV_INTERPRETER),
                "-m",
                "ruff",
                *ruff_arguments,
                "--no-cache",
                "--stdin-filename",
                str(SCAFFOLD_TEMPLATE_DIRECTORY / path_in_project),
                "-",
            ],
            input=contents,
            capture_output=True,
            text=True,
            timeout=RUFF_TIMEOUT_SECONDS,
            check=False,
        )
        assert finished.returncode == 0, (
            f"ruff {' '.join(ruff_arguments)} rejects the scaffolded {path_in_project}:\n"
            f"{finished.stdout}{finished.stderr}"
        )


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
    assert len(set(node_uuids)) == len(node_uuids) == 3
    assert [node["type"] for node in graph_document["nodes"]] == [
        "tatolab.testpattern:TestPatternSource",
        "nodes.halftone_lens_effect:HalftoneLensEffect",
        "nodes.brightness_meter:BrightnessMeter",
    ]
    assert all(
        link["source"]["node"] in node_uuids and link["target"]["node"] in node_uuids
        for link in graph_document["links"]
    ), "each link end names its node by UUID"


def test_the_sample_stream_runs_no_camera_and_names_no_window(
    make_scaffolded_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The sample stream runs on a machine with no capture device and no display:
    nothing `new` writes mentions a window or runs a camera."""
    app_directory = scaffold_with_tatolab_new(make_scaffolded_tatolab_project, start_tatolabd)

    for path_in_project, contents in files_written_into(app_directory).items():
        assert "window" not in contents.lower(), (
            f"the scaffolded {path_in_project} names a window:\n{contents}"
        )
        if path_in_project.endswith(".py"):
            names_in_the_code = {
                node.id for node in ast.walk(ast.parse(contents)) if isinstance(node, ast.Name)
            }
            assert "CameraSource" not in names_in_the_code, (
                f"the scaffolded {path_in_project} runs a camera:\n{contents}"
            )
    graph_document = (app_directory / "streams" / "main.graph.json").read_text()
    assert "tatolab.testpattern:TestPatternSource" in graph_document
    assert "Camera" not in graph_document, graph_document

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab new` end to end: the project the runtime unit's `tatolab` writes.

The templates are checked as files in the stream suite; this checks what the
binary renders from them, in both variants, as a user receives it: every
Python file lints and is formatted, and the stream compiles in a venv holding
`tatolab-stream` and no runtime. Needs no GPU.
"""

from __future__ import annotations

import json
import os
import subprocess
from collections.abc import Callable
from pathlib import Path

import pytest

from runtime_unit_under_test import REPOSITORY_ROOT
from tatolab.stream import CameraSource, DisplayWindow, TestPatternSource

# ruff is pointed at the template tree for each rendered file, so it reads the
# `[tool.ruff]` the stream package checks the templates under.
SCAFFOLD_TEMPLATE_DIRECTORY = REPOSITORY_ROOT / "sdk" / "tatolab-stream" / "scaffold_template"
COMPILE_ENTRY_TIMEOUT_SECONDS = 60.0
RUFF_TIMEOUT_SECONDS = 60.0

SCAFFOLDED_FILE_PATHS = {
    "stream.py",
    "nodes/__init__.py",
    "nodes/inverting_effect.py",
    "nodes/brightness_meter.py",
    "pyproject.toml",
    ".python-version",
    ".gitignore",
}

SCAFFOLD_VARIANTS = pytest.mark.parametrize(
    "use_test_pattern_source", [False, True], ids=["camera", "test-pattern"]
)


def scaffold_with_tatolab_new(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    use_test_pattern_source: bool,
) -> Path:
    """`tatolab new` into a project whose `.venv` is the suite venv; returns the project."""
    app_directory = make_tatolab_project(directory_name="demo")
    scaffolded = run_tatolab(
        "new",
        app_directory,
        *(["--test-pattern"] if use_test_pattern_source else []),
        working_directory=app_directory.parent,
    )
    assert scaffolded.returncode == 0, scaffolded.stdout + scaffolded.stderr
    return app_directory


def files_written_into(app_directory: Path) -> "dict[str, str]":
    """Every file under the project but its `.venv`, by its path in the project."""
    written_files: "dict[str, str]" = {}
    for directory, subdirectory_names, file_names in os.walk(app_directory):
        subdirectory_names[:] = [name for name in subdirectory_names if name != ".venv"]
        for file_name in file_names:
            path = Path(directory) / file_name
            written_files[path.relative_to(app_directory).as_posix()] = path.read_text(
                encoding="utf-8"
            )
    return written_files


@SCAFFOLD_VARIANTS
@pytest.mark.parametrize("ruff_arguments", [("check",), ("format", "--check")])
def test_every_scaffolded_python_file_passes_ruff(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    use_test_pattern_source: bool,
    ruff_arguments: "tuple[str, ...]",
):
    """Linted as rendered, not as templated: the test-pattern variant is text no
    template file holds."""
    app_directory = scaffold_with_tatolab_new(
        make_tatolab_project, run_tatolab, use_test_pattern_source
    )
    written_files = files_written_into(app_directory)

    assert set(written_files) == SCAFFOLDED_FILE_PATHS
    for path_in_project, contents in sorted(written_files.items()):
        if not path_in_project.endswith(".py"):
            continue
        assert "noqa" not in contents, f"the scaffolded {path_in_project} suppresses a lint"
        finished = subprocess.run(
            [
                str(app_directory / ".venv" / "bin" / "python"),
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


@SCAFFOLD_VARIANTS
def test_the_scaffolded_project_compiles_with_no_runtime_in_its_venv(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    use_test_pattern_source: bool,
):
    """The compile `tatolab run` makes, in the project's own venv, over what `new` wrote."""
    app_directory = scaffold_with_tatolab_new(
        make_tatolab_project, run_tatolab, use_test_pattern_source
    )
    project_venv_interpreter = app_directory / ".venv" / "bin" / "python"

    runtime_probe = subprocess.run(
        [
            str(project_venv_interpreter),
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

    finished = subprocess.run(
        [
            str(project_venv_interpreter),
            "-I",
            "-m",
            "tatolab.stream._project_stream_compile_entry",
            "--verb",
            "run",
        ],
        cwd=app_directory,
        capture_output=True,
        text=True,
        timeout=COMPILE_ENTRY_TIMEOUT_SECONDS,
        check=False,
    )

    assert finished.returncode == 0, finished.stdout + finished.stderr
    compiled = json.loads(finished.stdout)
    assert Path(compiled["project_directory"]) == app_directory.resolve()
    stream_graph = compiled["stream_graph"]
    assert stream_graph["stream"] == "main"
    expected_source_type = (TestPatternSource if use_test_pattern_source else CameraSource).type
    assert [node["type"] for node in stream_graph["nodes"]] == [
        expected_source_type,
        "nodes.inverting_effect:InvertingEffect",
        "nodes.brightness_meter:BrightnessMeter",
        DisplayWindow.type,
    ]
    assert stream_graph["exposed"] == [
        {"node": "invertingeffect", "port": "video_to_downstream", "level": "private"}
    ]


def test_the_test_pattern_render_names_no_camera(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
):
    """`--test-pattern` is for a machine with no capture device, so nothing it
    writes mentions one."""
    app_directory = scaffold_with_tatolab_new(make_tatolab_project, run_tatolab, True)

    for path_in_project, contents in files_written_into(app_directory).items():
        assert "camera" not in contents.lower(), (
            f"the test-pattern render's {path_in_project} names a camera:\n{contents}"
        )
    assert "stream_builder.add(TestPatternSource)" in (app_directory / "stream.py").read_text()

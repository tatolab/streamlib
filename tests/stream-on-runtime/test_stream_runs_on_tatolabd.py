# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""How the harness turns what a test hands it into the project `tatolab run` loads.

Needs no runtime: these check the project the harness would hand `tatolab run`,
compiled by the suite venv's own compile entry, and the wrapper interpreter of a
hand-written graph's project, run directly.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from runtime_load_streams import named_pattern_into_a_named_sink
from runtime_unit_under_test import (
    STREAM_ON_RUNTIME_SUITE_DIRECTORY,
    SUITE_VENV_INTERPRETER,
    SUITE_VENV_PREFIX,
)
from stream_runs_on_tatolabd import (
    COMPILE_ENTRY_INVOCATION,
    StreamFunctionNotRunnableFromTheSuiteProject,
    TatolabRunOfAProject,
    stream_id_of_a_one_stream_project,
    stream_id_of_the_location,
    write_a_hand_written_graph_project,
    write_a_one_stream_project_of_a_suite_stream_function,
)
from tatolab.stream import StreamBuilder

WRAPPER_INVOCATION_TIMEOUT_SECONDS = 30.0


def a_stream_function_defined_in_this_test_module(stream_builder: StreamBuilder) -> None:
    """Never run: a test module is no place a compile interpreter imports."""


def test_a_suite_stream_function_runs_as_a_one_stream_project_compiling_to_its_own_graph(
    tmp_path: Path,
):
    project_directory = tmp_path / "project"

    tatolab_run = write_a_one_stream_project_of_a_suite_stream_function(
        project_directory, named_pattern_into_a_named_sink
    )

    assert tatolab_run == TatolabRunOfAProject(working_directory=project_directory)
    assert (project_directory / ".venv").resolve() == SUITE_VENV_PREFIX.resolve()
    assert (project_directory / "runtime_load_streams.py").resolve() == (
        STREAM_ON_RUNTIME_SUITE_DIRECTORY / "runtime_load_streams.py"
    )
    compiled = subprocess.run(
        [
            str(project_directory / ".venv" / "bin" / "python"),
            *COMPILE_ENTRY_INVOCATION,
            "--verb",
            "run",
            "--every-stream",
        ],
        cwd=project_directory,
        capture_output=True,
        text=True,
        timeout=WRAPPER_INVOCATION_TIMEOUT_SECONDS,
        check=True,
    )
    compiled_document = json.loads(compiled.stdout.splitlines()[-1])
    [compiled_stream] = compiled_document["streams"]
    assert compiled_stream["stream_function"] == "stream:named_pattern_into_a_named_sink"
    assert compiled_stream["stream_graph"]["stream"] == "named_pattern_into_a_named_sink"
    assert stream_id_of_a_one_stream_project(
        project_directory, named_pattern_into_a_named_sink
    ) == stream_id_of_the_location(
        Path(compiled_document["project_directory"]), compiled_stream["stream_function"]
    )


def test_a_stream_function_in_a_test_module_is_refused_naming_where_to_move_it(tmp_path: Path):
    with pytest.raises(StreamFunctionNotRunnableFromTheSuiteProject) as refusal:
        write_a_one_stream_project_of_a_suite_stream_function(
            tmp_path / "project", a_stream_function_defined_in_this_test_module
        )

    assert "test_stream_runs_on_tatolabd" in str(refusal.value)
    assert "`*_streams.py`" in str(refusal.value)


def test_a_stream_function_defined_inside_a_function_is_refused_as_not_importable(
    tmp_path: Path,
):
    def built_at_run_time(stream_builder: StreamBuilder) -> None: ...

    with pytest.raises(StreamFunctionNotRunnableFromTheSuiteProject) as refusal:
        write_a_one_stream_project_of_a_suite_stream_function(
            tmp_path / "project", built_at_run_time
        )

    assert "<locals>" in str(refusal.value)
    assert "not a module-level function" in str(refusal.value)


def test_a_hand_written_graph_projects_interpreter_prints_the_graph_as_the_compile_document(
    tmp_path: Path,
):
    graph_text = '{"stream": "hand-written", "nodes": [], "value": NaN}'
    project = write_a_hand_written_graph_project(
        tmp_path / "project", graph_text, reported_project_directory=tmp_path / "nodes-live-here"
    )

    compiled = subprocess.run(
        [str(project.venv_interpreter), *COMPILE_ENTRY_INVOCATION, "--verb", "run"],
        capture_output=True,
        text=True,
        timeout=WRAPPER_INVOCATION_TIMEOUT_SECONDS,
        check=True,
    )

    assert compiled.stdout == (
        '{"stream_graph": ' + graph_text + ', "project_directory": '
        + json.dumps(str(tmp_path / "nodes-live-here"))
        + ', "stream_function": "stream:hand-written"}\n'
    )
    every_stream_compiled = subprocess.run(
        [str(project.venv_interpreter), *COMPILE_ENTRY_INVOCATION, "--verb", "run", "--every-stream"],
        capture_output=True,
        text=True,
        timeout=WRAPPER_INVOCATION_TIMEOUT_SECONDS,
        check=True,
    )
    assert every_stream_compiled.stdout == (
        '{"project_directory": '
        + json.dumps(str(tmp_path / "nodes-live-here"))
        + ', "streams": [{"stream_function": "stream:hand-written", "description": null, '
        + '"stream_graph": ' + graph_text + "}]}\n"
    )
    assert project.tatolab_run == TatolabRunOfAProject(working_directory=tmp_path / "project")


def test_a_hand_written_graph_projects_interpreter_execs_the_processor_interpreter_otherwise(
    tmp_path: Path,
):
    project = write_a_hand_written_graph_project(tmp_path / "project", {"stream": "x", "nodes": []})

    ran = subprocess.run(
        [str(project.venv_interpreter), "-c", "import sys; print(sys.executable)"],
        capture_output=True,
        text=True,
        timeout=WRAPPER_INVOCATION_TIMEOUT_SECONDS,
        check=True,
    )

    assert ran.stdout.strip() == str(SUITE_VENV_INTERPRETER)
    assert project.reported_project_directory == STREAM_ON_RUNTIME_SUITE_DIRECTORY

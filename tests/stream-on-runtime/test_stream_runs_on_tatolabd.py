# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""How the harness turns what a test hands it into the `tatolab run` that loads it.

Needs no runtime: these check the arguments and the project the harness would
hand `tatolab run`, and the wrapper interpreter of a hand-written graph's
project, run directly.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from runtime_load_streams import named_pattern_into_a_named_sink
from runtime_unit_under_test import STREAM_ON_RUNTIME_SUITE_DIRECTORY, SUITE_VENV_INTERPRETER
from stream_runs_on_tatolabd import (
    COMPILE_ENTRY_INVOCATION,
    StreamFunctionNotRunnableFromTheSuiteProject,
    TatolabRunOfAProject,
    tatolab_run_of_a_suite_stream_function,
    write_a_hand_written_graph_project,
)
from tatolab.stream import StreamBuilder

WRAPPER_INVOCATION_TIMEOUT_SECONDS = 30.0


def a_stream_function_defined_in_this_test_module(stream_builder: StreamBuilder) -> None:
    """Never run: a test module is no place a compile interpreter imports."""


def test_a_suite_stream_function_runs_by_module_and_function_from_the_suite_project():
    assert tatolab_run_of_a_suite_stream_function(
        named_pattern_into_a_named_sink, "served"
    ) == TatolabRunOfAProject(
        working_directory=STREAM_ON_RUNTIME_SUITE_DIRECTORY,
        tatolab_run_arguments=(
            "runtime_load_streams:named_pattern_into_a_named_sink",
            "--name",
            "served",
        ),
    )


def test_a_stream_function_in_a_test_module_is_refused_naming_where_to_move_it():
    with pytest.raises(StreamFunctionNotRunnableFromTheSuiteProject) as refusal:
        tatolab_run_of_a_suite_stream_function(a_stream_function_defined_in_this_test_module, None)

    assert "test_stream_runs_on_tatolabd" in str(refusal.value)
    assert "`*_streams.py`" in str(refusal.value)


def test_a_stream_function_defined_inside_a_function_is_refused_as_not_importable():
    def built_at_run_time(stream_builder: StreamBuilder) -> None: ...

    with pytest.raises(StreamFunctionNotRunnableFromTheSuiteProject) as refusal:
        tatolab_run_of_a_suite_stream_function(built_at_run_time, None)

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
        + json.dumps(str(tmp_path / "nodes-live-here")) + "}\n"
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

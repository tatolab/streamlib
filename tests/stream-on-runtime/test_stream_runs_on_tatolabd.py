# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""How the harness turns what a test hands it into the project `tatolab start` loads.

Needs no runtime: these check the project the harness would hand `tatolab start` — its
project file, the graph documents it writes and the venv it links — and the interpreter of
a hand-written graph's project, run directly.
"""

from __future__ import annotations

import json
import subprocess
import uuid
from collections.abc import Callable
from pathlib import Path

import pytest

from runtime_load_streams import named_pattern_into_a_named_sink
from runtime_unit_under_test import (
    STREAM_ON_RUNTIME_SUITE_DIRECTORY,
    SUITE_VENV_INTERPRETER,
    SUITE_VENV_PREFIX,
)
from stream_runs_on_tatolabd import (
    PROJECT_FILE_NAME,
    SUITE_FILES_THE_SUITE_VENV_IS_SYNCED_FROM,
    StreamFunctionNotRunnableFromTheSuiteProject,
    TatolabRunOfAProject,
    graph_document_path_of,
    graph_nodes_holding_a_uuid_each,
    stream_id_named,
    stream_id_of_a_one_stream_project,
    write_a_hand_written_graph_project,
    write_a_one_stream_project_of_a_suite_stream_function,
    write_the_graph_documents_the_stream_py_compiles_to,
)
from tatolab.stream import StreamBuilder
from tatolab.stream._stream_graph_builder import run_graph_generator

WRAPPER_INVOCATION_TIMEOUT_SECONDS = 30.0


def a_stream_function_defined_in_this_test_module(stream_builder: StreamBuilder) -> None:
    """Never run: a test module is no place a processor interpreter imports."""


def test_a_suite_stream_function_runs_as_a_one_stream_project_holding_its_graph_document(
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
    assert not (project_directory / "stream.py").exists()
    project_file = json.loads((project_directory / PROJECT_FILE_NAME).read_text())
    assert project_file["name"] == "project"
    graph_document = json.loads(
        graph_document_path_of(project_directory, "named_pattern_into_a_named_sink").read_text()
    )
    stream_asset = graph_document.pop("asset")
    assert graph_document == graph_nodes_holding_a_uuid_each(
        run_graph_generator(named_pattern_into_a_named_sink), stream_asset
    )
    assert stream_id_of_a_one_stream_project(
        project_directory, named_pattern_into_a_named_sink
    ) == stream_id_named(
        "project", "named_pattern_into_a_named_sink", project_file["project"], stream_asset
    )


def test_a_written_project_holds_nothing_the_runtime_would_sync_the_suite_venv_from(
    tmp_path: Path,
):
    """The runtime syncs a project's `.venv` again from its own `pyproject.toml`, `uv.lock`
    and `.python-version`; the suite's, read from a project under the test's tmp directory,
    name path sources that are not there."""
    one_stream_project = tmp_path / "one-stream"
    write_a_one_stream_project_of_a_suite_stream_function(
        one_stream_project, named_pattern_into_a_named_sink
    )
    hand_written_project = write_a_hand_written_graph_project(
        tmp_path / "hand-written", '{"nodes": []}'
    ).tatolab_run.working_directory

    for suite_file in SUITE_FILES_THE_SUITE_VENV_IS_SYNCED_FROM:
        assert (STREAM_ON_RUNTIME_SUITE_DIRECTORY / suite_file).is_file(), suite_file
        assert not (one_stream_project / suite_file).exists(), suite_file
        assert not (hand_written_project / suite_file).exists(), suite_file


def test_a_made_project_given_its_own_pyproject_is_refused_the_suite_venv(
    make_tatolab_project: "Callable[..., Path]",
):
    """The runtime syncs a project's `.venv` to the project's own `pyproject.toml`; one
    linked to the suite venv would strip the suite's packages for every later test."""
    with pytest.raises(ValueError, match="runs_in_the_suite_venv=False"):
        make_tatolab_project({"pyproject.toml": '[project]\nname = "own"\nversion = "0"\n'})


def test_a_made_project_not_run_in_the_suite_venv_holds_no_venv_for_the_runtime_to_make(
    make_tatolab_project: "Callable[..., Path]",
):
    project_directory = make_tatolab_project(
        {"pyproject.toml": '[project]\nname = "own"\nversion = "0"\n'},
        runs_in_the_suite_venv=False,
    )

    assert not (project_directory / ".venv").exists()
    assert not (project_directory / ".venv").is_symlink()
    assert (project_directory / "pyproject.toml").is_file()


def test_the_stream_id_oracle_is_the_project_name_the_stream_name_and_six_hex_of_the_two_uuids():
    project = "4b2a8c58-9d9f-4f43-9a52-1f1f6b7f2a10"
    stream_asset = "e3c0d9b4-1d55-4b8e-8a8f-0b3a7c7f9a21"

    stream_id = stream_id_named("Robot Cell", "arm control", project, stream_asset)

    assert stream_id.startswith("robot-cell-arm-control-")
    assert len(stream_id.rsplit("-", 1)[-1]) == 6
    assert stream_id != stream_id_named("Robot Cell", "arm control", project, str(uuid.uuid4()))


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


def test_a_hand_written_graph_is_written_verbatim_but_for_its_stream_asset(tmp_path: Path):
    graph_text = '{"stream": "hand-written", "nodes": [], "value": NaN}'

    project = write_a_hand_written_graph_project(tmp_path / "project", graph_text)

    document_text = project.graph_document_path.read_text()
    assert project.graph_document_path == graph_document_path_of(
        tmp_path / "project", "hand-written"
    )
    assert document_text.endswith(' "stream": "hand-written", "nodes": [], "value": NaN}')
    assert document_text.startswith('{"asset": "')
    assert project.tatolab_run == TatolabRunOfAProject(working_directory=tmp_path / "project")

    rewritten = write_a_hand_written_graph_project(
        tmp_path / "project", "not json", reported_project_directory=tmp_path / "project"
    )
    assert rewritten.stream_name == "main"
    assert rewritten.graph_document_path.read_text() == "not json"


def test_hand_written_graphs_of_one_reported_project_share_its_project_file(tmp_path: Path):
    nodes_live_here = tmp_path / "nodes-live-here"

    first = write_a_hand_written_graph_project(
        tmp_path / "unused", {"stream": "first", "nodes": []},
        reported_project_directory=nodes_live_here,
    )
    project_file_text = (nodes_live_here / PROJECT_FILE_NAME).read_text()
    second = write_a_hand_written_graph_project(
        tmp_path / "unused", {"stream": "second", "nodes": []},
        reported_project_directory=nodes_live_here,
    )

    assert first.project_directory == second.project_directory == nodes_live_here
    assert (nodes_live_here / PROJECT_FILE_NAME).read_text() == project_file_text
    assert first.stream_id != second.stream_id
    assert not (tmp_path / "unused").exists()


def test_a_hand_written_graph_projects_interpreter_execs_the_processor_interpreter(
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
    assert (project.project_directory / "runtime_load_streams.py").resolve() == (
        STREAM_ON_RUNTIME_SUITE_DIRECTORY / "runtime_load_streams.py"
    )


def test_a_stream_py_compiles_to_one_graph_document_per_stream_keeping_each_asset(
    tmp_path: Path,
):
    (tmp_path / "stream.py").write_text(
        "from tatolab.stream import StreamBuilder, graph_generator\n\n\n"
        "@graph_generator\ndef camera(stream_builder: StreamBuilder) -> None: ...\n\n\n"
        "@graph_generator\ndef preview(stream_builder: StreamBuilder) -> None: ...\n",
        encoding="utf-8",
    )

    assert write_the_graph_documents_the_stream_py_compiles_to(tmp_path) == ["camera", "preview"]
    camera_id = json.loads(graph_document_path_of(tmp_path, "camera").read_text())["asset"]
    (tmp_path / "stream.py").write_text(
        "from tatolab.stream import StreamBuilder, graph_generator\n\n\n"
        "@graph_generator\ndef camera(stream_builder: StreamBuilder) -> None: ...\n",
        encoding="utf-8",
    )

    assert write_the_graph_documents_the_stream_py_compiles_to(tmp_path) == ["camera"]
    assert json.loads(graph_document_path_of(tmp_path, "camera").read_text())["asset"] == camera_id
    assert not graph_document_path_of(tmp_path, "preview").exists()

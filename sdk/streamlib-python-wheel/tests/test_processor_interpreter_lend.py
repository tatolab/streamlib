# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stream's Python nodes run from a venv holding only `tatolab-stream`.

The fixture venv has no `tatolab.runtime` of its own: a processor interpreter
started from it borrows the runtime through the lend directory, and `load`
learns each node's ports by describing it there. What a load refuses — a module
that raises, an unstamped class, an interpreter that cannot load the lent
runtime — is read from a Runtime that is never run, so only the running stream
needs a GPU.
"""

from __future__ import annotations

import platform
import re
import shutil
import subprocess
import sys
import textwrap
from collections.abc import Iterator
from pathlib import Path

import pytest

import tatolab.runtime
from tatolab.runtime._engine import processor_class_import_paths_in_this_processes_catalog

TATOLAB_STREAM_SOURCE_DIRECTORY = Path(__file__).resolve().parents[2] / "tatolab-stream"

APP = Path(__file__).parent / "processor_interpreter_lend_app.py"

SECONDS_TO_BUILD_THE_FIXTURE_VENV = 600.0

PROCESSOR_INTERPRETER_MARKER = re.compile(r"MARKER:PROCESSOR_INTERPRETER=(\S+)")
LENT_RUNTIME_FILE_MARKER = re.compile(r"MARKER:LENT_RUNTIME_FILE=(\S+)")
LEND_DIRECTORY_MARKER = re.compile(r"MARKER:LEND_DIRECTORY=(\S+)")
BAGS_PROCESSED_MARKER = re.compile(
    r"MARKER:BAGS_PROCESSED=(\d+) SINK_INTERPRETER=(\S+) UPSTREAM_INTERPRETER=(\S+)"
)


def _run_building_the_fixture_venv(command: list[str]) -> None:
    built = subprocess.run(
        command,
        capture_output=True,
        text=True,
        timeout=SECONDS_TO_BUILD_THE_FIXTURE_VENV,
        check=False,
    )
    if built.returncode != 0:
        pytest.fail(
            f"building the fixture venv failed at {command!r} (exit {built.returncode}):\n"
            f"{built.stdout}\n{built.stderr}"
        )


@pytest.fixture(scope="session")
def fixture_venv_interpreter(tmp_path_factory: pytest.TempPathFactory) -> Path:
    """The interpreter of a venv holding `tatolab-stream` and nothing of the runtime.

    Built with `uv` when it is on PATH, else with `venv` and `pip`. A machine with
    neither fails here rather than skipping: these tests are the proof that a node
    runs from a venv the runtime is not installed in.
    """
    venv_directory = tmp_path_factory.mktemp("processor-interpreter-lend") / "fixture-venv"
    venv_interpreter = venv_directory / "bin" / "python"
    uv = shutil.which("uv")
    if uv is not None:
        _run_building_the_fixture_venv([uv, "venv", "--python", sys.executable, str(venv_directory)])
        _run_building_the_fixture_venv(
            [uv, "pip", "install", "--python", str(venv_interpreter), str(TATOLAB_STREAM_SOURCE_DIRECTORY)]
        )
    else:
        _run_building_the_fixture_venv([sys.executable, "-m", "venv", str(venv_directory)])
        _run_building_the_fixture_venv(
            [str(venv_interpreter), "-m", "pip", "install", str(TATOLAB_STREAM_SOURCE_DIRECTORY)]
        )

    holds_only_the_stream_package = subprocess.run(
        [
            str(venv_interpreter),
            "-c",
            "import importlib.util\n"
            "assert importlib.util.find_spec('tatolab.runtime') is None, 'the runtime is installed'\n"
            "import tatolab.stream\n",
        ],
        capture_output=True,
        text=True,
        timeout=SECONDS_TO_BUILD_THE_FIXTURE_VENV,
        check=False,
    )
    assert holds_only_the_stream_package.returncode == 0, holds_only_the_stream_package.stderr
    return venv_interpreter


@pytest.fixture
def runtime() -> Iterator[tatolab.runtime.Runtime]:
    """A Runtime built but never run, shut down whatever the test did."""
    built_runtime = tatolab.runtime.Runtime()
    try:
        yield built_runtime
    finally:
        built_runtime.shutdown()


def write_project_module(project_directory: Path, module_name: str, source: str) -> None:
    (project_directory / f"{module_name}.py").write_text(textwrap.dedent(source))


def graph_naming_one_node_of_type(node_type: str) -> dict[str, object]:
    return {"nodes": [{"name": "nodeundertest", "type": node_type, "config": {}}]}


# ---- describe, from the fixture venv's interpreter ---------------------------


def test_a_type_whose_module_raises_at_import_is_refused_quoting_the_interpreters_stderr(
    runtime: tatolab.runtime.Runtime, fixture_venv_interpreter: Path, tmp_path: Path
):
    write_project_module(
        tmp_path,
        "lend_raising_nodes",
        """
        raise RuntimeError("lend_raising_nodes refuses to import on purpose")
        """,
    )

    with pytest.raises(RuntimeError) as refused:
        runtime.load(
            graph_naming_one_node_of_type("lend_raising_nodes:NeverDeclared"),
            project_directory=tmp_path,
            interpreter=fixture_venv_interpreter,
        )

    assert "lend_raising_nodes:NeverDeclared" in str(refused.value)
    assert "lend_raising_nodes refuses to import on purpose" in str(refused.value)
    assert "lend_raising_nodes" not in sys.modules


def test_an_unstamped_class_is_refused_by_name(
    runtime: tatolab.runtime.Runtime, fixture_venv_interpreter: Path, tmp_path: Path
):
    write_project_module(
        tmp_path,
        "lend_unstamped_nodes",
        """
        class NotANode:
            def process(self, ctx) -> None: ...
        """,
    )

    with pytest.raises(RuntimeError) as refused:
        runtime.load(
            graph_naming_one_node_of_type("lend_unstamped_nodes:NotANode"),
            project_directory=tmp_path,
            interpreter=fixture_venv_interpreter,
        )

    assert "lend_unstamped_nodes:NotANode" in str(refused.value)
    assert "carries no `@node` declaration" in str(refused.value)


def test_an_interpreter_that_cannot_load_the_lent_runtime_is_refused_naming_what_it_is(
    runtime: tatolab.runtime.Runtime, fixture_venv_interpreter: Path, tmp_path: Path
):
    """The wrapper drops the lend from `PYTHONPATH`, so the fixture venv's
    interpreter — which has no runtime of its own — cannot import it."""
    project_directory = tmp_path / "project"
    project_directory.mkdir()
    write_project_module(
        project_directory,
        "lend_declared_nodes_for_a_refused_interpreter",
        """
        from tatolab.stream import node


        @node(execution="manual")
        class DeclaredNode:
            @node.output()
            def nothing_to_downstream(self) -> None: ...
        """,
    )
    interpreter_without_the_lend = tmp_path / "python-without-the-lend"
    interpreter_without_the_lend.write_text(
        f'#!/bin/sh\nunset PYTHONPATH\nexec "{fixture_venv_interpreter}" "$@"\n'
    )
    interpreter_without_the_lend.chmod(0o755)

    with pytest.raises(RuntimeError) as refused:
        runtime.load(
            graph_naming_one_node_of_type(
                "lend_declared_nodes_for_a_refused_interpreter:DeclaredNode"
            ),
            project_directory=project_directory,
            interpreter=interpreter_without_the_lend,
        )

    refusal = str(refused.value)
    assert "lend_declared_nodes_for_a_refused_interpreter:DeclaredNode" in refusal
    assert str(interpreter_without_the_lend) in refusal
    assert f"interpreter: {fixture_venv_interpreter}" in refusal
    assert f"implementation: {platform.python_implementation()}" in refusal
    assert f"version: {platform.python_version()}" in refusal
    assert "free-threaded: no" in refusal
    assert f"architecture: {platform.machine()}" in refusal
    assert "No module named 'tatolab.runtime'" in refusal


def test_a_type_that_describes_registers_without_its_module_entering_this_process(
    runtime: tatolab.runtime.Runtime, fixture_venv_interpreter: Path, tmp_path: Path
):
    write_project_module(
        tmp_path,
        "lend_described_nodes",
        """
        from tatolab.stream import node


        @node(execution="continuous", interval_ms=50)
        class DescribedSource:
            @node.output()
            def bags_to_downstream(self) -> None: ...

            def process(self, ctx) -> None: ...
        """,
    )
    described_type = "lend_described_nodes:DescribedSource"
    assert described_type not in processor_class_import_paths_in_this_processes_catalog()

    runtime.load(
        graph_naming_one_node_of_type(described_type),
        project_directory=tmp_path,
        interpreter=fixture_venv_interpreter,
    )

    assert described_type in processor_class_import_paths_in_this_processes_catalog()
    assert "lend_described_nodes" not in sys.modules


# ---- a running stream --------------------------------------------------------


@pytest.mark.requires_gpu
def test_a_node_runs_in_a_processor_interpreter_started_from_a_venv_holding_only_tatolab_stream(
    start_app_under_test, fixture_venv_interpreter: Path
):
    app = start_app_under_test(
        APP, "nodes_run_in_the_named_interpreter", str(fixture_venv_interpreter)
    )
    app.await_output_containing("MARKER:BAGS_PROCESSED=", "the sink processing bags")
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    lend_directory = LEND_DIRECTORY_MARKER.search(app.output)
    processor_interpreter = PROCESSOR_INTERPRETER_MARKER.search(app.output)
    lent_runtime_file = LENT_RUNTIME_FILE_MARKER.search(app.output)
    bags_processed = BAGS_PROCESSED_MARKER.search(app.output)
    assert lend_directory and processor_interpreter and lent_runtime_file and bags_processed, (
        app.output
    )
    assert processor_interpreter.group(1) == str(fixture_venv_interpreter)
    assert processor_interpreter.group(1) != sys.executable
    assert bags_processed.group(2) == str(fixture_venv_interpreter)
    assert bags_processed.group(3) == str(fixture_venv_interpreter)
    assert Path(lent_runtime_file.group(1)).resolve().is_relative_to(
        Path(lend_directory.group(1)).resolve()
    ), (lent_runtime_file.group(1), lend_directory.group(1))

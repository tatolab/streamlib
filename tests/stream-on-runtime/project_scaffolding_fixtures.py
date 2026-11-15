# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The integration suite's fixtures that make project directories; `conftest.py`
registers them and says how a test uses them."""

from __future__ import annotations

import re
import subprocess
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest

from conftest import TatolabdUnderTest
from node_module_whose_describe_holds_the_load import NodeModuleWhoseDescribeHoldsTheLoad
from runtime_unit_under_test import SUITE_VENV_PREFIX
from stream_runs_on_tatolabd import write_the_graph_documents_the_stream_py_compiles_to


@pytest.fixture
def make_tatolab_project(tmp_path: Path) -> "Callable[..., Path]":
    """Make a `tatolab` project directory whose `.venv` symlinks the suite venv.

    `make_tatolab_project(files={relative_path: text}, directory_name="app",
    runs_in_the_suite_venv=True) -> Path`: each file is written under the project, and
    `.venv` points at the suite venv's root, so the project's Python nodes run in
    `<project>/.venv/bin/python`, holding `tatolab-stream` and nothing of the runtime. A
    `stream.py` among the files is compiled in the suite venv to the project's graph
    documents. With `runs_in_the_suite_venv=False` the project gets no `.venv`, and the
    runtime makes one of its own from the `pyproject.toml` the project holds or is
    given: the runtime syncs a venv to its project's `pyproject.toml`, so a project
    holding one never links the suite venv.
    """

    def make(
        files: "dict[str, str] | None" = None,
        directory_name: str = "app",
        runs_in_the_suite_venv: bool = True,
    ) -> Path:
        if runs_in_the_suite_venv and "pyproject.toml" in (files or {}):
            raise ValueError(
                "a project given its own pyproject.toml would have the runtime sync "
                "the suite venv to it; make it with runs_in_the_suite_venv=False"
            )
        project_directory = tmp_path / directory_name
        project_directory.mkdir(parents=True)
        if runs_in_the_suite_venv:
            (project_directory / ".venv").symlink_to(SUITE_VENV_PREFIX, target_is_directory=True)
        for relative_path, text in (files or {}).items():
            project_file = project_directory / relative_path
            project_file.parent.mkdir(parents=True, exist_ok=True)
            project_file.write_text(text, encoding="utf-8")
        if "stream.py" in (files or {}):
            write_the_graph_documents_the_stream_py_compiles_to(project_directory)
        return project_directory

    return make


@pytest.fixture
def make_scaffolded_tatolab_project(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
) -> "Callable[..., Path]":
    """`tatolab new` into an empty project directory, on a running `tatolabd`.

    `make_scaffolded_tatolab_project(tatolabd, directory_name="app") -> Path`:
    the runtime writes the scaffold, makes the project's own `.venv` from the
    scaffold's `pyproject.toml` as it does for a user, and loads its stream;
    returns once that action is `done`.
    """

    def make(tatolabd: TatolabdUnderTest, directory_name: str = "app") -> Path:
        app_directory = make_tatolab_project(
            directory_name=directory_name, runs_in_the_suite_venv=False
        )
        created = run_tatolab("new", app_directory, working_directory=app_directory.parent)
        assert created.returncode == 0, created.stdout + created.stderr
        action = created.stdout.split(" ", 1)[0]
        create_once_ended = tatolabd.local_api_client().stream_action_once_ended(action)
        assert create_once_ended["state"] == "done", create_once_ended
        return app_directory

    return make


@pytest.fixture
def held_node_module(
    request: pytest.FixtureRequest, tmp_path: Path
) -> "Iterator[NodeModuleWhoseDescribeHoldsTheLoad]":
    """A node module in a project directory of its own, whose describe parks the load.

    Name `<held_node_module.name>:LoadedFrameRelay` in a hand-written graph run
    with `project_directory=held_node_module.project_directory`, then
    `wait_until_the_load_reaches_the_import()` and `release_the_load()`.
    """
    project_directory = tmp_path / "held-describe-project"
    project_directory.mkdir()
    node_module = NodeModuleWhoseDescribeHoldsTheLoad(
        project_directory, "runtime_load_held_nodes_" + re.sub(r"\W", "_", request.node.name)
    )
    try:
        yield node_module
    finally:
        node_module.close()

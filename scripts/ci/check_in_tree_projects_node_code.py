# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Check the node code of every in-tree project with `tatolab check`, on the built runtime unit.

Every project in `examples/` and `packages/` is checked: each directory holding a
`tatolab-project.json`, and each first-party module that ships without one, checked in a
scratch project holding a copy of its package. A project whose own code holds no Python is
listed and not checked. Each project's `.venv` is made with `uv sync`, as the runtime makes a
project's, with `tatolab-stream` held at this checkout's version, so `UV_FIND_LINKS` reaches the
one built from this checkout even while PyPI holds a later release, or this version built from
older code.

The unit's `tatolabd` is started once, and `tatolab check --json --dir <project>` asked of it
for each project. A project fails when its check fails, or when its Python node declarations
were not checked to their end. A finding in a module under a `tests/` folder is left out: a
project's tests import what only its test environment holds, and no test ships. A declaration in a
retired form, a `declaration_retired` warning, fails its project too: every in-tree node is typed.

Exits 0 when every project passed, 1 when a project failed, and 2 when the run could not
check every project: a venv that would not sync, or a runtime that did not answer.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
from collections.abc import Iterator, Sequence
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath
from typing import Any

import tomllib

PROJECT_FILE_NAME = "tatolab-project.json"
"""The file that makes a directory a project."""

DIRECTORIES_HOLDING_IN_TREE_PROJECTS = ("examples", "packages")
"""Where in the repository the projects and first-party modules are."""

FIRST_PARTY_DISTRIBUTION_NAME_PREFIX = "tatolab-"
"""What every first-party module's distribution name starts with."""

PYTHON_NODE_DECLARATIONS_CHECK = "python_node_declarations"
"""The check whose `not_checked` fails a project."""

RETIRED_DECLARATION_FINDING_CODE = "declaration_retired"
"""The finding code of a node declared in a retired form."""

DIRECTORY_NAMES_NEVER_READ = frozenset({".venv", "__pycache__", "node_modules", "target", "tests", "imports"})
"""Folders whose Python is not a project's own node code."""

DIRECTORY_NAME_OF_TESTS = "tests"
"""The folder whose modules' findings are left out: a project's tests ship in no module or sample."""

DIRECTORY_NAMES_A_SCRATCH_COPY_LEAVES_OUT = (".venv", "__pycache__", "node_modules", "target", "tests")
"""What a module's scratch project does not copy from its package."""

PASSED_EXIT_CODE = 0
FAILED_EXIT_CODE = 1
NOT_FULLY_CHECKED_EXIT_CODE = 2

RUNTIME_ANSWER_WAIT_SECONDS = 60.0
"""How long the started `tatolabd` has to answer `tatolab streams`."""

VENV_SYNC_TIMEOUT_SECONDS = 1800.0
"""The longest one project's `uv sync` may take, native builds and large wheels included."""

TATOLAB_STREAM_PYPROJECT = Path("sdk") / "tatolab-stream" / "pyproject.toml"
"""Where, under the repository's root, this checkout's `tatolab-stream` states its version."""

CHECK_TIMEOUT_SECONDS = 180.0
"""The longest one `tatolab check` may take; the runtime bounds its own check at 60 seconds."""


@dataclass(frozen=True)
class InTreeProject:
    """A project, or a first-party module, whose node code the run checks."""

    name: str
    """Where it is, relative to the repository."""

    directory: Path
    """Its directory in the repository."""

    has_a_project_file: bool
    """Whether it is a project as it stands; a module without one is checked in a scratch project."""


@dataclass
class ProjectCheckOutcome:
    """What the run found of one project."""

    project: InTreeProject
    failed_because: list[str] = field(default_factory=list)
    """Each reason it fails: an error finding, or a check that did not run to its end."""

    not_fully_checked_because: list[str] = field(default_factory=list)
    """Each reason the run could not check it: a venv that would not sync, an unread answer."""


def in_tree_projects(repository: Path) -> list[InTreeProject]:
    """Every in-tree project and first-party module, sorted by where it is."""
    projects: list[InTreeProject] = []
    for holding_directory_name in DIRECTORIES_HOLDING_IN_TREE_PROJECTS:
        holding_directory = repository / holding_directory_name
        for directory_name, subdirectory_names, file_names in os.walk(holding_directory):
            subdirectory_names[:] = [
                name for name in subdirectory_names if not name.startswith(".") and name not in DIRECTORY_NAMES_NEVER_READ
            ]
            if PROJECT_FILE_NAME in file_names:
                directory = Path(directory_name)
                projects.append(InTreeProject(directory.relative_to(repository).as_posix(), directory, True))
        for package in sorted(holding_directory.iterdir()):
            if (
                package.is_dir()
                and not (package / PROJECT_FILE_NAME).is_file()
                and is_a_first_party_module(package)
            ):
                projects.append(InTreeProject(package.relative_to(repository).as_posix(), package, False))
    return sorted(projects, key=lambda project: project.name)


def is_a_first_party_module(package: Path) -> bool:
    """Whether `package` builds a first-party module: a `pyproject.toml` naming a `tatolab-` distribution."""
    pyproject = package / "pyproject.toml"
    if not pyproject.is_file():
        return False
    distribution_name = tomllib.loads(pyproject.read_text(encoding="utf-8")).get("project", {}).get("name", "")
    return distribution_name.startswith(FIRST_PARTY_DISTRIBUTION_NAME_PREFIX)


def holds_python_of_its_own(project: InTreeProject) -> bool:
    """Whether the project's own code, outside its imports, tests, build output and the projects
    nested in it, holds a `.py` file."""
    for directory, subdirectory_names, file_names in os.walk(project.directory):
        subdirectory_names[:] = [
            name
            for name in subdirectory_names
            if not name.startswith(".")
            and name not in DIRECTORY_NAMES_NEVER_READ
            and not (Path(directory) / name / PROJECT_FILE_NAME).is_file()
        ]
        if any(file_name.endswith(".py") for file_name in file_names):
            return True
    return False


def judge_a_check_answer(project: InTreeProject, answer: dict[str, Any]) -> ProjectCheckOutcome:
    """What `tatolab check --json` answered of `project`, judged: every error and every retired
    declaration outside its tests fails it, as does a Python node declaration check that did not
    run to its end."""
    outcome = ProjectCheckOutcome(project)
    for finding in answer.get("findings", []):
        if DIRECTORY_NAME_OF_TESTS in PurePosixPath(finding.get("path", "")).parts[:-1]:
            continue
        place = f"{finding.get('path', '?')}:{finding.get('line') or '?'}"
        if finding.get("severity") == "error":
            words = (finding.get("words") or "").replace("\n", "\n    ")
            outcome.failed_because.append(f"{place}: {finding.get('finding_code')}: {words}")
        elif finding.get("finding_code") == RETIRED_DECLARATION_FINDING_CODE:
            outcome.failed_because.append(
                f"{place}: {RETIRED_DECLARATION_FINDING_CODE}: {finding.get('node_type') or '?'}"
            )
    python_node_declaration_checks = [
        check for check in answer.get("checks", []) if check.get("check") == PYTHON_NODE_DECLARATIONS_CHECK
    ]
    if not python_node_declaration_checks:
        outcome.failed_because.append(f"{PYTHON_NODE_DECLARATIONS_CHECK}: the runtime did not run it")
    for check in python_node_declaration_checks:
        if check.get("state") not in ("passed", "failed"):
            why = check.get("not_checked_because") or {}
            outcome.failed_because.append(
                f"{PYTHON_NODE_DECLARATIONS_CHECK}: not checked: {why.get('refusal_code', '?')}: "
                f"{why.get('runtime_words', 'no reason given')}"
            )
    if answer.get("verdict") == "failed" and not any(
        finding.get("severity") == "error" for finding in answer.get("findings", [])
    ):
        outcome.failed_because.append("the check failed with no error finding")
    return outcome


def the_report(outcomes: Sequence[ProjectCheckOutcome], unchecked: Sequence[InTreeProject]) -> str:
    """Each project's failures, then the projects that hold no Python of their own."""
    lines: list[str] = []
    for outcome in outcomes:
        for reason in outcome.not_fully_checked_because:
            lines.append(f"{outcome.project.name}: not fully checked: {reason}")
        for reason in outcome.failed_because:
            lines.append(f"{outcome.project.name}: failed: {reason}")
    for project in unchecked:
        lines.append(f"{project.name}: holds no Python of its own, so no node code to check")
    failed = sum(1 for outcome in outcomes if outcome.failed_because)
    not_fully_checked = sum(1 for outcome in outcomes if outcome.not_fully_checked_because)
    lines.append(
        f"{len(outcomes)} projects checked: {failed} failed, {not_fully_checked} not fully checked"
    )
    return "\n".join(lines) + "\n"


def the_exit_code_of(outcomes: Sequence[ProjectCheckOutcome]) -> int:
    """1 when a project failed, else 2 when one was not fully checked, else 0."""
    if any(outcome.failed_because for outcome in outcomes):
        return FAILED_EXIT_CODE
    if any(outcome.not_fully_checked_because for outcome in outcomes):
        return NOT_FULLY_CHECKED_EXIT_CODE
    return PASSED_EXIT_CODE


class RuntimeUnit:
    """The built runtime unit's `tatolabd` and `tatolab`."""

    def __init__(self, unit_directory: Path) -> None:
        self.tatolabd = unit_directory / "bin" / "tatolabd"
        self.tatolab = unit_directory / "bin" / "tatolab"
        for executable in (self.tatolabd, self.tatolab):
            if not executable.is_file():
                raise SystemExit(f"{executable} is not there; build the unit with `cargo xtask build-runtime`")

    def tatolab_run(self, *arguments: str, timeout: float) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(self.tatolab), *arguments], capture_output=True, text=True, timeout=timeout, check=False
        )

    @contextmanager
    def serving(self) -> Iterator[None]:
        """Start `tatolabd` and yield it once it answers, stopping it afterwards."""
        process = subprocess.Popen(
            [str(self.tatolabd)], stdin=subprocess.DEVNULL, stdout=sys.stderr, stderr=sys.stderr, text=True
        )
        try:
            deadline = time.monotonic() + RUNTIME_ANSWER_WAIT_SECONDS
            while self.tatolab_run("streams", timeout=RUNTIME_ANSWER_WAIT_SECONDS).returncode != 0:
                if process.poll() is not None:
                    raise SystemExit(f"tatolabd exited with {process.returncode} before it answered")
                if time.monotonic() > deadline:
                    raise SystemExit(f"tatolabd did not answer within {RUNTIME_ANSWER_WAIT_SECONDS:.0f}s")
                time.sleep(0.5)
            yield
        finally:
            process.terminate()
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                process.kill()


def the_directory_to_check(project: InTreeProject, scratch_directory: Path) -> Path:
    """The project's own directory, or for a module without a project file a scratch project holding a copy of its package."""
    if project.has_a_project_file:
        return project.directory
    scratch_project = scratch_directory / project.directory.name
    shutil.copytree(
        project.directory, scratch_project, ignore=shutil.ignore_patterns(*DIRECTORY_NAMES_A_SCRATCH_COPY_LEAVES_OUT)
    )
    (scratch_project / PROJECT_FILE_NAME).write_text(
        json.dumps(
            {
                "dependencies": [],
                "description": f"{project.name}, checked as a project of its own",
                "name": project.directory.name,
                "project": str(uuid.uuid4()),
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    return scratch_project


def this_checkouts_tatolab_stream_version(repository: Path) -> str:
    """The version this checkout's `tatolab-stream` states."""
    return tomllib.loads((repository / TATOLAB_STREAM_PYPROJECT).read_text(encoding="utf-8"))["project"]["version"]


def the_venv_sync_command(directory: Path, tatolab_stream_version: str) -> list[str]:
    """`uv sync` of the project in `directory`, its `tatolab-stream` held at `tatolab_stream_version`."""
    return [
        "uv",
        "sync",
        "--quiet",
        "--project",
        str(directory),
        "--upgrade-package",
        f"tatolab-stream=={tatolab_stream_version}",
    ]


def check_a_project(
    runtime_unit: RuntimeUnit, project: InTreeProject, directory: Path, tatolab_stream_version: str
) -> ProjectCheckOutcome:
    """Make the project's `.venv` and ask the runtime to check its code; a step that fails or runs
    out of time leaves the project not fully checked, and the run goes on to the next."""
    outcome = ProjectCheckOutcome(project)
    try:
        synced = subprocess.run(
            the_venv_sync_command(directory, tatolab_stream_version),
            capture_output=True,
            text=True,
            timeout=VENV_SYNC_TIMEOUT_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired:
        outcome.not_fully_checked_because.append(f"`uv sync` ran past {VENV_SYNC_TIMEOUT_SECONDS:.0f}s")
        return outcome
    if synced.returncode != 0:
        outcome.not_fully_checked_because.append(f"`uv sync` failed: {synced.stderr.strip()[-2000:]}")
        return outcome
    try:
        checked = runtime_unit.tatolab_run("check", "--json", "--dir", str(directory), timeout=CHECK_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        outcome.not_fully_checked_because.append(f"`tatolab check` ran past {CHECK_TIMEOUT_SECONDS:.0f}s")
        return outcome
    try:
        answer = json.loads(checked.stdout)
    except json.JSONDecodeError:
        outcome.not_fully_checked_because.append(
            f"`tatolab check` exited {checked.returncode} with no answer: {checked.stderr.strip()[-2000:]}"
        )
        return outcome
    return judge_a_check_answer(project, answer)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    parser.add_argument("--runtime-unit", type=Path, required=True, help="the built runtime unit's directory")
    parser.add_argument(
        "--repository", type=Path, default=Path(__file__).resolve().parents[2], help="the repository's root"
    )
    parser.add_argument(
        "--project",
        type=Path,
        action="append",
        default=[],
        help="check only this project directory (repeatable) instead of every in-tree project",
    )
    arguments = parser.parse_args(argv)
    runtime_unit = RuntimeUnit(arguments.runtime_unit.resolve())
    tatolab_stream_version = this_checkouts_tatolab_stream_version(arguments.repository.resolve())
    projects = (
        [InTreeProject(str(directory), directory.resolve(), True) for directory in arguments.project]
        if arguments.project
        else in_tree_projects(arguments.repository.resolve())
    )
    to_check = [project for project in projects if holds_python_of_its_own(project)]
    unchecked = [project for project in projects if project not in to_check]
    outcomes: list[ProjectCheckOutcome] = []
    with tempfile.TemporaryDirectory(prefix="in-tree-project-check-") as scratch, runtime_unit.serving():
        for project in to_check:
            print(f"checking {project.name}", file=sys.stderr, flush=True)
            outcomes.append(
                check_a_project(
                    runtime_unit, project, the_directory_to_check(project, Path(scratch)), tatolab_stream_version
                )
            )
    sys.stdout.write(the_report(outcomes, unchecked))
    return the_exit_code_of(outcomes)


if __name__ == "__main__":
    sys.exit(main())

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What a test hands the runtime, turned into the `tatolab run` that loads it.

A stream reaches `tatolabd` only as a user's does: `tatolab run` from a project
directory, the runtime compiling the stream in the project's own
`.venv/bin/python` and loading the graph that compile printed. Three shapes
reach that call:

- a `@stream` function of one of the suite's own modules, run as
  `tatolab run <module>:<function>` from the suite directory, which is a project
  whose `.venv` is the suite venv;
- a graph written by hand — a dict, or text that may not even parse — run from
  a project of its own whose `.venv/bin/python` is a wrapper: the compile entry's
  invocation prints the given graph in the compile document, naming the project
  directory the graph's node modules import from; every other invocation, a
  describe or a processor interpreter, execs the interpreter it was given;
- `TatolabRunOfAProject`, a project directory and the arguments `tatolab run`
  takes there, verbatim.
"""

from __future__ import annotations

import json
import shlex
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from runtime_unit_under_test import STREAM_ON_RUNTIME_SUITE_DIRECTORY, SUITE_VENV_INTERPRETER

#: How the runtime starts a project's interpreter on the compile entry, ahead of its arguments.
COMPILE_ENTRY_INVOCATION = ("-I", "-m", "tatolab.stream._project_stream_compile_entry")

#: The compile document a hand-written graph's project prints, beside its `.venv`.
HAND_WRITTEN_COMPILE_DOCUMENT_FILE_NAME = "hand-written-compile-document.json"

#: Modules a compile interpreter must never import to reach a stream function:
#: a test module imports pytest, the harness and whatever it skips on.
MODULE_NAME_PREFIXES_A_STREAM_FUNCTION_IS_NOT_RUN_FROM = ("test_", "conftest")


@dataclass(frozen=True)
class TatolabRunOfAProject:
    """`tatolab run <tatolab_run_arguments...>` from `working_directory`, the project it names."""

    working_directory: Path
    tatolab_run_arguments: "tuple[str, ...]" = ()


#: A `@stream` function of a suite module, a hand-written graph (a dict, or its
#: text verbatim), or a project's own `tatolab run`.
StreamOrGraph = Any


class StreamFunctionNotRunnableFromTheSuiteProject(ValueError):
    """A `@stream` function the compile interpreter could not import by `<module>:<function>`."""


def tatolab_run_of_a_suite_stream_function(
    stream_function: "Callable[..., Any]", stream_name: "str | None"
) -> TatolabRunOfAProject:
    """`tatolab run <module>:<function> [--name <stream_name>]` from the suite directory.

    Refused by name for a function the project's compile interpreter could not
    import: one defined inside another function, in a test module, or in
    `__main__`. Move it to one of the suite's `*_streams.py` modules.
    """
    module_name = stream_function.__module__
    qualified_name = stream_function.__qualname__
    target = f"{module_name}:{qualified_name}"
    if "<locals>" in qualified_name or "." in qualified_name:
        raise StreamFunctionNotRunnableFromTheSuiteProject(
            f"the stream function {target} is not a module-level function, so `tatolab run "
            f"<module>:<function>` cannot import it; define it at module level in one of the "
            f"suite's `*_streams.py` modules, taking what varies from its config"
        )
    if module_name == "__main__" or module_name.startswith(
        MODULE_NAME_PREFIXES_A_STREAM_FUNCTION_IS_NOT_RUN_FROM
    ):
        raise StreamFunctionNotRunnableFromTheSuiteProject(
            f"the stream function {target} lives in `{module_name}`, which the project's "
            f"compile interpreter would have to import — pytest, the harness and every skip "
            f"with it; move it to one of the suite's `*_streams.py` modules"
        )
    module_file = STREAM_ON_RUNTIME_SUITE_DIRECTORY / f"{module_name.replace('.', '/')}.py"
    if not module_file.is_file():
        raise StreamFunctionNotRunnableFromTheSuiteProject(
            f"the stream function {target} lives in `{module_name}`, which is not a module "
            f"of the suite directory {STREAM_ON_RUNTIME_SUITE_DIRECTORY}, the project it is "
            f"run from"
        )
    tatolab_run_arguments: "tuple[str, ...]" = (target,)
    if stream_name is not None:
        tatolab_run_arguments += ("--name", stream_name)
    return TatolabRunOfAProject(
        working_directory=STREAM_ON_RUNTIME_SUITE_DIRECTORY,
        tatolab_run_arguments=tatolab_run_arguments,
    )


def graph_text_of(graph: "dict[str, Any] | str") -> str:
    """A hand-written graph's text: a dict as JSON, or text verbatim."""
    if isinstance(graph, str):
        return graph
    return json.dumps(graph, allow_nan=False)


@dataclass(frozen=True)
class HandWrittenGraphProject:
    """A project whose `.venv/bin/python` hands the runtime a graph written by hand."""

    project_directory: Path
    #: The wrapper the runtime starts as the project's interpreter.
    venv_interpreter: Path
    #: The project directory the compile document names, which the graph's
    #: Python node types are described and run from.
    reported_project_directory: Path
    #: What the wrapper execs for every invocation but the compile entry's.
    processor_interpreter: Path

    @property
    def tatolab_run(self) -> TatolabRunOfAProject:
        """`tatolab run` from the project, compiling its sole stream."""
        return TatolabRunOfAProject(working_directory=self.project_directory)


def write_a_hand_written_graph_project(
    project_directory: Path,
    graph: "dict[str, Any] | str",
    *,
    reported_project_directory: "Path | None" = None,
    processor_interpreter: "Path | None" = None,
) -> HandWrittenGraphProject:
    """Write a project at `project_directory` whose compile prints `graph` verbatim.

    `reported_project_directory` defaults to the suite directory, so the
    suite's node modules describe; `processor_interpreter` to the suite venv's.
    """
    reported_project_directory = reported_project_directory or STREAM_ON_RUNTIME_SUITE_DIRECTORY
    processor_interpreter = processor_interpreter or SUITE_VENV_INTERPRETER
    venv_interpreter = project_directory / ".venv" / "bin" / "python"
    venv_interpreter.parent.mkdir(parents=True)
    compile_document_path = project_directory / HAND_WRITTEN_COMPILE_DOCUMENT_FILE_NAME
    compile_document_path.write_text(
        '{"stream_graph": '
        + graph_text_of(graph)
        + ', "project_directory": '
        + json.dumps(str(reported_project_directory))
        + "}\n",
        encoding="utf-8",
    )
    first, second, third = (shlex.quote(argument) for argument in COMPILE_ENTRY_INVOCATION)
    venv_interpreter.write_text(
        "#!/bin/sh\n"
        f'if [ "$1" = {first} ] && [ "$2" = {second} ] && [ "$3" = {third} ]; then\n'
        f"    exec cat {shlex.quote(str(compile_document_path))}\n"
        "fi\n"
        f'exec {shlex.quote(str(processor_interpreter))} "$@"\n',
        encoding="utf-8",
    )
    venv_interpreter.chmod(0o755)
    return HandWrittenGraphProject(
        project_directory=project_directory,
        venv_interpreter=venv_interpreter,
        reported_project_directory=reported_project_directory,
        processor_interpreter=processor_interpreter,
    )

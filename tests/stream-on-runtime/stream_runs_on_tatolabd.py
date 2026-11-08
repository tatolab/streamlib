# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What a test hands the runtime, turned into the project `tatolab run --dir` loads.

A stream reaches `tatolabd` only as a user's does: `tatolab run --dir` queues a
load of a project, the runtime reading each stream asset's graph document under the
project and loading its graph, the project's Python node types run in its own
`.venv/bin/python`. Three shapes reach that call:

- a `@stream` function of one of the suite's own modules, compiled in this process
  and run as a one-stream project of its own: its graph document
  `streams/<function>.graph.json` beside a link to each of the suite directory's
  own entries, so its modules import in every processor interpreter, and a `.venv`
  linked to the suite venv;
- a graph written by hand — a dict, or text that may not even parse — written as
  the graph document of a project of its own, linked to the suite directory's
  entries, or written into `reported_project_directory`, whose node modules the
  graph names; the project's `.venv/bin/python` execs the interpreter it was given;
- `TatolabRunOfAProject`, a project directory, run as it is.

A test that writes a project's streams in Python writes its `stream.py`, and
`write_the_graph_documents_the_stream_py_compiles_to` compiles it in the suite venv to
the graph documents the runtime reads.
"""

from __future__ import annotations

import hashlib
import json
import shlex
import subprocess
import uuid
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from runtime_unit_under_test import (
    STREAM_ON_RUNTIME_SUITE_DIRECTORY,
    SUITE_VENV_INTERPRETER,
    SUITE_VENV_PREFIX,
)
from tatolab.stream._exposed_name_cast import (
    ExposedNameCastsToNothingError,
    cast_exposed_name_to_url_safe,
)
from tatolab.stream._stream_graph_builder import compile_stream_to_graph

#: The file that makes a directory a project: its UUID and its name.
PROJECT_FILE_NAME = "tatolab-project.json"

#: Where a project written here keeps its graph documents.
GRAPH_DOCUMENTS_DIRECTORY_NAME = "streams"

#: What every graph document's file name ends with; the stream's name is what comes before it.
GRAPH_DOCUMENT_FILE_SUFFIX = ".graph.json"

#: The top-level key of a graph document holding its stream asset's UUID.
GRAPH_DOCUMENT_ASSET_KEY = "asset"

#: The stream a hand-written graph is named when it names none of its own.
HAND_WRITTEN_STREAM_NAME_WHEN_THE_GRAPH_NAMES_NONE = "main"

#: How the suite compiles every `@stream` of a project's `stream.py`, in the suite venv.
STREAM_PY_COMPILE_INVOCATION = (
    "-I",
    "-m",
    "tatolab.stream._project_stream_compile_entry",
    "--verb",
    "run",
    "--every-stream",
)

#: Bounded: a compile imports a few modules and runs each `@stream` once.
STREAM_PY_COMPILE_TIMEOUT_SECONDS = 60.0

#: Longest a stream id is: the exposed-name cast's bound.
STREAM_ID_MAXIMUM_LENGTH = 63

#: How many hex digits of the stream's SHA-256 a stream id carries.
STREAM_ID_HASH_HEX_DIGITS = 6

#: Modules a stream function is never run from: a test module imports pytest, the
#: harness and whatever it skips on.
MODULE_NAME_PREFIXES_A_STREAM_FUNCTION_IS_NOT_RUN_FROM = ("test_", "conftest")

#: The suite directory's entries a project written here links none of: its own venv, what
#: Python and pytest write beside the modules, and what makes the project a project.
SUITE_DIRECTORY_ENTRIES_A_WRITTEN_PROJECT_LINKS_NONE_OF = frozenset(
    {".venv", "__pycache__", ".pytest_cache", PROJECT_FILE_NAME, GRAPH_DOCUMENTS_DIRECTORY_NAME}
)


@dataclass(frozen=True)
class TatolabRunOfAProject:
    """`tatolab run --dir <dir_as_typed>` from `working_directory`: a load of every stream
    of the project `--dir` names, the working directory itself unless `dir_as_typed` is given."""

    working_directory: Path
    dir_as_typed: "str | None" = None

    @property
    def dir_argument(self) -> str:
        """The `--dir` `tatolab run` and `dev` are given."""
        return self.dir_as_typed if self.dir_as_typed is not None else str(self.working_directory)


#: A `@stream` function of a suite module, a hand-written graph (a dict, or its
#: text verbatim), or a project's own `tatolab run`.
StreamOrGraph = Any


class StreamFunctionNotRunnableFromTheSuiteProject(ValueError):
    """A `@stream` function whose node classes a one-stream project could not import."""


def write_the_project_file_unless_there(
    project_directory: Path, project_name: "str | None" = None
) -> "dict[str, Any]":
    """The project file of `project_directory`, written with a fresh project UUID and
    `project_name` (default: the directory's name) when it holds none."""
    project_file_path = project_directory / PROJECT_FILE_NAME
    if project_file_path.is_file():
        return json.loads(project_file_path.read_text(encoding="utf-8"))
    project_file = {
        "project": str(uuid.uuid4()),
        "name": project_name if project_name is not None else project_directory.name,
        "dependencies": [],
    }
    project_directory.mkdir(parents=True, exist_ok=True)
    project_file_path.write_text(json.dumps(project_file), encoding="utf-8")
    return project_file


def graph_document_path_of(project_directory: Path, stream_name: str) -> Path:
    """Where the graph document of the stream `stream_name` sits in a project written here."""
    return (
        project_directory
        / GRAPH_DOCUMENTS_DIRECTORY_NAME
        / f"{stream_name}{GRAPH_DOCUMENT_FILE_SUFFIX}"
    )


def stream_asset_held_at(graph_document_path: Path) -> "str | None":
    """The stream asset UUID the graph document at `graph_document_path` holds, if it reads."""
    try:
        document = json.loads(graph_document_path.read_text(encoding="utf-8"))
    except (OSError, ValueError, RecursionError):
        return None
    asset = document.get(GRAPH_DOCUMENT_ASSET_KEY) if isinstance(document, dict) else None
    return asset if isinstance(asset, str) else None


def graph_nodes_holding_a_uuid_each(graph: "dict[str, Any]", asset: str) -> "dict[str, Any]":
    """`graph` with a UUID given to each node holding no `id`, as the runtime gives one: the
    same UUID for a node of one name in every write of one stream asset."""
    nodes = graph.get("nodes")
    if not isinstance(nodes, list):
        return graph
    asset_namespace = uuid.UUID(asset)
    return {
        **graph,
        "nodes": [
            {"id": str(uuid.uuid5(asset_namespace, str(node.get("name", "")))), **node}
            if isinstance(node, dict) and "id" not in node
            else node
            for node in nodes
        ],
    }


def graph_document_text_holding_an_asset(graph: "dict[str, Any] | str", asset: str) -> str:
    """`graph` as a graph document's text holding `asset` and a UUID for each node: a dict as
    JSON, text that parses as a JSON object holding no `asset` likewise, and any other text
    verbatim, so the runtime reads it as it was handed."""
    if isinstance(graph, str):
        try:
            parsed = json.loads(graph)
        except (ValueError, RecursionError):
            return graph
        if not isinstance(parsed, dict) or GRAPH_DOCUMENT_ASSET_KEY in parsed:
            return graph
        graph = parsed
    return json.dumps(
        {GRAPH_DOCUMENT_ASSET_KEY: asset, **graph_nodes_holding_a_uuid_each(graph, asset)},
        allow_nan=False,
    )


def write_a_graph_document(
    project_directory: Path, stream_name: str, graph: "dict[str, Any] | str"
) -> Path:
    """Write `graph` as the graph document of the stream `stream_name`, holding the stream
    asset UUID an earlier document of that stream held, else a fresh one; return its path."""
    graph_document_path = graph_document_path_of(project_directory, stream_name)
    asset = stream_asset_held_at(graph_document_path) or str(uuid.uuid4())
    graph_document_path.parent.mkdir(parents=True, exist_ok=True)
    graph_document_path.write_text(
        graph_document_text_holding_an_asset(graph, asset), encoding="utf-8"
    )
    return graph_document_path


def link_every_suite_directory_entry_into(project_directory: Path) -> None:
    """Link each of the suite directory's own entries into `project_directory`, so the
    suite's modules import in every processor interpreter run there."""
    for suite_entry in sorted(STREAM_ON_RUNTIME_SUITE_DIRECTORY.iterdir()):
        if suite_entry.name not in SUITE_DIRECTORY_ENTRIES_A_WRITTEN_PROJECT_LINKS_NONE_OF:
            (project_directory / suite_entry.name).symlink_to(suite_entry)


def write_a_one_stream_project_of_a_suite_stream_function(
    project_directory: Path, stream_function: "Callable[..., Any]"
) -> TatolabRunOfAProject:
    """Write a project at `project_directory` holding one graph document,
    `streams/<function>.graph.json`, the graph `stream_function` compiles to in this
    process; return the `tatolab run` of it.

    Refused by name for a function whose node classes the project could not import:
    one defined inside another function, in a test module, or in `__main__`.
    Move it to one of the suite's `*_streams.py` modules.
    """
    module_name = stream_function.__module__
    qualified_name = stream_function.__qualname__
    target = f"{module_name}:{qualified_name}"
    if "<locals>" in qualified_name or "." in qualified_name:
        raise StreamFunctionNotRunnableFromTheSuiteProject(
            f"the stream function {target} is not a module-level function, so a project "
            f"cannot import what it wires; define it at module level in one of the suite's "
            f"`*_streams.py` modules, taking what varies from its config"
        )
    if module_name == "__main__" or module_name.startswith(
        MODULE_NAME_PREFIXES_A_STREAM_FUNCTION_IS_NOT_RUN_FROM
    ):
        raise StreamFunctionNotRunnableFromTheSuiteProject(
            f"the stream function {target} lives in `{module_name}`, which every processor "
            f"interpreter of the project would have to import — pytest, the harness and every "
            f"skip with it; move it to one of the suite's `*_streams.py` modules"
        )
    module_file = STREAM_ON_RUNTIME_SUITE_DIRECTORY / f"{module_name.replace('.', '/')}.py"
    if not module_file.is_file():
        raise StreamFunctionNotRunnableFromTheSuiteProject(
            f"the stream function {target} lives in `{module_name}`, which is not a module "
            f"of the suite directory {STREAM_ON_RUNTIME_SUITE_DIRECTORY}, whose modules a "
            f"one-stream project links"
        )
    project_directory.mkdir(parents=True)
    (project_directory / ".venv").symlink_to(SUITE_VENV_PREFIX, target_is_directory=True)
    link_every_suite_directory_entry_into(project_directory)
    write_the_project_file_unless_there(project_directory)
    write_a_graph_document(
        project_directory, qualified_name, compile_stream_to_graph(stream_function)
    )
    return TatolabRunOfAProject(working_directory=project_directory)


def hand_written_stream_name_of(graph: "dict[str, Any] | str") -> str:
    """The stream a hand-written graph is the document of: its `stream` key, so two
    hand-written graphs of one project are two streams."""
    graph_document: Any = graph
    if isinstance(graph, str):
        try:
            graph_document = json.loads(graph)
        except (ValueError, RecursionError):
            graph_document = None
    stream_named = graph_document.get("stream") if isinstance(graph_document, dict) else None
    return (
        stream_named
        if isinstance(stream_named, str) and stream_named
        else HAND_WRITTEN_STREAM_NAME_WHEN_THE_GRAPH_NAMES_NONE
    )


def stream_id_named(project_name: str, stream_name: str, project: str, stream_asset: str) -> str:
    """The stream id the runtime records a stream under at its first load,
    `<project>-<stream>-<hash>`, written out from the plan's rule as the suite's own oracle:
    the project's name and the stream's, each cast, a part casting to nothing left out, both
    cut to fit 63 characters, and six hex digits of SHA-256 over `<project>\\n<stream asset>`."""
    location_hash = hashlib.sha256(f"{project}\n{stream_asset}".encode()).hexdigest()[
        :STREAM_ID_HASH_HEX_DIGITS
    ]
    cast_parts: "list[str]" = []
    for part in (project_name, stream_name):
        try:
            cast_parts.append(cast_exposed_name_to_url_safe(part))
        except ExposedNameCastsToNothingError:
            continue
    characters_left = STREAM_ID_MAXIMUM_LENGTH - STREAM_ID_HASH_HEX_DIGITS - len(cast_parts)
    if sum(map(len, cast_parts)) > characters_left:
        if len(cast_parts) == 1:
            shares = [characters_left]
        else:
            first_even_share = characters_left // 2
            second_even_share = characters_left - first_even_share
            first_length, second_length = map(len, cast_parts)
            if first_length <= first_even_share:
                shares = [first_length, characters_left - first_length]
            elif second_length <= second_even_share:
                shares = [characters_left - second_length, second_length]
            else:
                shares = [first_even_share, second_even_share]
        cast_parts = [
            cut_part
            for cut_part in (part[:share].rstrip("-") for part, share in zip(cast_parts, shares))
            if cut_part
        ]
    return "-".join([*cast_parts, location_hash])


def stream_id_of_the_stream_in(project_directory: Path, stream_name: str) -> str:
    """The stream id the stream `stream_name` of the project at `project_directory` is first
    recorded under, from its project file and its graph document's stream asset."""
    project_file = json.loads((project_directory / PROJECT_FILE_NAME).read_text(encoding="utf-8"))
    stream_asset = stream_asset_held_at(graph_document_path_of(project_directory, stream_name))
    assert stream_asset is not None, f"the stream {stream_name} of {project_directory} has no asset"
    return stream_id_named(project_file["name"], stream_name, project_file["project"], stream_asset)


def stream_id_of_a_one_stream_project(
    project_directory: Path, stream_function: "Callable[..., Any]"
) -> str:
    """The stream id the one-stream project of `stream_function` at `project_directory` loads
    under."""
    return stream_id_of_the_stream_in(project_directory, stream_function.__qualname__)


@dataclass(frozen=True)
class HandWrittenGraphProject:
    """A project holding a graph written by hand as its graph document."""

    project_directory: Path
    #: The graph document the graph was written as.
    graph_document_path: Path
    #: The project's `.venv/bin/python`, which execs `processor_interpreter`.
    venv_interpreter: Path
    #: What the project's interpreter execs for every invocation.
    processor_interpreter: Path
    #: The stream the graph document holds.
    stream_name: str

    @property
    def tatolab_run(self) -> TatolabRunOfAProject:
        """`tatolab run` from the project, loading its graph documents."""
        return TatolabRunOfAProject(working_directory=self.project_directory)

    @property
    def stream_id(self) -> str:
        """The stream id the graph's stream is first recorded under."""
        return stream_id_of_the_stream_in(self.project_directory, self.stream_name)


def write_a_hand_written_graph_project(
    project_directory: Path,
    graph: "dict[str, Any] | str",
    *,
    reported_project_directory: "Path | None" = None,
    processor_interpreter: "Path | None" = None,
) -> HandWrittenGraphProject:
    """Write `graph` as the graph document of the stream its `stream` names in a project.

    The project is `reported_project_directory` when one is given — whose node modules the
    graph names, and which keeps one project file across every graph written into it — else
    a new one at `project_directory`, linked to the suite directory's entries so the suite's
    node modules import. The project's `.venv/bin/python` execs `processor_interpreter`,
    default the suite venv's.
    """
    processor_interpreter = processor_interpreter or SUITE_VENV_INTERPRETER
    if reported_project_directory is None:
        project_directory.mkdir(parents=True)
        link_every_suite_directory_entry_into(project_directory)
    else:
        project_directory = reported_project_directory
        project_directory.mkdir(parents=True, exist_ok=True)
    venv_interpreter = project_directory / ".venv" / "bin" / "python"
    if not venv_interpreter.exists():
        venv_interpreter.parent.mkdir(parents=True)
        venv_interpreter.write_text(
            f'#!/bin/sh\nexec {shlex.quote(str(processor_interpreter))} "$@"\n', encoding="utf-8"
        )
        venv_interpreter.chmod(0o755)
    write_the_project_file_unless_there(project_directory)
    stream_name = hand_written_stream_name_of(graph)
    graph_document_path = write_a_graph_document(project_directory, stream_name, graph)
    return HandWrittenGraphProject(
        project_directory=project_directory,
        graph_document_path=graph_document_path,
        venv_interpreter=venv_interpreter,
        processor_interpreter=processor_interpreter,
        stream_name=stream_name,
    )


def write_the_graph_documents_the_stream_py_compiles_to(project_directory: Path) -> "list[str]":
    """Compile every `@stream` of the `stream.py` at `project_directory` in the suite venv and
    write each graph as `streams/<function>.graph.json` beside the project file, keeping the
    stream asset of a document already there and removing each document no `@stream` gives
    now; return the stream names. A `stream.py` that does not import, defines no `@stream` or
    holds one that raises fails the test with why."""
    finished = subprocess.run(
        [str(SUITE_VENV_INTERPRETER), *STREAM_PY_COMPILE_INVOCATION],
        cwd=project_directory,
        capture_output=True,
        text=True,
        timeout=STREAM_PY_COMPILE_TIMEOUT_SECONDS,
        check=False,
    )
    assert finished.returncode == 0, finished.stdout + finished.stderr
    compiled_document = json.loads(finished.stdout.splitlines()[-1])
    assert "streams" in compiled_document, compiled_document
    write_the_project_file_unless_there(project_directory)
    stream_names: "list[str]" = []
    for compiled_stream in compiled_document["streams"]:
        assert "stream_graph" in compiled_stream, compiled_stream
        stream_name = compiled_stream["stream_function"].rsplit(":", 1)[-1].rsplit(".", 1)[-1]
        write_a_graph_document(project_directory, stream_name, compiled_stream["stream_graph"])
        stream_names.append(stream_name)
    graph_documents_directory = project_directory / GRAPH_DOCUMENTS_DIRECTORY_NAME
    for graph_document_path in graph_documents_directory.glob(f"*{GRAPH_DOCUMENT_FILE_SUFFIX}"):
        if graph_document_path.name.removesuffix(GRAPH_DOCUMENT_FILE_SUFFIX) not in stream_names:
            graph_document_path.unlink()
    return stream_names


def stream_id_of_a_hand_written_graph(graph: "dict[str, Any] | str", *, project_directory: Path) -> str:
    """The stream id the hand-written `graph` written into the project at `project_directory`
    is first recorded under."""
    return stream_id_of_the_stream_in(project_directory, hand_written_stream_name_of(graph))

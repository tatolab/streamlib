# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
# streamlib:lint-logging:allow-file — stdout carries the one JSON document the runtime
# parses; neither stream is a log event.

"""The entry the runtime runs in a project's own interpreter to list its node types and graph generators.

The runtime starts it as `<venv python> -I -m tatolab.stream._project_node_type_listing_entry`
with the project directory as the working directory and, on stdin, one JSON object
`{"dependencies": [{"project", "folder", "version", "files": {path: source text}}]}`: the
Python files of each import as its folder under `imports/` holds them. Nothing is imported or run; every module is read from
source. stdout carries exactly one JSON object, `{"sources": [...]}`, and the exit code is 0.
Each source is the project itself outside `imports/`, an import or an installed distribution that declares
`tatolab.nodes` entry points, with the `node_types`, `graph_generators` and
`unreadable_modules` read from it. An import's node type is led by its folder,
`imports/<folder>/<module>:<qualname>`, the type a stream imports it from that folder by.
"""

from __future__ import annotations

import json
import os
import sys
import traceback
from collections.abc import Iterable, Iterator
from importlib import metadata
from pathlib import Path
from typing import Any, Optional

from ._node_type_source_reader import (
    module_name_of_a_source_path,
    read_node_types_and_graph_generators_from_module_sources,
)

__all__ = ["list_the_node_types_and_graph_generators_of_a_project", "main"]

INSTALLED_MODULE_NODES_ENTRY_POINT_GROUP = "tatolab.nodes"

PROJECT_SOURCE = "project"
DEPENDENCY_SOURCE = "dependency"
INSTALLED_DISTRIBUTION_SOURCE = "installed_distribution"
LISTING_FAILED_BECAUSE_DOCUMENT_KEY = "failed_because"

# Directories under a project that hold no node code of its own: its venv, its history,
# its uploads and every other dot-directory, and bytecode caches.
_PROJECT_DIRECTORY_NAMES_NEVER_READ = ("__pycache__", "node_modules")

# The project's own directory whose folders each hold an import, listed as its own source.
_PROJECT_IMPORTS_DIRECTORY_NAME = "imports"


def list_the_node_types_and_graph_generators_of_a_project(
    project_directory: Path,
    dependencies: Iterable[dict[str, Any]],
    installed_distribution_search_path: Optional[list[str]] = None,
) -> dict[str, Any]:
    """Every source's node types and graph generators: the project's, each dependency's, each installed distribution's."""
    sources: list[dict[str, Any]] = [
        {
            "source": PROJECT_SOURCE,
            **read_node_types_and_graph_generators_from_module_sources(
                _project_module_sources(project_directory)
            ),
        }
    ]
    for dependency in dependencies:
        read_from_source = read_node_types_and_graph_generators_from_module_sources(
            _dependency_module_sources(dependency["files"])
        )
        sources.append(
            {
                "source": DEPENDENCY_SOURCE,
                "project": dependency["project"],
                "folder": dependency["folder"],
                "version": dependency["version"],
                **read_from_source,
                "node_types": [
                    {
                        **node_type,
                        "type": _a_node_type_of_an_imports_own_code(
                            dependency["folder"], node_type["type"]
                        ),
                    }
                    for node_type in read_from_source["node_types"]
                ],
            }
        )
    sources.extend(_installed_distribution_sources(installed_distribution_search_path))
    return {"sources": sources}


def _a_node_type_of_an_imports_own_code(folder: str, class_import_path: str) -> str:
    """The type a stream names a class of an import's own node code by: led by its folder."""
    return f"{_PROJECT_IMPORTS_DIRECTORY_NAME}/{folder}/{class_import_path}"


def _project_module_sources(project_directory: Path) -> Iterator[tuple[str, str, str | OSError]]:
    for directory, subdirectory_names, file_names in os.walk(project_directory):
        is_the_project_root = Path(directory) == project_directory
        subdirectory_names[:] = sorted(
            name
            for name in subdirectory_names
            if not name.startswith(".")
            and name not in _PROJECT_DIRECTORY_NAMES_NEVER_READ
            and not (is_the_project_root and name == _PROJECT_IMPORTS_DIRECTORY_NAME)
        )
        for file_name in sorted(file_names):
            file_path = Path(directory) / file_name
            relative_path = file_path.relative_to(project_directory).as_posix()
            module_name = module_name_of_a_source_path(relative_path)
            if module_name is None:
                continue
            yield module_name, relative_path, _source_text_of(file_path)


def _dependency_module_sources(files: dict[str, str]) -> Iterator[tuple[str, str, str | OSError]]:
    for relative_path in sorted(files):
        module_name = module_name_of_a_source_path(relative_path)
        if module_name is not None:
            yield module_name, relative_path, files[relative_path]


def _installed_distribution_sources(
    search_path: Optional[list[str]],
) -> list[dict[str, Any]]:
    distributions = (
        metadata.distributions()
        if search_path is None
        else metadata.distributions(path=search_path)
    )
    sources: list[dict[str, Any]] = []
    seen_distribution_names: set[str] = set()
    for distribution in distributions:
        entry_points = [
            entry_point
            for entry_point in distribution.entry_points
            if entry_point.group == INSTALLED_MODULE_NODES_ENTRY_POINT_GROUP
        ]
        distribution_name = distribution.metadata["Name"] or ""
        if not entry_points or distribution_name in seen_distribution_names:
            continue
        seen_distribution_names.add(distribution_name)
        read_from_source = read_node_types_and_graph_generators_from_module_sources(
            _installed_distribution_module_sources(distribution)
        )
        sources.append(
            {
                "source": INSTALLED_DISTRIBUTION_SOURCE,
                "distribution": distribution_name,
                "distribution_version": distribution.version,
                **read_from_source,
                "node_types": _node_types_the_entry_points_name(
                    entry_points, read_from_source["node_types"]
                ),
            }
        )
    sources.sort(key=lambda source: source["distribution"])
    return sources


def _installed_distribution_module_sources(
    distribution: metadata.Distribution,
) -> Iterator[tuple[str, str, str | OSError]]:
    for distribution_file in sorted(distribution.files or [], key=str):
        relative_path = distribution_file.as_posix()
        if relative_path.startswith(".."):
            continue
        module_name = module_name_of_a_source_path(relative_path)
        if module_name is None:
            continue
        yield module_name, relative_path, _source_text_of(Path(str(distribution.locate_file(distribution_file))))


def _node_types_the_entry_points_name(
    entry_points: list[metadata.EntryPoint], node_types_read: list[dict[str, Any]]
) -> list[dict[str, Any]]:
    """Each `tatolab.nodes` entry point as the node type it names, found where it is declared."""
    node_types_by_type = {node_type["type"]: node_type for node_type in node_types_read}
    node_types_by_qualname: dict[str, list[dict[str, Any]]] = {}
    for node_type in node_types_read:
        node_types_by_qualname.setdefault(node_type["type"].split(":", 1)[1], []).append(node_type)
    named: dict[str, dict[str, Any]] = {}
    for entry_point in entry_points:
        import_path = entry_point.value.split("[", 1)[0].strip().replace(" ", "")
        if import_path in named:
            continue
        declared = node_types_by_type.get(import_path)
        if declared is None:
            declared_under_its_qualname = node_types_by_qualname.get(
                import_path.split(":", 1)[-1], []
            )
            if len(declared_under_its_qualname) == 1:
                declared = declared_under_its_qualname[0]
        named[import_path] = (
            {**declared, "type": import_path}
            if declared is not None
            else {
                "type": import_path,
                "kind": None,
                "description": "",
                "category": None,
                "inputs": None,
                "outputs": None,
                "path": None,
                "line": None,
            }
        )
    return [named[import_path] for import_path in sorted(named)]


def _source_text_of(file_path: Path) -> str | OSError:
    try:
        return file_path.read_bytes().decode("utf-8", errors="replace")
    except OSError as unreadable:
        return unreadable


def main() -> int:
    """Read the dependencies from stdin, list every source, and print the one document."""
    try:
        request = json.loads(sys.stdin.read() or "{}")
        document: dict[str, Any] = list_the_node_types_and_graph_generators_of_a_project(
            Path.cwd(), request.get("dependencies", [])
        )
    except Exception:
        document = {LISTING_FAILED_BECAUSE_DOCUMENT_KEY: traceback.format_exc()}
    sys.stdout.write(json.dumps(document, sort_keys=True) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())

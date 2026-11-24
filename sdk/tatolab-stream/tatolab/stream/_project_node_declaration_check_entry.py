# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
# no-print-in-python ignores this file: stdout carries the JSON lines the runtime parses and
# stderr what the project's code prints; neither is a log event.

"""The entry the runtime runs in a project's own interpreter to check its node declarations.

The runtime starts it as `<venv python> -I -m tatolab.stream._project_node_declaration_check_entry`
with the project directory as the working directory and, on stdin, one JSON object
`{"paths": null | [path, ...]}`. The modules checked are each one the node type listing names
in the project's own code outside `imports/` (a module declaring a node type or a graph
generator, or one that does not parse), or, with `paths`, the modules at those paths.

Each module is imported with the project directory first on `sys.path`, so the judge is the
reader that loads the node: each `NodeDeclarationRefusal` is an `error` finding with its code,
and any other failure to import is one `module_does_not_import` finding with its traceback. A
node declared in a retired form, and each retired name imported, is an `error` naming its
replacement, found where the source writes it; the import it fails is not reported again.

stdout carries one JSON object per line, each flushed as it is written:

- first `{"modules": [{"module", "path"}, ...], "checked_with": [...]}`, the modules in the
  order they are checked and what checks them;
- then, as each module is finished, `{"path", "content_sha256", "findings": [...]}`;
- or, when the check cannot run at all, one `{"failed_because": traceback}` in their place.

A run killed at its bound has written a line for each module it finished, so the module it
was importing is the first of `modules` with no line.
"""

from __future__ import annotations

import hashlib
import importlib
import json
import os
import platform
import sys
import traceback
from collections.abc import Callable, Iterable, Sequence
from importlib import metadata
from pathlib import Path, PurePosixPath
from typing import Any, BinaryIO, Optional

from ._entry_standard_output import carry_every_later_stdout_write_to_stderr
from ._node_type_source_reader import (
    module_name_of_a_source_path,
    read_node_types_and_graph_generators_from_module_sources,
    read_retired_declarations_from_module_source,
)
from ._pin_declaration import NodeDeclarationRefusal
from ._project_node_type_listing_entry import project_module_sources
from ._retired_declarations import RETIRED_DECLARATION_FINDING_CODE, RetiredImportRefusal

__all__ = ["check_the_node_declarations_of_a_project", "main"]

MODULES_LINE_KEY = "modules"
CHECKED_WITH_LINE_KEY = "checked_with"
CHECK_FAILED_BECAUSE_LINE_KEY = "failed_because"

MODULE_DOES_NOT_IMPORT_FINDING_CODE = "module_does_not_import"
PATH_NOT_A_MODULE_FINDING_CODE = "path_not_a_python_module"

ERROR_SEVERITY = "error"

_TATOLAB_STREAM_DISTRIBUTION = "tatolab-stream"

type LineWriter = Callable[[dict[str, Any]], None]
"""What writes one line of the check's output, given its object."""


def check_the_node_declarations_of_a_project(
    project_directory: Path, paths: Optional[Sequence[str]], write_line: LineWriter
) -> None:
    """Check each module of the project at `project_directory`, calling `write_line` with each line's object as it is ready."""
    modules = _the_modules_to_check(project_directory, paths)
    write_line(
        {
            MODULES_LINE_KEY: [{"module": module, "path": path} for module, path in modules],
            CHECKED_WITH_LINE_KEY: _what_checks_the_declarations(),
        }
    )
    for module, path in modules:
        source_bytes = _the_bytes_of(project_directory / path)
        write_line(
            {
                "path": path,
                "content_sha256": hashlib.sha256(source_bytes or b"").hexdigest(),
                "findings": _the_findings_of_one_module(project_directory, module, path, source_bytes),
            }
        )


def _the_modules_to_check(
    project_directory: Path, paths: Optional[Sequence[str]]
) -> list[tuple[Optional[str], str]]:
    """Each `(module name, path)` to check, in order; a path no module is named by has no module name."""
    if paths is not None:
        return [
            (module_name_of_a_source_path(path), path)
            for path in sorted({PurePosixPath(path).as_posix() for path in paths})
        ]
    listing = read_node_types_and_graph_generators_from_module_sources(
        project_module_sources(project_directory)
    )
    paths_the_listing_names = {
        row["path"]
        for rows in (listing["node_types"], listing["graph_generators"], listing["unreadable_modules"])
        for row in rows
    }
    return [
        (module_name_of_a_source_path(path), path) for path in sorted(paths_the_listing_names)
    ]


def _the_findings_of_one_module(
    project_directory: Path, module: Optional[str], path: str, source_bytes: Optional[bytes]
) -> list[dict[str, Any]]:
    if module is None:
        return [
            _finding(
                PATH_NOT_A_MODULE_FINDING_CODE,
                ERROR_SEVERITY,
                f"`{path}` is not a Python module an import names. To fix: check a `.py` file "
                f"whose folders and name are each a Python identifier.",
                path,
            )
        ]
    findings = _the_retired_declarations_of(module, path, source_bytes)
    import_finding = _the_finding_of_its_import(project_directory, module, path)
    if import_finding is not None and not (
        findings and import_finding["finding_code"] == RETIRED_DECLARATION_FINDING_CODE
    ):
        findings.append(import_finding)
    return findings


def _the_retired_declarations_of(
    module: str, path: str, source_bytes: Optional[bytes]
) -> list[dict[str, Any]]:
    if source_bytes is None:
        return []
    return [
        _finding(
            RETIRED_DECLARATION_FINDING_CODE,
            ERROR_SEVERITY,
            retired_declaration["words"],
            path,
            line=retired_declaration["line"],
            column=retired_declaration["column"],
            node_type=retired_declaration["node_type"],
        )
        for retired_declaration in read_retired_declarations_from_module_source(
            module, path, source_bytes.decode("utf-8", errors="replace")
        )
    ]


def _the_finding_of_its_import(
    project_directory: Path, module: str, path: str
) -> Optional[dict[str, Any]]:
    module_file = (project_directory / path).resolve()
    try:
        importlib.import_module(module)
    except NodeDeclarationRefusal as refusal:
        line, column = _the_place_in(refusal, module_file)
        return _finding(
            refusal.finding_code,
            ERROR_SEVERITY,
            str(refusal),
            path,
            line=line,
            column=column,
            node_type=f"{module}:{refusal.node}",
            port=refusal.pin,
        )
    except RetiredImportRefusal as refusal:
        line, column = _the_place_in(refusal, module_file)
        return _finding(
            RETIRED_DECLARATION_FINDING_CODE,
            ERROR_SEVERITY,
            str(refusal),
            path,
            line=line,
            column=column,
        )
    except BaseException as not_imported:  # noqa: BLE001 - the project's code may raise anything, `SystemExit` included
        line, column = _the_place_in(not_imported, module_file)
        return _finding(
            MODULE_DOES_NOT_IMPORT_FINDING_CODE,
            ERROR_SEVERITY,
            f"`{path}` does not import:\n{_the_traceback_inside_the_project(not_imported, project_directory)}",
            path,
            line=line,
            column=column,
        )
    return None


def _the_place_in(raised: BaseException, module_file: Path) -> tuple[Optional[int], Optional[int]]:
    """Where in `module_file` `raised` was raised from: a syntax error's own place, else the line of the innermost frame in the file."""
    if (
        isinstance(raised, SyntaxError)
        and raised.filename is not None
        and _is_the_same_file(raised.filename, module_file)
    ):
        return raised.lineno, raised.offset
    line: Optional[int] = None
    for frame, frame_line in traceback.walk_tb(raised.__traceback__):
        if _is_the_same_file(frame.f_code.co_filename, module_file):
            line = frame_line
    return line, None


def _is_the_same_file(file_name: str, module_file: Path) -> bool:
    try:
        return Path(file_name).resolve() == module_file
    except (OSError, ValueError):
        return False


def _the_traceback_inside_the_project(raised: BaseException, project_directory: Path) -> str:
    """`raised` formatted with only the frames of the project's own files, or every frame when none is."""
    rendered = traceback.TracebackException.from_exception(raised)
    project_root = project_directory.resolve()
    frames_inside = [
        frame for frame in rendered.stack if _lies_inside(Path(frame.filename), project_root)
    ]
    if frames_inside:
        rendered.stack = traceback.StackSummary.from_list(frames_inside)
    return "".join(rendered.format()).rstrip("\n")


def _lies_inside(file_path: Path, directory: Path) -> bool:
    # A frame of frozen or generated code is named `<frozen ...>`, which is no file.
    if not file_path.is_absolute():
        return False
    try:
        return file_path.resolve().is_relative_to(directory)
    except (OSError, ValueError):
        return False


def _finding(
    finding_code: str,
    severity: str,
    words: str,
    path: str,
    *,
    line: Optional[int] = None,
    column: Optional[int] = None,
    node_type: Optional[str] = None,
    port: Optional[str] = None,
) -> dict[str, Any]:
    return {
        "finding_code": finding_code,
        "severity": severity,
        "words": words,
        "path": path,
        "line": line,
        "column": column,
        "node_type": node_type,
        "port": port,
    }


def _the_bytes_of(file_path: Path) -> Optional[bytes]:
    try:
        return file_path.read_bytes()
    except OSError:
        return None


def _what_checks_the_declarations() -> list[str]:
    try:
        tatolab_stream_version = metadata.version(_TATOLAB_STREAM_DISTRIBUTION)
    except metadata.PackageNotFoundError:
        tatolab_stream_version = "of no installed version"
    return [
        f"{_TATOLAB_STREAM_DISTRIBUTION} {tatolab_stream_version}",
        f"Python {platform.python_version()}",
    ]


def _line_writer(entry_stdout: BinaryIO) -> LineWriter:
    def write_line(line_object: dict[str, Any]) -> None:
        entry_stdout.write((json.dumps(line_object, sort_keys=True) + "\n").encode("utf-8"))
        entry_stdout.flush()

    return write_line


def _the_paths_requested(request: dict[str, Any]) -> Optional[list[str]]:
    paths: Optional[Iterable[Any]] = request.get("paths")
    return None if paths is None else [str(path) for path in paths]


def main() -> int:
    """Read the request from stdin, check each module and write each line as it is ready."""
    project_directory = Path.cwd()
    sys.path.insert(0, str(project_directory))
    entry_stdout_descriptor = carry_every_later_stdout_write_to_stderr()
    with os.fdopen(entry_stdout_descriptor, "wb") as entry_stdout:
        write_line = _line_writer(entry_stdout)
        try:
            request = json.loads(sys.stdin.read() or "{}")
            check_the_node_declarations_of_a_project(
                project_directory, _the_paths_requested(request), write_line
            )
        except Exception:
            write_line({CHECK_FAILED_BECAUSE_LINE_KEY: traceback.format_exc()})
    return 0


if __name__ == "__main__":
    sys.exit(main())

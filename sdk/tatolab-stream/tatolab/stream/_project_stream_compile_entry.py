# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
# streamlib:lint-logging:allow-file — stdout carries the one JSON document `tatolab`
# parses and stderr what it shows the user; neither is a log event.

"""The compile entry `tatolab run` and `tatolab dev` run in the project's own interpreter.

`tatolab` starts it as `<venv python> -I -m tatolab.stream._project_stream_compile_entry
--verb {run,dev} [TARGET] [-f FILE] [--dir DIR] [--name NAME]`, with the anchor
directory as the working directory; `--dir` is only how the caller's command spelled
it. On success stdout carries exactly one JSON object,
`{"stream_graph": ..., "project_directory": ...}`, and the exit code is 0. A refusal
prints `error: <message>` to stderr and exits 1; an app or compile failure prints the
app's own traceback to stderr and exits 1; an app's deliberate `SystemExit` keeps its
code, so exit 0 with nothing on stdout is an app that chose to exit, and there is no
stream to start.

`-I` keeps the working directory off `sys.path` while this module and
`tatolab.stream` import, so a project module named like a standard-library one they
import (`json.py` at the anchor) cannot replace it; it also ignores every `PYTHON*`
variable and the user's site-packages, so the venv alone decides what imports.
"""

from __future__ import annotations

import argparse
import importlib
import importlib.util
import json
import os
import runpy
import sys
import traceback
from collections.abc import Sequence
from pathlib import Path
from typing import Any, Optional

from . import _stream_graph_builder
from ._cross_floor_check import (
    check_app_directory_for_floor_bindings,
    render_cross_floor_warning_block,
)
from ._project_stream_entry_execution import (
    execute_app_entry_file,
    import_stream_entry_module,
    project_directory_the_stream_entry_imports_from,
    refuse_a_stream_name_that_casts_to_nothing,
    select_stream_function,
    stream_functions_defined_in,
)
from ._project_stream_entry_resolution import (
    DEFAULT_STREAM_ENTRY_FILE_NAME,
    ProjectStreamCompileRefusalError,
    ResolvedStreamEntryModule,
    locate_stream_entry_module,
    resolve_stream_entry,
)
from ._stream_graph_builder import compile_stream_to_graph

__all__ = ["main"]

COMPILED_STREAM_GRAPH_DOCUMENT_KEY = "stream_graph"
COMPILED_PROJECT_DIRECTORY_DOCUMENT_KEY = "project_directory"

_PROJECT_STREAM_COMPILE_MODULE_FILE_NAMES = (
    "_project_stream_compile_entry.py",
    "_project_stream_entry_resolution.py",
    "_project_stream_entry_execution.py",
)


def _launcher_source_file_names() -> frozenset[str]:
    """The files whose frames sit between the compile entry and the app's own code.

    `<frozen runpy>` as well as `runpy.__file__`: since CPython 3.11 runpy is
    frozen into the binary and its frames report the former, so matching only
    the latter leaves its frames sitting on top of the user's own. The importlib
    frames are the same for a `<module>:<function>` target — `importlib.util` is
    frozen from 3.11 too — and the builder's are the call into the user's
    `@stream` function.
    """
    tatolab_stream_package_directory = Path(__file__).parent
    return frozenset(
        file_name
        for file_name in (
            *(
                str(tatolab_stream_package_directory / module_file_name)
                for module_file_name in _PROJECT_STREAM_COMPILE_MODULE_FILE_NAMES
            ),
            runpy.__file__,
            "<frozen runpy>",
            str(importlib.__file__),
            str(importlib.util.__file__),
            "<frozen importlib.util>",
            "<frozen importlib._bootstrap>",
            "<frozen importlib._bootstrap_external>",
            _stream_graph_builder.__file__,
        )
        if file_name is not None
    )


def print_app_failure(entry_described: str, app_failure: BaseException) -> None:
    """Print an app-side failure as the app's own traceback, on stderr.

    The launcher's frames are dropped from the head so the first line the user
    reads is in their code. A `SyntaxError` carries no frames from the file at
    all — the file never ran — and prints as CPython prints it.
    """
    app_traceback = app_failure.__traceback__
    launcher_files = _launcher_source_file_names()
    while app_traceback is not None and app_traceback.tb_frame.f_code.co_filename in launcher_files:
        app_traceback = app_traceback.tb_next

    print(f"error: {entry_described} failed", file=sys.stderr)
    traceback.print_exception(type(app_failure), app_failure, app_traceback, file=sys.stderr)


def build_argument_parser() -> argparse.ArgumentParser:
    """The compile entry's arguments: the verb `tatolab` was given and its target flags."""
    parser = argparse.ArgumentParser(
        prog="python -I -m tatolab.stream._project_stream_compile_entry",
        description=(
            f"Compile the stream `tatolab run` / `tatolab dev` names, in the project's own "
            f"interpreter, with the anchor directory as the working directory. Executes "
            f"`{DEFAULT_STREAM_ENTRY_FILE_NAME}` from the anchor, or the file named by `-f` "
            f"or TARGET, compiles its sole @stream function (or the one TARGET names), and "
            f"writes the stream graph and its project directory to stdout as one JSON object."
        ),
    )
    parser.add_argument(
        "--verb",
        required=True,
        choices=("run", "dev"),
        help="The `tatolab` verb being served, as refusals spell it.",
    )
    parser.add_argument(
        "requested_stream_target",
        nargs="?",
        metavar="TARGET",
        help=(
            "The stream to compile: `<file>.py[:<function>]` or `<module>:<function>` "
            f"(default: the sole @stream in {DEFAULT_STREAM_ENTRY_FILE_NAME})."
        ),
    )
    parser.add_argument(
        "-f",
        "--file",
        dest="requested_entry_file",
        type=Path,
        metavar="FILE",
        help=(
            f"Entry file, overriding the `{DEFAULT_STREAM_ENTRY_FILE_NAME}` convention; "
            f"not with TARGET."
        ),
    )
    parser.add_argument(
        "--dir",
        dest="anchor_directory_as_typed",
        metavar="DIR",
        help=(
            "The project directory as the `tatolab` command spelled it, quoted in suggested "
            "commands; the anchor itself is the working directory."
        ),
    )
    parser.add_argument(
        "--name",
        dest="requested_stream_name",
        metavar="NAME",
        help="Compile the stream under this name instead of its function's.",
    )
    return parser


def _carry_every_later_stdout_write_to_stderr() -> int:
    """Point fd 1 and `sys.stdout` at stderr for the rest of the process.

    Returns a descriptor on the real stdout, the only way the compiled document
    reaches it — so nothing the app arranges to run later, an `atexit` handler, a
    thread or a child still holding fd 1, can follow the document there.
    """
    sys.stdout.flush()
    compiled_document_stdout_descriptor = os.dup(sys.stdout.fileno())
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    sys.stdout = sys.stderr
    return compiled_document_stdout_descriptor


def _write_the_compiled_document(
    compiled_document_stdout_descriptor: int, compiled_document: dict[str, Any]
) -> None:
    with os.fdopen(compiled_document_stdout_descriptor, "wb") as compiled_document_stdout:
        compiled_document_stdout.write(
            (json.dumps(compiled_document, allow_nan=False) + "\n").encode("utf-8")
        )


def _print_the_cross_floor_warning_block(anchor_directory: Path) -> None:
    # Advisory, so a defect in the check itself is reported and never stops the compile.
    try:
        cross_floor_warning_block = render_cross_floor_warning_block(
            check_app_directory_for_floor_bindings(anchor_directory), anchor_directory
        )
    except Exception as cross_floor_check_failure:  # noqa: BLE001 — advisory only
        cross_floor_warning_block = (
            f"tatolab: the cross-floor check could not run: {cross_floor_check_failure!r}\n"
        )
    print(cross_floor_warning_block, end="", file=sys.stderr, flush=True)


def compile_the_requested_stream(
    verb: str,
    *,
    anchor_directory: Path,
    anchor_directory_as_typed: Optional[str],
    requested_entry_file: Optional[Path],
    requested_stream_target: Optional[str],
    requested_stream_name: Optional[str],
) -> Optional[dict[str, Any]]:
    """The compiled document, or `None` once an app failure's traceback is printed."""
    resolved_stream_entry = resolve_stream_entry(
        verb, anchor_directory, requested_entry_file, requested_stream_target
    )
    if requested_stream_name is not None:
        refuse_a_stream_name_that_casts_to_nothing(requested_stream_name)
    entry_described = resolved_stream_entry.described_for_the_user()

    _print_the_cross_floor_warning_block(anchor_directory)

    try:
        if isinstance(resolved_stream_entry, ResolvedStreamEntryModule):
            resolved_stream_entry = locate_stream_entry_module(
                verb, anchor_directory, anchor_directory_as_typed, resolved_stream_entry
            )
            entry_described = resolved_stream_entry.described_for_the_user()
            entry_namespace = import_stream_entry_module(resolved_stream_entry)
        else:
            entry_namespace = execute_app_entry_file(resolved_stream_entry.entry_file)
    except ProjectStreamCompileRefusalError:
        raise
    # SystemExit passes through: an app that calls `sys.exit()` at module scope
    # chose its exit code, and reporting that as a failure would override it.
    except Exception as entry_failure:  # noqa: BLE001 — reported as the app's own
        print_app_failure(entry_described, entry_failure)
        return None

    stream_function = select_stream_function(
        verb,
        resolved_stream_entry,
        anchor_directory,
        anchor_directory_as_typed,
        entry_namespace,
        stream_functions_defined_in(entry_namespace, str(entry_namespace.get("__name__", ""))),
    )
    try:
        stream_graph = compile_stream_to_graph(stream_function, name=requested_stream_name)
    except Exception as compile_failure:  # noqa: BLE001 — reported as the app's own
        print_app_failure(entry_described, compile_failure)
        return None

    return {
        COMPILED_STREAM_GRAPH_DOCUMENT_KEY: stream_graph,
        COMPILED_PROJECT_DIRECTORY_DOCUMENT_KEY: str(
            project_directory_the_stream_entry_imports_from(
                resolved_stream_entry, anchor_directory
            )
        ),
    }


def main(argv: Optional[Sequence[str]] = None) -> int:
    """Compile the requested stream and write its document to stdout; return the exit code."""
    arguments = build_argument_parser().parse_args(argv)

    compiled_document_stdout_descriptor = _carry_every_later_stdout_write_to_stderr()
    try:
        compiled_document = compile_the_requested_stream(
            arguments.verb,
            anchor_directory=Path.cwd(),
            anchor_directory_as_typed=arguments.anchor_directory_as_typed,
            requested_entry_file=arguments.requested_entry_file,
            requested_stream_target=arguments.requested_stream_target,
            requested_stream_name=arguments.requested_stream_name,
        )
    except ProjectStreamCompileRefusalError as refusal:
        os.close(compiled_document_stdout_descriptor)
        print(f"error: {refusal}", file=sys.stderr)
        return 1
    except BaseException:
        os.close(compiled_document_stdout_descriptor)
        raise
    if compiled_document is None:
        os.close(compiled_document_stdout_descriptor)
        return 1

    _write_the_compiled_document(compiled_document_stdout_descriptor, compiled_document)
    return 0

if __name__ == "__main__":
    sys.exit(main())

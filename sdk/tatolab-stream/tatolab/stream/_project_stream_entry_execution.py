# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Running a resolved stream entry in the project's interpreter: executing its file
or importing its module, and choosing the `@stream` function it names.
"""

from __future__ import annotations

import importlib
import runpy
import shlex
import sys
from collections.abc import Callable, Sequence
from pathlib import Path
from typing import Any, Optional

from ._exposed_name_cast import ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe
from ._project_stream_entry_resolution import (
    STREAM_FUNCTION_EXPLAINED_WITH_A_SAMPLE,
    ProjectStreamCompileRefusalError,
    ResolvedStreamEntry,
    ResolvedStreamEntryFile,
    ResolvedStreamEntryModule,
    stream_target_forms,
    tatolab_command_as_typed,
)
from ._stream_graph_builder import is_stream_function

__all__ = [
    "execute_app_entry_file",
    "import_stream_entry_module",
    "project_directory_the_stream_entry_imports_from",
    "refuse_a_stream_name_that_casts_to_nothing",
    "select_stream_function",
    "stream_functions_defined_in",
]

_STREAM_DESCRIPTION_ATTRIBUTE = "__streamlib_stream_description__"


def execute_app_entry_file(entry_file: Path) -> dict[str, Any]:
    """Execute the entry file and return its module namespace.

    Run under the name `__main__` with its own directory leading `sys.path`,
    which is what `python stream.py` does — so an app that imports its own
    `nodes/` package resolves it here exactly as it does there. `sys.argv` is
    narrowed to the entry file for the same reason: the launcher's own flags
    are not the app's.
    """
    entry_directory = str(entry_file.parent)
    if sys.path[:1] != [entry_directory]:
        sys.path.insert(0, entry_directory)

    launcher_argv = sys.argv
    sys.argv = [str(entry_file)]
    try:
        return runpy.run_path(str(entry_file), run_name="__main__")
    finally:
        sys.argv = launcher_argv


def import_stream_entry_module(located_stream_entry: ResolvedStreamEntryModule) -> dict[str, Any]:
    """Import a located `<module>:<function>` target's module and return its namespace.

    `sys.argv` is narrowed to the module's file, as `python -m` narrows it.
    """
    launcher_argv = sys.argv
    sys.argv = [
        str(located_stream_entry.entry_module_file)
        if located_stream_entry.entry_module_file is not None
        else located_stream_entry.entry_module_name
    ]
    try:
        return vars(importlib.import_module(located_stream_entry.entry_module_name))
    finally:
        sys.argv = launcher_argv


def stream_functions_defined_in(
    entry_namespace: dict[str, Any], defining_module_name: str
) -> list[Callable[..., Any]]:
    """The `@stream` functions the entry defines itself, each once, in definition order.

    A stream imported into the entry from another module is that module's: it is
    never the entry's sole stream nor listed among its streams, though a target
    naming the name it is bound at selects it. A second name bound to a stream is
    still one stream.
    """
    return list(
        dict.fromkeys(
            candidate
            for candidate in entry_namespace.values()
            if is_stream_function(candidate)
            and getattr(candidate, "__module__", None) == defining_module_name
        )
    )


def _stream_function_listing(stream_functions: Sequence[Callable[..., Any]]) -> str:
    listed_lines: list[str] = []
    for stream_function in stream_functions:
        description = getattr(stream_function, _STREAM_DESCRIPTION_ATTRIBUTE, "")
        first_description_line = description.splitlines()[0] if description else ""
        listed_lines.append(
            f"    {stream_function.__name__}"
            + (f" — {first_description_line}" if first_description_line else "")
        )
    return "\n".join(listed_lines)


def _entry_file_as_typed(entry_file: Path, anchor_directory: Path) -> str:
    """The entry file as a command line spells it: relative to the anchor, shell-quoted."""
    try:
        return shlex.quote(entry_file.relative_to(anchor_directory).as_posix())
    except ValueError:
        return shlex.quote(str(entry_file))


def _entry_as_typed_for_a_target(
    resolved_stream_entry: ResolvedStreamEntry, anchor_directory: Path
) -> str:
    if isinstance(resolved_stream_entry, ResolvedStreamEntryModule):
        return resolved_stream_entry.entry_module_name
    return _entry_file_as_typed(resolved_stream_entry.entry_file, anchor_directory)


def select_stream_function(
    verb: str,
    resolved_stream_entry: ResolvedStreamEntry,
    anchor_directory: Path,
    anchor_directory_as_typed: Optional[str],
    entry_namespace: dict[str, Any],
    stream_functions: list[Callable[..., Any]],
) -> Callable[..., Any]:
    """The stream the target named, else the entry's sole `@stream` function."""
    entry_described = resolved_stream_entry.described_for_the_user()
    entry_as_typed = _entry_as_typed_for_a_target(resolved_stream_entry, anchor_directory)
    tatolab_command = tatolab_command_as_typed(verb, anchor_directory_as_typed)
    stream_function_name = resolved_stream_entry.stream_function_name
    if stream_function_name is None:
        if len(stream_functions) == 1:
            return stream_functions[0]
        if not stream_functions:
            raise _no_stream_defined_refusal(verb, entry_described)
        raise ProjectStreamCompileRefusalError(
            f"{entry_described} defines {len(stream_functions)} @stream functions:\n"
            f"{_stream_function_listing(stream_functions)}\n"
            f"Name the one to launch: `{tatolab_command} {entry_as_typed}:<function>`."
        )

    named_value = entry_namespace.get(stream_function_name)
    if is_stream_function(named_value):
        return named_value
    if stream_function_name in entry_namespace:
        raise ProjectStreamCompileRefusalError(
            f"`{stream_function_name}` in {entry_described} is not a @stream function: "
            f"decorate it with `@stream` — a module-level `def "
            f"{stream_function_name}(stream_builder: StreamBuilder) -> None:` that adds "
            f"its nodes."
        )
    if stream_functions:
        raise ProjectStreamCompileRefusalError(
            f"{entry_described} defines no @stream function named "
            f"`{stream_function_name}`; it defines:\n"
            f"{_stream_function_listing(stream_functions)}\n"
            f"Name one of them: `{tatolab_command} {entry_as_typed}:<function>`."
        )
    raise ProjectStreamCompileRefusalError(
        f"{entry_described} defines no @stream function named "
        f"`{stream_function_name}`, nor any other. Make `{stream_function_name}` one: "
        f"`@stream` above a module-level `def {stream_function_name}(stream_builder: "
        f"StreamBuilder) -> None:` that adds its nodes."
    )


def _no_stream_defined_refusal(verb: str, entry_described: str) -> ProjectStreamCompileRefusalError:
    return ProjectStreamCompileRefusalError(
        f"{entry_described} defines no @stream function\n"
        f"{STREAM_FUNCTION_EXPLAINED_WITH_A_SAMPLE}"
        "\n"
        "Name another entry file with `-f <file>`, or the stream itself with "
        f"{stream_target_forms(verb)}."
    )


def refuse_a_stream_name_that_casts_to_nothing(requested_stream_name: str) -> None:
    """Refuse a `--name` the exposed-name cast leaves nothing of, before any entry runs."""
    try:
        cast_exposed_name_to_url_safe(requested_stream_name)
    except ExposedNameCastsToNothingError as casts_to_nothing:
        raise ProjectStreamCompileRefusalError(
            f"--name {requested_stream_name!r} cannot name a stream: {casts_to_nothing}"
        ) from casts_to_nothing


def project_directory_the_stream_entry_imports_from(
    resolved_stream_entry: ResolvedStreamEntry, anchor_directory: Path
) -> Path:
    """The directory the entry's own imports resolve from, which every processor
    interpreter of its stream starts in: an entry file's own directory, which
    `execute_app_entry_file` puts first on `sys.path`, or the anchor a module
    target was found under."""
    if isinstance(resolved_stream_entry, ResolvedStreamEntryFile):
        return resolved_stream_entry.entry_file.resolve().parent
    return anchor_directory.resolve()

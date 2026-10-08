# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Where a `tatolab run` / `tatolab dev` target points: the entry file or module, and
the stream function it names.

A target ending `.py`, with or without `:<function>`, is a file resolved against
the anchor directory; `<module>:<function>` is a module imported with the anchor
leading the import path; no target means `stream.py` at the anchor, or the file
`-f` names.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import shlex
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Optional, Union

__all__ = [
    "DEFAULT_STREAM_ENTRY_FILE_NAME",
    "ProjectStreamCompileRefusalError",
    "ResolvedStreamEntryFile",
    "ResolvedStreamEntryModule",
    "STREAM_FUNCTION_EXPLAINED_WITH_A_SAMPLE",
    "locate_stream_entry_module",
    "resolve_app_entry_file",
    "resolve_stream_entry",
    "stream_target_forms",
    "tatolab_command_as_typed",
]

DEFAULT_STREAM_ENTRY_FILE_NAME = "stream.py"
APP_PY_FILE_NAME_REFUSED_WHERE_STREAM_PY_IS_MISSING = "app.py"
STREAM_TARGET_FILE_SUFFIX = ".py"
STREAM_TARGET_FUNCTION_SEPARATOR = ":"
PROCESS_MAIN_MODULE_NAME = "__main__"

STREAM_FUNCTION_EXPLAINED_WITH_A_SAMPLE = (
    "A stream is a module-level function decorated `@stream` that adds, links and "
    "exposes its nodes on the `StreamBuilder` it is given:\n"
    "\n"
    "    from tatolab.stream import CameraSource, DisplayWindow, StreamBuilder, stream\n"
    "\n"
    "    @stream\n"
    "    def main(stream_builder: StreamBuilder) -> None:\n"
    "        source = stream_builder.add(CameraSource)\n"
    "        window = stream_builder.add(DisplayWindow)\n"
    '        stream_builder.connect(source.output("video"), window.input("video"))\n'
)


class ProjectStreamCompileRefusalError(Exception):
    """A compile refused before the stream's graph exists, with a message shaped for a terminal."""


@dataclass(frozen=True)
class ResolvedStreamEntryFile:
    """An entry file the compile executes, and the stream function its target named."""

    entry_file: Path
    stream_function_name: Optional[str]

    def described_for_the_user(self) -> str:
        """The entry as a message names it, quoted."""
        return f"`{self.entry_file}`"


@dataclass(frozen=True)
class ResolvedStreamEntryModule:
    """A `<module>:<function>` target, and its module's file once located."""

    entry_module_name: str
    stream_function_name: str
    entry_module_file: Optional[Path] = None

    def described_for_the_user(self) -> str:
        """The entry as a message names it, quoted, with its module's file once found."""
        if self.entry_module_file is None:
            return f"`{self.entry_module_name}`"
        return f"`{self.entry_module_name}` (`{self.entry_module_file}`)"


ResolvedStreamEntry = Union[ResolvedStreamEntryFile, ResolvedStreamEntryModule]


def tatolab_command_as_typed(verb: str, anchor_directory_as_typed: Optional[str]) -> str:
    """`tatolab <verb>`, keeping the `--dir` the target was resolved against as it was typed."""
    if anchor_directory_as_typed is None:
        return f"tatolab {verb}"
    return f"tatolab {verb} --dir {shlex.quote(anchor_directory_as_typed)}"


def stream_target_forms(verb: str) -> str:
    """The three target forms, each spelled as a `tatolab <verb>` command."""
    return (
        f"`tatolab {verb} <file>.py`, `tatolab {verb} <file>.py:<function>` or "
        f"`tatolab {verb} <module>:<function>`"
    )


def resolve_app_entry_file(
    verb: str, anchor_directory: Path, requested_entry_file: Optional[Path]
) -> Path:
    """The entry file `-f` names (relative to the anchor), else `stream.py` directly at the anchor."""
    if requested_entry_file is not None:
        return _resolve_named_entry_file(
            anchor_directory, requested_entry_file, f"-f {requested_entry_file}"
        )
    conventional_entry_file = anchor_directory / DEFAULT_STREAM_ENTRY_FILE_NAME
    if conventional_entry_file.is_file():
        return conventional_entry_file
    if (anchor_directory / APP_PY_FILE_NAME_REFUSED_WHERE_STREAM_PY_IS_MISSING).is_file():
        raise ProjectStreamCompileRefusalError(
            f"no `{DEFAULT_STREAM_ENTRY_FILE_NAME}` in `{anchor_directory}`, only an "
            f"`{APP_PY_FILE_NAME_REFUSED_WHERE_STREAM_PY_IS_MISSING}`\n"
            f"`tatolab {verb}` launches a @stream function from "
            f"`{DEFAULT_STREAM_ENTRY_FILE_NAME}`, and reads "
            f"`{APP_PY_FILE_NAME_REFUSED_WHERE_STREAM_PY_IS_MISSING}` "
            f"only when `-f` or a target names it.\n"
            f"{STREAM_FUNCTION_EXPLAINED_WITH_A_SAMPLE}"
            f"\n"
            f"Write the stream in `{DEFAULT_STREAM_ENTRY_FILE_NAME}`, or name the file "
            f"that defines it with `-f <file>` or "
            f"`tatolab {verb} <file>.py[:<function>]`."
        )
    raise ProjectStreamCompileRefusalError(
        f"no `{DEFAULT_STREAM_ENTRY_FILE_NAME}` in `{anchor_directory}`\n"
        f"`tatolab {verb}` reads `{DEFAULT_STREAM_ENTRY_FILE_NAME}` from this "
        f"directory only — it never searches parent directories.\n"
        f"Run it from your project root, point at one with `--dir <project-root>`, "
        f"or name the entry file with `-f <file>` or "
        f"`tatolab {verb} <file>.py[:<function>]`."
    )


def _resolve_named_entry_file(
    anchor_directory: Path, named_entry_file: Path, named_by: str
) -> Path:
    resolved_entry_file = (
        named_entry_file
        if named_entry_file.is_absolute()
        else anchor_directory / named_entry_file
    )
    if not resolved_entry_file.is_file():
        raise ProjectStreamCompileRefusalError(
            f"no entry file at `{resolved_entry_file}` (from `{named_by}`)"
        )
    return resolved_entry_file


def resolve_stream_entry(
    verb: str,
    anchor_directory: Path,
    requested_entry_file: Optional[Path],
    requested_stream_target: Optional[str],
) -> ResolvedStreamEntry:
    """Resolve the positional target, or `-f` / the convention when there is none."""
    if requested_stream_target is None:
        return ResolvedStreamEntryFile(
            entry_file=resolve_app_entry_file(verb, anchor_directory, requested_entry_file),
            stream_function_name=None,
        )
    if requested_entry_file is not None:
        raise ProjectStreamCompileRefusalError(
            f"`-f {requested_entry_file}` and `{requested_stream_target}` both name what "
            f"to launch — give one: `tatolab {verb} -f <file>`, or "
            f"{stream_target_forms(verb)}"
        )

    named_by = f"tatolab {verb} {requested_stream_target}"
    before_separator, separator, after_separator = requested_stream_target.rpartition(
        STREAM_TARGET_FUNCTION_SEPARATOR
    )
    if requested_stream_target.endswith(STREAM_TARGET_FILE_SUFFIX):
        return ResolvedStreamEntryFile(
            entry_file=_resolve_named_entry_file(
                anchor_directory, Path(requested_stream_target), named_by
            ),
            stream_function_name=None,
        )
    if separator and after_separator.isidentifier():
        if before_separator.endswith(STREAM_TARGET_FILE_SUFFIX):
            return ResolvedStreamEntryFile(
                entry_file=_resolve_named_entry_file(
                    anchor_directory, Path(before_separator), named_by
                ),
                stream_function_name=after_separator,
            )
        if all(part.isidentifier() for part in before_separator.split(".")):
            return ResolvedStreamEntryModule(
                entry_module_name=before_separator,
                stream_function_name=after_separator,
            )
    raise ProjectStreamCompileRefusalError(
        f"`{requested_stream_target}` names no stream: a target is a file ending "
        f"`{STREAM_TARGET_FILE_SUFFIX}`, optionally followed by `:<function>`, or an "
        f"importable `<module>:<function>` — {stream_target_forms(verb)}"
    )


def locate_stream_entry_module(
    verb: str,
    anchor_directory: Path,
    anchor_directory_as_typed: Optional[str],
    resolved_stream_entry: ResolvedStreamEntryModule,
) -> ResolvedStreamEntryModule:
    """Find a `<module>:<function>` target's module file, running only its parent packages.

    The anchor leads `sys.path` first, as an entry file's directory does.
    """
    entry_module_name = resolved_stream_entry.entry_module_name
    stream_function_name = resolved_stream_entry.stream_function_name
    named_by = f"tatolab {verb} {entry_module_name}:{stream_function_name}"
    anchor_import_root = str(anchor_directory)
    if sys.path[:1] != [anchor_import_root]:
        sys.path.insert(0, anchor_import_root)

    tatolab_command = tatolab_command_as_typed(verb, anchor_directory_as_typed)
    # The compile entry itself runs as `__main__`, so that name holds a module of
    # the launcher's, never one of the project's.
    if entry_module_name == PROCESS_MAIN_MODULE_NAME:
        raise _module_already_running_refusal(
            anchor_directory, named_by, entry_module_name, stream_function_name, tatolab_command
        )
    launcher_argv = sys.argv
    sys.argv = [entry_module_name]
    try:
        entry_module_spec = importlib.util.find_spec(entry_module_name)
    except ModuleNotFoundError as missing_module:
        if missing_module.name is None or not (
            entry_module_name == missing_module.name
            or entry_module_name.startswith(f"{missing_module.name}.")
        ):
            raise
        entry_module_spec = None
    finally:
        sys.argv = launcher_argv

    entry_module_file = (
        Path(entry_module_spec.origin)
        if entry_module_spec is not None
        and entry_module_spec.has_location
        and entry_module_spec.origin is not None
        else None
    )
    project_module_file = _project_module_file_named(anchor_directory, entry_module_name)
    if project_module_file is None:
        if entry_module_spec is None:
            raise ProjectStreamCompileRefusalError(
                f"no module `{entry_module_name}` is importable from `{anchor_directory}` "
                f"(from `{named_by}`). A module target imports with the project "
                f"directory first on the import path; name a file instead with "
                f"`tatolab {verb} <file>.py:<function>`."
            )
    elif entry_module_file is None or not entry_module_file.resolve().is_relative_to(
        anchor_directory.resolve()
    ):
        raise _shadowed_project_module_refusal(
            anchor_directory,
            named_by,
            entry_module_name,
            entry_module_spec,
            project_module_file,
            f"{tatolab_command} {project_module_file.as_posix()}:{stream_function_name}",
        )
    return ResolvedStreamEntryModule(
        entry_module_name=entry_module_name,
        stream_function_name=stream_function_name,
        entry_module_file=entry_module_file,
    )


def _module_already_running_refusal(
    anchor_directory: Path,
    named_by: str,
    entry_module_name: str,
    stream_function_name: str,
    tatolab_command: str,
) -> ProjectStreamCompileRefusalError:
    return ProjectStreamCompileRefusalError(
        f"`{entry_module_name}` (from `{named_by}`) is a module already running, not "
        f"one the project defines — the process's own `__main__` is one — so it names "
        f"no file in `{anchor_directory}`. Name the file that defines the stream "
        f"instead: `{tatolab_command} <file>.py:{stream_function_name}`."
    )


def _shadowed_project_module_refusal(
    anchor_directory: Path,
    named_by: str,
    entry_module_name: str,
    entry_module_spec: Optional[importlib.machinery.ModuleSpec],
    project_module_file: Path,
    launch_of_the_project_module_file: str,
) -> ProjectStreamCompileRefusalError:
    """Refuse a module target whose name something outside the project answers first.

    The anchor leads the path search, but a built-in module and one already in
    `sys.modules` are answered before any path is searched — for the module
    itself, or for a parent package it is then searched inside.
    """
    project_module_as_typed = project_module_file.as_posix()
    launch_its_file_instead = (
        f"or launch its file instead: `{launch_of_the_project_module_file}`."
    )
    if entry_module_spec is not None:
        return ProjectStreamCompileRefusalError(
            f"`{entry_module_name}` (from `{named_by}`) resolves to "
            f"`{entry_module_spec.origin}`, not to `{project_module_as_typed}` in "
            f"`{anchor_directory}`: a module built into Python, or one already imported, "
            f"holds that name first. Rename the project's module, {launch_its_file_instead}"
        )
    does_not_resolve_to_the_project_module = (
        f"`{entry_module_name}` (from `{named_by}`) does not resolve to "
        f"`{project_module_as_typed}` in `{anchor_directory}`"
    )
    module_name_parts = entry_module_name.split(".")
    for parent_depth in range(1, len(module_name_parts)):
        parent_package_name = ".".join(module_name_parts[:parent_depth])
        parent_package = sys.modules.get(parent_package_name)
        if parent_package is None:
            continue
        project_package_directory = anchor_directory.joinpath(
            *module_name_parts[:parent_depth]
        ).resolve()
        parent_package_search_directories = {
            Path(search_directory).resolve()
            for search_directory in getattr(parent_package, "__path__", [])
        }
        if project_package_directory not in parent_package_search_directories:
            parent_package_file = getattr(parent_package, "__file__", None)
            parent_package_resolved_to = (
                f"`{parent_package_file}`" if parent_package_file else "a module with no file"
            )
            return ProjectStreamCompileRefusalError(
                f"{does_not_resolve_to_the_project_module}: its parent package "
                f"`{parent_package_name}` resolves to {parent_package_resolved_to}, "
                f"outside the project, so the project's `{parent_package_name}` is never "
                f"searched. Rename the project's package, {launch_its_file_instead}"
            )
    return ProjectStreamCompileRefusalError(
        f"{does_not_resolve_to_the_project_module}: a module outside the project holds "
        f"a name on its path first. Rename the project's module, {launch_its_file_instead}"
    )


def _project_module_file_named(anchor_directory: Path, entry_module_name: str) -> Optional[Path]:
    """The file under `anchor_directory` a dotted module name spells, relative to it."""
    module_path = Path(*entry_module_name.split("."))
    for project_module_file in (
        module_path / "__init__.py",
        module_path.with_suffix(".py"),
    ):
        if (anchor_directory / project_module_file).is_file():
            return project_module_file
    return None

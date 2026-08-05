# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `streamlib` console script: `new`, `run`, and `dev`.

`run` and `dev` are a thin runner over the engine this wheel already exposes.
They resolve the app's entry file, execute it exactly as `python app.py` would,
call its `setup(rt)`, and block in `rt.run()` — so the launched app and a
hand-run script are the same arrangement, not two. `new` writes an app that
works before the user has written anything.
"""

from __future__ import annotations

import argparse
import re
import runpy
import sys
import traceback
from pathlib import Path
from typing import Any, Callable, Optional, Sequence

from . import Runtime, arm_gil_hold_watchdog

__all__ = ["main"]

DEFAULT_APP_ENTRY_FILE_NAME = "app.py"
APP_SETUP_FUNCTION_NAME = "setup"

DEFAULT_CONTROL_PLANE_BIND_HOST = "0.0.0.0"
DEFAULT_CONTROL_PLANE_BIND_PORT = 9000

SCAFFOLD_PYTHON_VERSION = "3.12"
STREAMLIB_SIMPLE_INDEX_URL = "https://tatolab.github.io/streamlib/simple/"


class AppLaunchError(Exception):
    """A launch that failed before any engine was built.

    Carries a message already shaped for a terminal — the caller prints it and
    exits rather than raising a traceback at a user who did nothing wrong.
    """


def resolve_app_anchor_directory(requested_anchor_directory: Optional[Path]) -> Path:
    """Resolve the app root: `--dir` when given, else the exact CWD.

    No walk-up, deliberately: inside a monorepo a walk-up makes "which app am I
    in" ambiguous.
    """
    if requested_anchor_directory is not None:
        return requested_anchor_directory
    return Path.cwd()


def resolve_app_entry_file(
    verb: str, anchor_directory: Path, requested_entry_file: Optional[Path]
) -> Path:
    """Resolve the entry file whose `setup(rt)` builds the graph.

    `requested_entry_file` outright (relative paths against `anchor_directory`),
    else [`DEFAULT_APP_ENTRY_FILE_NAME`] directly at `anchor_directory`.
    """
    if requested_entry_file is None:
        conventional_entry_file = anchor_directory / DEFAULT_APP_ENTRY_FILE_NAME
        if not conventional_entry_file.is_file():
            raise AppLaunchError(
                f"no `{DEFAULT_APP_ENTRY_FILE_NAME}` in `{anchor_directory}`\n"
                f"`streamlib {verb}` reads `{DEFAULT_APP_ENTRY_FILE_NAME}` from this "
                f"directory only — it never searches parent directories.\n"
                f"Run it from your app root, point at one with `--dir <app-root>`, or "
                f"name the entry file with `-f <file>`."
            )
        return conventional_entry_file

    if requested_entry_file.is_absolute():
        resolved_entry_file = requested_entry_file
    else:
        resolved_entry_file = anchor_directory / requested_entry_file
    if not resolved_entry_file.is_file():
        raise AppLaunchError(
            f"no entry file at `{resolved_entry_file}` (from `-f {requested_entry_file}`)"
        )
    return resolved_entry_file


def _launcher_source_file_names() -> "frozenset[str]":
    """The files whose frames are this launcher's, not the app's."""
    return frozenset({__file__, runpy.__file__})


def print_app_failure(entry_file: Path, app_failure: BaseException) -> None:
    """Print an app-side failure as the app's own traceback.

    The launcher's frames are dropped from the head so the first line the user
    reads is in their code. A `SyntaxError` carries no frames from the file at
    all — the file never ran — and prints as CPython prints it.
    """
    app_traceback = app_failure.__traceback__
    launcher_files = _launcher_source_file_names()
    while (
        app_traceback is not None
        and app_traceback.tb_frame.f_code.co_filename in launcher_files
    ):
        app_traceback = app_traceback.tb_next

    print(f"error: `{entry_file}` failed", file=sys.stderr)
    traceback.print_exception(
        type(app_failure), app_failure, app_traceback, file=sys.stderr
    )


def execute_app_entry_file(entry_file: Path) -> "dict[str, Any]":
    """Execute the entry file and return its module namespace.

    Run under the name `__main__` with its own directory leading `sys.path`,
    which is what `python app.py` does — so an app that imports its own
    `processors/` package resolves it here exactly as it does there.
    """
    entry_directory = str(entry_file.parent)
    if sys.path[:1] != [entry_directory]:
        sys.path.insert(0, entry_directory)
    return runpy.run_path(str(entry_file), run_name="__main__")


def read_app_setup_function(
    entry_namespace: "dict[str, Any]", entry_file: Path
) -> "Callable[[Runtime], None]":
    """Take `setup` out of the executed entry namespace.

    The convention is the whole contract — a missing or non-callable `setup` is
    the one thing an otherwise-valid entry file can get wrong, so it is named
    rather than surfacing as `NoneType is not callable` from inside the runner.
    """
    app_setup_function = entry_namespace.get(APP_SETUP_FUNCTION_NAME)
    if app_setup_function is None:
        raise AppLaunchError(
            f"`{entry_file}` defines no `{APP_SETUP_FUNCTION_NAME}(rt)`\n"
            f"An app's entry file declares its pipeline in a function named "
            f"`{APP_SETUP_FUNCTION_NAME}` taking the runtime:\n"
            f"\n"
            f"    def {APP_SETUP_FUNCTION_NAME}(rt):\n"
            f"        source = rt.add(CameraSource)\n"
            f"        window = rt.add(DisplayWindow)\n"
            f"        rt.connect(source.output(\"video\"), window.input(\"video\"))\n"
        )
    if not callable(app_setup_function):
        raise AppLaunchError(
            f"`{entry_file}` defines `{APP_SETUP_FUNCTION_NAME}` as "
            f"{type(app_setup_function).__name__}, not a function taking the runtime"
        )
    return app_setup_function


def launch_app_node(
    verb: str,
    *,
    requested_anchor_directory: Optional[Path],
    requested_entry_file: Optional[Path],
    bind_host: str,
    bind_port: int,
    node_name: Optional[str],
) -> int:
    """Boot the app's node and own its run loop until the user interrupts it."""
    anchor_directory = resolve_app_anchor_directory(requested_anchor_directory)
    entry_file = resolve_app_entry_file(verb, anchor_directory, requested_entry_file)

    # What `dev` is: the same launch as `run`, plus the diagnostics an author
    # iterating wants and a deployed node should not pay for.
    if verb == "dev":
        arm_gil_hold_watchdog()

    try:
        entry_namespace = execute_app_entry_file(entry_file)
    except AppLaunchError:
        raise
    except BaseException as entry_failure:  # noqa: BLE001 — reported as the app's own
        print_app_failure(entry_file, entry_failure)
        return 1

    app_setup_function = read_app_setup_function(entry_namespace, entry_file)

    # Constructed only once the app's code has run: a file that cannot even be
    # executed must not cost a GPU context and an engine boot on the way to its
    # error message.
    runtime = Runtime()
    try:
        app_setup_function(runtime)
    except BaseException as setup_failure:  # noqa: BLE001 — reported as the app's own
        print_app_failure(entry_file, setup_failure)
        runtime.shutdown()
        return 1

    # After `setup`, so an app that failed to build its graph publishes no node
    # entry for `streamlib nodes` to find.
    runtime.host_control_plane(
        bind_host=bind_host, bind_port=bind_port, node_name=node_name
    )
    runtime.run()
    return 0


def _python_distribution_name_for(directory_name: str) -> str:
    """A PEP 503 name for the scaffolded project, from its directory name."""
    normalized = re.sub(r"[^A-Za-z0-9._-]+", "-", directory_name).strip("-.")
    return normalized.lower() or "streamlib-app"


def _scaffolded_app_entry_source(*, source_class_name: str) -> str:
    return f'''"""A StreamLib app: camera → effect → window.

`streamlib dev` finds `setup(rt)` below by convention — there is no manifest and
no `main()`. Edit `InvertingEffect`, re-run `streamlib dev`, and see the change.
"""

import numpy

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    {source_class_name},
    DisplayWindow,
    Runtime,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    output,
    processor,
)


@processor
class InvertingEffect:
    """Reads each frame, inverts its colors in place, and passes it on."""

    @input(delivery_profile="latest")
    def video_from_upstream(self) -> None: ...

    @output()
    def video_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None:
            return
        frame = VideoFrame.from_bag(bag)
        # The frame arrives as a surface id, not pixels: resolve it, open CPU
        # access, and edit the engine's own memory in place.
        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
            surface.lock(read_only=False)
            pixels = surface.as_numpy()
            # Color channels only — inverting alpha would erase the picture.
            numpy.subtract(255, pixels[:, :, :3], out=pixels[:, :, :3])
            surface.unlock()
        ctx.outputs.write("video_to_downstream", bag)


def setup(rt: Runtime) -> None:
    source = rt.add({source_class_name})
    effect = rt.add(InvertingEffect)
    window = rt.add(DisplayWindow, config={{"title": "StreamLib", "scaling": "fit"}})

    rt.connect(source.output("video"), effect.input("video_from_upstream"))
    rt.connect(effect.output("video_to_downstream"), window.input("video"))
'''


def _scaffolded_project_manifest(distribution_name: str) -> str:
    return f'''[project]
name = "{distribution_name}"
version = "0.1.0"
requires-python = ">={SCAFFOLD_PYTHON_VERSION}"
dependencies = ["streamlib", "numpy>=2.1"]

# streamlib is served from its own simple index until the PyPI publication that
# follows the project rename; everything else resolves from PyPI as usual.
[[tool.uv.index]]
name = "streamlib"
url = "{STREAMLIB_SIMPLE_INDEX_URL}"
explicit = true

[tool.uv.sources]
streamlib = {{ index = "streamlib" }}
'''


SCAFFOLDED_GITIGNORE = """.venv/
__pycache__/
*.py[cod]
"""


def scaffold_new_app(target_directory: Path, *, use_test_pattern_source: bool) -> int:
    """Write a working app into `target_directory`."""
    source_class_name = (
        "TestPatternSource" if use_test_pattern_source else "CameraSource"
    )
    scaffolded_files = {
        DEFAULT_APP_ENTRY_FILE_NAME: _scaffolded_app_entry_source(
            source_class_name=source_class_name
        ),
        "pyproject.toml": _scaffolded_project_manifest(
            _python_distribution_name_for(target_directory.resolve().name)
        ),
        ".python-version": f"{SCAFFOLD_PYTHON_VERSION}\n",
        ".gitignore": SCAFFOLDED_GITIGNORE,
    }

    # Checked before anything is written: a half-scaffolded directory is worse
    # than a refusal, and the user's own `app.py` is the file most likely to
    # already be there.
    already_present = sorted(
        name for name in scaffolded_files if (target_directory / name).exists()
    )
    if already_present:
        raise AppLaunchError(
            f"`{target_directory}` already has {', '.join(already_present)} — "
            f"scaffolding would overwrite it. Pick an empty directory."
        )

    target_directory.mkdir(parents=True, exist_ok=True)
    for file_name, contents in scaffolded_files.items():
        (target_directory / file_name).write_text(contents, encoding="utf-8")

    print(f"Created a StreamLib app in `{target_directory}`.\n")
    print("Next:")
    print(f"    cd {target_directory}")
    print(f"    uv venv --python {SCAFFOLD_PYTHON_VERSION} && uv sync")
    print("    streamlib dev")
    return 0


def build_argument_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="streamlib",
        description="StreamLib — a realtime streaming engine with Python authoring.",
    )
    subcommands = parser.add_subparsers(dest="verb", required=True)

    new_command = subcommands.add_parser(
        "new",
        help="Scaffold a new StreamLib app.",
        description=(
            "Write app.py, pyproject.toml, .python-version and .gitignore into "
            "DIRECTORY — a working camera → effect → window pipeline."
        ),
    )
    new_command.add_argument(
        "directory", type=Path, help="Directory to scaffold the app into."
    )
    new_command.add_argument(
        "--test-pattern",
        action="store_true",
        help=(
            "Wire the built-in test pattern instead of the camera, so the app "
            "runs on a machine with no capture device."
        ),
    )

    for launch_verb, summary in (
        ("run", "Boot this app as a StreamLib node."),
        ("dev", "Boot this app as a StreamLib node for development."),
    ):
        launch_command = subcommands.add_parser(
            launch_verb,
            help=summary,
            description=(
                f"{summary} Reads `{DEFAULT_APP_ENTRY_FILE_NAME}` from the anchor "
                f"directory — `--dir` when given, else the exact CWD, never a "
                f"parent — or the file named by `-f`, and calls its "
                f"`{APP_SETUP_FUNCTION_NAME}(rt)`. Runs until interrupted."
            ),
        )
        launch_command.add_argument(
            "-f",
            "--file",
            dest="entry_file",
            type=Path,
            metavar="FILE",
            help=(
                f"Entry file to launch, overriding the "
                f"`{DEFAULT_APP_ENTRY_FILE_NAME}` convention."
            ),
        )
        launch_command.add_argument(
            "--dir",
            dest="anchor_directory",
            type=Path,
            metavar="DIR",
            help="App root to resolve the entry file against (default: CWD, no walk-up).",
        )
        launch_command.add_argument(
            "--host",
            dest="bind_host",
            default=DEFAULT_CONTROL_PLANE_BIND_HOST,
            metavar="HOST",
            help="Host address to bind the control plane to (default: all interfaces).",
        )
        launch_command.add_argument(
            "-p",
            "--port",
            dest="bind_port",
            type=int,
            default=DEFAULT_CONTROL_PLANE_BIND_PORT,
            metavar="PORT",
            help="Port for the control plane; increments on collision.",
        )
        launch_command.add_argument(
            "--name",
            dest="node_name",
            metavar="NAME",
            help="Node name published to the registry (auto-generated when omitted).",
        )

    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = build_argument_parser()
    arguments = parser.parse_args(argv)

    try:
        if arguments.verb == "new":
            return scaffold_new_app(
                arguments.directory, use_test_pattern_source=arguments.test_pattern
            )
        return launch_app_node(
            arguments.verb,
            requested_anchor_directory=arguments.anchor_directory,
            requested_entry_file=arguments.entry_file,
            bind_host=arguments.bind_host,
            bind_port=arguments.bind_port,
            node_name=arguments.node_name,
        )
    except AppLaunchError as launch_failure:
        print(f"error: {launch_failure}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `streamlib` console script: `new`, `run`, `dev`, and the observation verbs.

`nodes`, `graph`, `tap`, `logs`, and `exchange` observe nodes that are already
running — `nodes` off the on-disk registry, the rest as clients of a node's
control plane. None of them mutates a graph: a node's graph is defined by its
code, and the edit loop is re-running `dev`.

`run` and `dev` are a thin runner over the engine this wheel already exposes.
They resolve the app's entry file, execute it as `python app.py` would, call its
`setup(rt)`, and block in `rt.run()` — so the launched app and a hand-run script
are the same arrangement, not two. `new` writes an app that works before the
user has written anything.

streamlib:lint-logging:allow-file — a console script's user-facing output is
not a log event. `new` never builds an engine, and a `run` that cannot resolve
its entry file reports before a Runner exists to carry a subscriber; the
remaining sites report a launch that has already failed, where routing the
user's own error into the engine's log pipeline would bury it. Whether this
reading of the logging rule should be written into the rule itself rather than
asserted here is an open question for `/propose-rule`.
"""

from __future__ import annotations

import argparse
import re
import runpy
import sys
import traceback
from pathlib import Path
from typing import TYPE_CHECKING, Any, Callable, Optional, Sequence

from . import Runtime
from ._control_plane_client import ControlPlaneError, call_tool, resolve_control_url
from ._surface_image_exchange import (
    DEFAULT_SURFACE_ID_BAG_FIELD_NAME,
    SampledChannelExchangeReport,
    exchange_one_published_surface_id_into_directory,
    sample_channel_into_exchanged_surface_images,
)

if TYPE_CHECKING:
    from ._runtime_log_reader import LogRecordFilters

__all__ = ["main"]

DEFAULT_APP_ENTRY_FILE_NAME = "app.py"
APP_SETUP_FUNCTION_NAME = "setup"

DEFAULT_CONTROL_PLANE_BIND_HOST = "0.0.0.0"
DEFAULT_CONTROL_PLANE_BIND_PORT = 9000

SCAFFOLD_PYTHON_VERSION = "3.12"
STREAMLIB_SIMPLE_INDEX_URL = "https://tatolab.github.io/streamlib/simple/"


class ObservationVerbUsageError(Exception):
    """An observation verb invoked with flags that contradict each other.

    Distinct from [`AppLaunchError`], which names the `run` / `dev` path: these
    are argument mistakes on `nodes` / `graph` / `tap` / `logs` / `exchange`,
    and the two surfaces are free to diverge in how they report.
    """


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
    """The files whose frames are this launcher's, not the app's.

    `<frozen runpy>` as well as `runpy.__file__`: since CPython 3.11 runpy is
    frozen into the binary and its frames report the former, so matching only
    the latter leaves three runpy frames sitting on top of the user's own.
    """
    return frozenset({__file__, runpy.__file__, "<frozen runpy>"})


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
    `processors/` package resolves it here exactly as it does there. `sys.argv`
    is narrowed to the entry file for the same reason: the launcher's own flags
    are not the app's, and an app that parses `sys.argv` would otherwise see
    `run --dir … --port …`.
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


def read_app_setup_function(
    entry_namespace: "dict[str, Any]", entry_file: Path
) -> "Callable[[Runtime], Any]":
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

    try:
        entry_namespace = execute_app_entry_file(entry_file)
    # SystemExit passes through: an app that calls `sys.exit()` at module scope
    # chose its exit code, and reporting that as a failure would override it.
    except Exception as entry_failure:  # noqa: BLE001 — reported as the app's own
        print_app_failure(entry_file, entry_failure)
        return 1

    app_setup_function = read_app_setup_function(entry_namespace, entry_file)

    # Constructed only once the app's code has run: a file that cannot even be
    # executed must not cost a GPU context and an engine boot on the way to its
    # error message.
    runtime = Runtime()
    try:
        app_setup_function(runtime)
    except Exception as setup_failure:  # noqa: BLE001 — reported as the app's own
        print_app_failure(entry_file, setup_failure)
        runtime.shutdown()
        return 1

    try:
        # After `setup`, so an app that failed to build its graph publishes no
        # node entry for `streamlib nodes` to find.
        runtime.host_control_plane(
            bind_host=bind_host, bind_port=bind_port, node_name=node_name
        )
        runtime.run()
    except RuntimeError as engine_failure:
        # The engine compiles the graph at `run()`, so the failures a user hits
        # most — a bad config, no camera, no Vulkan ICD — surface here rather
        # than from `setup`. They are the app's problem, not a launcher crash,
        # and must not arrive as a traceback through this file.
        raise AppLaunchError(str(engine_failure)) from engine_failure
    return 0


def _python_distribution_name_for(directory_name: str) -> str:
    """A PEP 503 name for the scaffolded project, from its directory name."""
    normalized = re.sub(r"[^A-Za-z0-9._-]+", "-", directory_name).strip("-.")
    return normalized.lower() or "streamlib-app"


SCAFFOLDED_EFFECT_MODULE_PATH = "processors/inverting_effect.py"
SCAFFOLDED_EFFECT_CLASS_NAME = "InvertingEffect"
SCAFFOLDED_EFFECT_MODULE_NAME = "processors.inverting_effect"

# Carries a docstring rather than being empty: every other file `new` writes
# explains itself, and this one is where a reader first meets the rule that
# sends processor classes out of the entry file.
SCAFFOLDED_PROCESSOR_PACKAGE_SOURCE = (
    '"""One module per processor — each one a class a child interpreter imports."""\n'
)


def _scaffolded_app_entry_source(*, source_class_name: str) -> str:
    """The entry file: imports, wiring, and nothing else.

    The effect lives in its own module rather than here because a processor
    class defined in the entry file identifies as `__main__:<Type>`, which is a
    wiring error — the entry file runs as `__main__`, and the child interpreter
    that runs the processor imports its class by name.
    """
    source_description = (
        "camera" if source_class_name == "CameraSource" else "test pattern"
    )
    return f'''"""A StreamLib app: {source_description} → effect → window.

`streamlib dev` finds `setup(rt)` below by convention — there is no manifest and
no `main()`. Edit `{SCAFFOLDED_EFFECT_MODULE_PATH}` and re-run `streamlib dev` to
see the change.

Processors live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

from {SCAFFOLDED_EFFECT_MODULE_NAME} import {SCAFFOLDED_EFFECT_CLASS_NAME}
from streamlib import {source_class_name}, DisplayWindow, Runtime


def setup(rt: Runtime) -> None:
    source = rt.add({source_class_name})
    effect = rt.add({SCAFFOLDED_EFFECT_CLASS_NAME})
    window = rt.add(DisplayWindow, config={{"title": "StreamLib", "scaling": "fit"}})

    rt.connect(source.output("video"), effect.input("video_from_upstream"))
    rt.connect(effect.output("video_to_downstream"), window.input("video"))
'''


def _scaffolded_effect_module_source() -> str:
    """The effect, in a module the engine can import by name."""
    return f'''"""The effect the app wires between its source and its window.

Importable as `{SCAFFOLDED_EFFECT_MODULE_NAME}:{SCAFFOLDED_EFFECT_CLASS_NAME}`, which is the
name the engine spawns this processor's child interpreter with.
"""

import numpy

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    output,
    processor,
)


@processor
class {SCAFFOLDED_EFFECT_CLASS_NAME}:
    """Reads each frame, inverts its colors in place, and passes it on."""

    @input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    @output()
    def video_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None:
            return
        frame = VideoFrame.from_bag(bag)
        # The frame arrives as a surface id, not pixels: resolve it and open
        # CPU access to the engine's own memory.
        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
            surface.lock(read_only=False)
            pixels = surface.as_numpy()
            # One bulk read out, edit on the host, one bulk write back. The
            # mapping is write-combined: CPU reads of it run around 175 MB/s,
            # so editing in place through a strided view re-reads that memory
            # per channel and costs ~225ms a frame against ~30ms this way.
            edited = pixels.copy()
            # Color channels only — inverting alpha would erase the picture.
            edited[:, :, :3] = 255 - edited[:, :, :3]
            pixels[...] = edited
            surface.unlock()
        ctx.outputs.write("video_to_downstream", bag)
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
        "processors/__init__.py": SCAFFOLDED_PROCESSOR_PACKAGE_SOURCE,
        SCAFFOLDED_EFFECT_MODULE_PATH: _scaffolded_effect_module_source(),
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
        scaffolded_file = target_directory / file_name
        scaffolded_file.parent.mkdir(parents=True, exist_ok=True)
        scaffolded_file.write_text(contents, encoding="utf-8")

    print(f"Created a StreamLib app in `{target_directory}`.\n")
    print("Next:")
    print(f"    cd {target_directory}")
    print(f"    uv venv --python {SCAFFOLD_PYTHON_VERSION} && uv sync")
    print("    streamlib dev")
    return 0



# ─── Observation verbs ───────────────────────────────────────────────────────


def print_discovered_nodes() -> int:
    """`streamlib nodes`: the running control planes, as an aligned table."""
    from ._node_registry import registry_directory, scan_check_and_prune

    stream = sys.stdout
    nodes = scan_check_and_prune()
    if not nodes:
        print(f"No running nodes found in {registry_directory()}.", file=stream)
        print(
            "(Only runtimes hosting a control plane appear here.)",
            file=stream,
        )
        return 0

    runtime_id_width = max(
        [len(node.entry.runtime_id) for node in nodes] + [len("RUNTIME_ID")]
    )
    control_url_width = max(
        [len(node.entry.control_url) for node in nodes] + [len("CONTROL_URL")]
    )
    print(
        f"{'RUNTIME_ID':<{runtime_id_width}}  {'CONTROL_URL':<{control_url_width}}  "
        f"{'PID':>7}  {'ALIVE?':<6}  HINT",
        file=stream,
    )
    for node in nodes:
        print(
            f"{node.entry.runtime_id:<{runtime_id_width}}  "
            f"{node.entry.control_url:<{control_url_width}}  "
            f"{node.entry.pid:>7}  {'yes' if node.reachable else 'no':<6}  "
            f"{node.entry.hint}",
            file=stream,
        )
    print(
        "\nOnly runtimes hosting a control plane appear here; a runtime without "
        "a control endpoint is not listed (and is not missing).",
        file=stream,
    )
    return 0


def call_observation_tool(
    tool_name: str,
    *,
    requested_url: "Optional[str]",
    requested_node: "Optional[str]",
    arguments: "Optional[dict[str, Any]]" = None,
) -> int:
    """Resolve the target node, drive one tool, print its result."""
    url = resolve_control_url(requested_url, requested_node)
    print(call_tool(url, tool_name, arguments or {}))
    return 0


def render_runtime_logs(
    *,
    runtime_id: "Optional[str]",
    list_runtimes: bool,
    follow: bool,
    filters: "LogRecordFilters",
) -> int:
    """`streamlib logs` in on-disk mode: enumerate runtimes, or render one's file."""
    from ._runtime_log_reader import (
        enumerate_runtime_log_files,
        format_size,
        format_started_at,
        newest_log_file_for_runtime,
        read_log_file,
        runtime_log_directory_path,
        wait_for_runtime_log_file,
    )

    log_directory = runtime_log_directory_path()

    if list_runtimes:
        ignored_alongside_list = [
            name
            for name, value in (
                ("RUNTIME_ID", runtime_id),
                ("--follow", follow),
                ("--processor", filters.processor),
                ("--pipeline", filters.pipeline),
                ("--rhi", filters.rhi_only),
                ("--level", filters.minimum_level),
                ("--source", filters.source),
                ("--intercepted-only", filters.intercepted_only),
            )
            if value
        ]
        if ignored_alongside_list:
            raise ObservationVerbUsageError(
                f"`--list` enumerates the runtimes that have log files and reads "
                f"none of them, so it takes no {', '.join(ignored_alongside_list)}."
            )
        log_files = sorted(
            enumerate_runtime_log_files(log_directory),
            key=lambda log_file: log_file.started_at_millis,
            reverse=True,
        )
        if not log_files:
            print(f"(no runtime log files in {log_directory})")
            return 0
        print(f"{'RUNTIME_ID':<24}  {'STARTED_AT':<24}  SIZE")
        for log_file in log_files:
            print(
                f"{log_file.runtime_id:<24}  "
                f"{format_started_at(log_file.started_at_millis):<24}  "
                f"{format_size(log_file.size_bytes)}"
            )
        return 0

    if runtime_id is None:
        raise ObservationVerbUsageError(
            "missing RUNTIME_ID.\n"
            "`streamlib logs --list` enumerates the runtimes that have log files, "
            "and `--url` / `--node` reads a running node's live event stream instead."
        )

    log_file = newest_log_file_for_runtime(log_directory, runtime_id)
    if log_file is None:
        if not follow:
            raise AppLaunchError(
                f"no log file for runtime `{runtime_id}` in {log_directory}.\n"
                f"Use `streamlib logs --list` to see the runtimes that have one."
            )
        try:
            log_file = wait_for_runtime_log_file(log_directory, runtime_id, sys.stderr)
        except KeyboardInterrupt:
            return 0

    try:
        for rendered in read_log_file(
            log_file,
            filters,
            follow=follow,
            errors=sys.stderr,
            log_directory=log_directory,
        ):
            print(rendered)
    except KeyboardInterrupt:
        # Ctrl-C out of a `--follow` tail is how it ends, not a failure.
        pass
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

    def add_control_target_flags(command: argparse.ArgumentParser) -> None:
        """`--url` / `--node`: the two ways to pin which node a verb drives.

        Mutually exclusive by construction. With neither, the verb resolves the
        sole live node, which is the whole ceremony for the common case of one
        node on the machine.
        """
        target = command.add_mutually_exclusive_group()
        target.add_argument(
            "--url",
            dest="requested_url",
            metavar="URL",
            help="Control-plane base URL of the target node.",
        )
        target.add_argument(
            "--node",
            dest="requested_node",
            metavar="RUNTIME_ID",
            help="Registered runtime_id to target (resolved via the node registry).",
        )

    subcommands.add_parser(
        "nodes",
        help="List the running StreamLib nodes on this machine.",
        description=(
            "Scans the node registry, liveness-checks every entry, prunes the "
            "ones that are gone, and prints runtime_id, control_url, pid, "
            "alive? and hint. Only runtimes hosting a control plane register."
        ),
    )

    graph_command = subcommands.add_parser(
        "graph",
        help="Export a running node's live graph as JSON.",
        description=(
            "Processors, ports, links, channel names, states and metrics, plus the "
            "capability extensions loaded in that node's process, as the node "
            "reports them right now."
        ),
    )
    add_control_target_flags(graph_command)

    tap_command = subcommands.add_parser(
        "tap",
        help="Collect a bounded sample of raw bags from one channel.",
        description=(
            "Attaches a read-only tap to CHANNEL and collects a bounded sample. "
            "The tap forwards bags verbatim and never blocks the producer, so a "
            "quiet channel returns a partial sample rather than hanging."
        ),
    )
    tap_command.add_argument(
        "channel",
        help="Channel data-service name, e.g. {source_processor}/{output_port}.",
    )
    tap_command.add_argument(
        "--count",
        type=int,
        metavar="N",
        help="Bags to collect before returning (default: a small sample).",
    )
    tap_command.add_argument(
        "--max-bag-bytes",
        type=int,
        metavar="BYTES",
        help=(
            "Per-bag ceiling on the bytes returned. A bag over the cap comes "
            "back flagged and cannot be decoded, so raise this rather than "
            "accept one (default: high enough to carry any audio block whole)."
        ),
    )
    add_control_target_flags(tap_command)

    exchange_command = subcommands.add_parser(
        "exchange",
        help="Exchange published surface ids for PNG files on disk.",
        description=(
            "With SURFACE_ID, exchanges that one id. With --channel, taps the "
            "channel, reads a surface id out of each sampled bag, and exchanges "
            "it — one warm process, no window in the graph and no display server "
            "in the path. Writes exact full-resolution PNGs into --out and prints "
            "their paths on stdout, one per line — those paths are this run's "
            "frames, and --out is not cleared, so read them rather than listing "
            "the directory."
        ),
    )
    exchange_command.add_argument(
        "surface_id",
        nargs="?",
        metavar="SURFACE_ID",
        help="A surface id a bag published, e.g. `{slot}#{generation}`.",
    )
    exchange_command.add_argument(
        "--out",
        dest="output_directory",
        required=True,
        type=Path,
        metavar="DIR",
        help="Directory the PNGs are written into (created when absent).",
    )
    exchange_command.add_argument(
        "--channel",
        metavar="CHANNEL",
        help="Sample this channel instead of naming one id, e.g. {proc}/{port}.",
    )
    exchange_command.add_argument(
        "--count",
        type=int,
        metavar="N",
        help="(--channel only) Frames to exchange before returning. Default 1.",
    )
    exchange_command.add_argument(
        "--every",
        dest="every_nth_bag",
        type=int,
        metavar="N",
        help="(--channel only) Exchange every Nth sampled bag. Default 1.",
    )
    exchange_command.add_argument(
        "--field",
        dest="surface_id_bag_field_name",
        metavar="NAME",
        help=(
            "(--channel only) Bag field carrying the surface id "
            f"(default: {DEFAULT_SURFACE_ID_BAG_FIELD_NAME})."
        ),
    )
    add_control_target_flags(exchange_command)

    logs_command = subcommands.add_parser(
        "logs",
        help="Read a runtime's JSONL log file, or a running node's event stream.",
        description=(
            "With RUNTIME_ID, renders that runtime's on-disk JSONL log exactly as "
            "the runtime mirrored it. With --url / --node, collects a bounded "
            "sample of a running node's live event stream instead."
        ),
    )
    logs_command.add_argument(
        "runtime_id",
        nargs="?",
        metavar="RUNTIME_ID",
        help="Runtime to read logs for. Omit with --list, --url or --node.",
    )
    logs_command.add_argument(
        "--list",
        dest="list_runtimes",
        action="store_true",
        help="Enumerate the runtimes that have log files instead of reading one.",
    )
    logs_command.add_argument(
        "-f",
        "--follow",
        action="store_true",
        help="Follow the log file as new records land (like `tail -F`).",
    )
    logs_command.add_argument(
        "--processor", metavar="ID", help="Only records from this processor id."
    )
    logs_command.add_argument(
        "--pipeline", metavar="ID", help="Only records from this pipeline id."
    )
    logs_command.add_argument(
        "--rhi", action="store_true", help="Only RHI operations (records with rhi_op)."
    )
    logs_command.add_argument(
        "--level",
        choices=["trace", "debug", "info", "warn", "error"],
        help="Minimum severity to show.",
    )
    logs_command.add_argument(
        "--source",
        choices=["rust", "python"],
        help="Only records emitted by this runtime language.",
    )
    logs_command.add_argument(
        "--intercepted-only",
        dest="intercepted_only",
        action="store_true",
        help="Only intercepted records (captured stdout/stderr/print).",
    )
    logs_command.add_argument(
        "--count",
        type=int,
        metavar="N",
        help="(--url / --node only) Max events to collect before returning.",
    )
    add_control_target_flags(logs_command)

    return parser


def _print_sampled_channel_exchange_report(
    channel: str, report: "SampledChannelExchangeReport", wanted_image_count: int
) -> None:
    """Say what the run exchanged and what it had to retry, on stderr.

    stdout carries the paths and nothing else, so a harness can consume it
    directly; the accounting a human needs goes beside it rather than into it.
    """
    print(
        f"exchanged {len(report.written_image_paths)} of {wanted_image_count} "
        f"requested frames from `{channel}` "
        f"({report.bags_examined} bags examined over {report.tap_rounds} tap "
        f"{'round' if report.tap_rounds == 1 else 'rounds'})",
        file=sys.stderr,
    )
    if report.retried_recycled_surface_ids:
        print(
            f"retried {len(report.retried_recycled_surface_ids)} recycled "
            f"{'frame' if len(report.retried_recycled_surface_ids) == 1 else 'frames'} "
            f"against newer bags: {', '.join(report.retried_recycled_surface_ids)}",
            file=sys.stderr,
        )
    if report.bags_missing_the_surface_id_field:
        missing = report.bags_missing_the_surface_id_field
        print(
            f"{missing} {'bag' if missing == 1 else 'bags'} carried no surface id in "
            f"the named field — name the right one with `--field`",
            file=sys.stderr,
        )
    if report.stopped_early_because:
        print(f"error: {report.stopped_early_because}", file=sys.stderr)


def _run_exchange_verb(arguments: argparse.Namespace) -> int:
    """`exchange` has two forms; naming an id or a channel picks one.

    The channel-form flags have no meaning against a single id, so passing them
    with one is a wiring error rather than a silently-ignored flag.
    """
    if arguments.surface_id and arguments.channel:
        raise ObservationVerbUsageError(
            "`exchange` takes a surface id or `--channel`, not both. One id is one "
            "exchange; `--channel` samples ids off a channel."
        )
    if not arguments.surface_id and not arguments.channel:
        raise ObservationVerbUsageError(
            "`exchange` needs a surface id or `--channel`. Ids come from bags — "
            "`streamlib tap <channel>` shows what one carries."
        )

    if arguments.surface_id:
        channel_form_flags = [
            name
            for name, given in (
                ("--count", arguments.count is not None),
                ("--every", arguments.every_nth_bag is not None),
                ("--field", arguments.surface_id_bag_field_name is not None),
            )
            if given
        ]
        if channel_form_flags:
            raise ObservationVerbUsageError(
                f"{', '.join(channel_form_flags)} sample a channel, and a surface id "
                f"names one frame already. Use `--channel` instead of SURFACE_ID."
            )
        url = resolve_control_url(arguments.requested_url, arguments.requested_node)
        try:
            written_image_path = exchange_one_published_surface_id_into_directory(
                url, arguments.surface_id, arguments.output_directory
            )
        except OSError as write_failure:
            # A `--out` that names an existing file, or a directory this user
            # cannot write: a typo, and typos get a message, not a traceback.
            raise ObservationVerbUsageError(
                f"could not write into `{arguments.output_directory}`: {write_failure}"
            ) from write_failure
        print(written_image_path)
        return 0

    wanted_image_count = 1 if arguments.count is None else arguments.count
    every_nth_bag = 1 if arguments.every_nth_bag is None else arguments.every_nth_bag
    if wanted_image_count < 1:
        raise ObservationVerbUsageError("`--count` must be at least 1.")
    if every_nth_bag < 1:
        raise ObservationVerbUsageError("`--every` must be at least 1.")

    url = resolve_control_url(arguments.requested_url, arguments.requested_node)
    report = sample_channel_into_exchanged_surface_images(
        url,
        arguments.channel,
        arguments.output_directory,
        wanted_image_count=wanted_image_count,
        every_nth_bag=every_nth_bag,
        surface_id_bag_field_name=(
            arguments.surface_id_bag_field_name or DEFAULT_SURFACE_ID_BAG_FIELD_NAME
        ),
    )
    for image_path in report.written_image_paths:
        print(image_path)
    _print_sampled_channel_exchange_report(
        arguments.channel, report, wanted_image_count
    )
    # A short sample is a failure, not a partial success: a harness that read the
    # directory and found fewer frames than it asked for would otherwise take
    # exit 0 as "this is all the channel had".
    return 0 if len(report.written_image_paths) == wanted_image_count else 1


def _run_logs_verb(arguments: argparse.Namespace) -> int:
    """`logs` has two modes; a control target picks the live one.

    The on-disk filters have no meaning against a live event stream (the tool
    takes a count and nothing else), so asking for both is a wiring error rather
    than a silently-ignored flag.
    """
    from ._runtime_log_reader import LogRecordFilters

    targets_a_running_node = bool(arguments.requested_url or arguments.requested_node)
    if targets_a_running_node:
        conflicting = [
            name
            for name, value in (
                ("RUNTIME_ID", arguments.runtime_id),
                ("--list", arguments.list_runtimes),
                ("--follow", arguments.follow),
                ("--processor", arguments.processor),
                ("--pipeline", arguments.pipeline),
                ("--rhi", arguments.rhi),
                ("--level", arguments.level),
                ("--source", arguments.source),
                ("--intercepted-only", arguments.intercepted_only),
            )
            if value
        ]
        if conflicting:
            raise ObservationVerbUsageError(
                f"`--url` / `--node` reads a running node's live event stream, which "
                f"takes no {', '.join(conflicting)}. Drop the control target to read "
                f"an on-disk log file instead."
            )
        return call_observation_tool(
            "logs",
            requested_url=arguments.requested_url,
            requested_node=arguments.requested_node,
            arguments={"count": arguments.count} if arguments.count else {},
        )

    if arguments.count is not None:
        raise ObservationVerbUsageError(
            "`--count` bounds a live event-stream sample; it has no meaning for an "
            "on-disk log file. Use `--url` / `--node`, or drop `--count`."
        )
    return render_runtime_logs(
        runtime_id=arguments.runtime_id,
        list_runtimes=arguments.list_runtimes,
        follow=arguments.follow,
        filters=LogRecordFilters(
            processor=arguments.processor,
            pipeline=arguments.pipeline,
            rhi_only=arguments.rhi,
            minimum_level=arguments.level,
            source=arguments.source,
            intercepted_only=arguments.intercepted_only,
        ),
    )


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = build_argument_parser()
    arguments = parser.parse_args(argv)

    try:
        if arguments.verb == "new":
            return scaffold_new_app(
                arguments.directory, use_test_pattern_source=arguments.test_pattern
            )
        if arguments.verb == "nodes":
            return print_discovered_nodes()
        if arguments.verb == "graph":
            return call_observation_tool(
                "graph",
                requested_url=arguments.requested_url,
                requested_node=arguments.requested_node,
            )
        if arguments.verb == "tap":
            tap_arguments: "dict[str, Any]" = {"channel": arguments.channel}
            if arguments.count is not None:
                tap_arguments["count"] = arguments.count
            if arguments.max_bag_bytes is not None:
                tap_arguments["max_bag_bytes"] = arguments.max_bag_bytes
            return call_observation_tool(
                "tap",
                requested_url=arguments.requested_url,
                requested_node=arguments.requested_node,
                arguments=tap_arguments,
            )
        if arguments.verb == "exchange":
            return _run_exchange_verb(arguments)
        if arguments.verb == "logs":
            return _run_logs_verb(arguments)
        return launch_app_node(
            arguments.verb,
            requested_anchor_directory=arguments.anchor_directory,
            requested_entry_file=arguments.entry_file,
            bind_host=arguments.bind_host,
            bind_port=arguments.bind_port,
            node_name=arguments.node_name,
        )
    except (AppLaunchError, ObservationVerbUsageError, ControlPlaneError) as failure:
        print(f"error: {failure}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())

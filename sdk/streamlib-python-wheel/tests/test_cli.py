# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `streamlib` console script: entry resolution, stream selection, `setup(rt)`, and `new`.

Nothing here boots an engine. Entry resolution and scaffolding are pure
functions over the filesystem, and the failure paths are the point: an entry
that cannot be executed must produce the user's traceback and exit, never a GPU
context and a stack dump. `load` needs no device — only `run()` does — so a
compiled stream is loaded into a real `Runtime` here, in a child process. The
launch-to-a-live-node half needs a device and lives in `test_cli_launch.py`.
"""

import argparse
import ast
import json
import os
import platform
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, cast

import pytest
from app_under_test import ENGINE_STARTING_LOG_LINE

from streamlib import Runtime, _node_registry, cli

MINIMAL_APP_SOURCE = "def setup(rt):\n    pass\n"
MINIMAL_STREAM_SOURCE = (
    "from streamlib import Stream, TestPatternSource, stream\n"
    "\n"
    "\n"
    "@stream\n"
    "def main(stream: Stream) -> None:\n"
    "    stream.add(TestPatternSource)\n"
)

# Bounded: a resolution failure exits before anything is built, so a run that
# reaches this deadline has booted a node instead of failing.
RESOLUTION_FAILURE_TIMEOUT_SECONDS = 60.0


@pytest.fixture(autouse=True)
def restore_the_launchers_import_path():
    """Undo what `execute_app_entry_file` leaves on `sys.path`.

    The launcher leads `sys.path` with the entry file's directory and keeps it
    there on purpose — the app imports its own modules for as long as it runs,
    so restoring it the way `sys.argv` is restored would break the app. In a
    real launch that lasts until the process exits; here it lasts until the end
    of the pytest session, and each test that launches an app leaves another
    `tmp_path` in front. The first slot is what a helper process is told to
    import the app's processors from, so a leaked one sends every later
    suite's children looking in an empty temporary directory.
    """
    launcher_import_path = list(sys.path)
    try:
        yield
    finally:
        sys.path[:] = launcher_import_path


def write_app(directory: Path, file_name: str, source: str = MINIMAL_APP_SOURCE) -> Path:
    entry_file = directory / file_name
    entry_file.parent.mkdir(parents=True, exist_ok=True)
    entry_file.write_text(source, encoding="utf-8")
    return entry_file


def run_cli(
    *arguments: str, environment: "dict[str, str] | None" = None
) -> "subprocess.CompletedProcess[str]":
    """Drive the console script's module entry in a child interpreter.

    `-m streamlib.cli` rather than the installed `streamlib` binary so the test
    is about the code, not about whether this environment's `bin/` is on PATH —
    the shipped entry point is checked separately.
    """
    return subprocess.run(
        [sys.executable, "-m", "streamlib.cli", *arguments],
        capture_output=True,
        text=True,
        timeout=RESOLUTION_FAILURE_TIMEOUT_SECONDS,
        env=environment,
    )


# ---------------------------------------------------------------------------
# Entry resolution — the `stream.py` convention, `app.py` where there is none
# ---------------------------------------------------------------------------


def test_no_args_resolves_the_conventional_stream_entry_at_the_anchor(tmp_path: Path):
    write_app(tmp_path, "stream.py", MINIMAL_STREAM_SOURCE)

    resolved = cli.resolve_app_entry_file("run", tmp_path, None)

    assert resolved == tmp_path / "stream.py"


def test_stream_py_is_read_before_app_py(tmp_path: Path):
    write_app(tmp_path, "stream.py", MINIMAL_STREAM_SOURCE)
    write_app(tmp_path, "app.py")

    resolved = cli.resolve_app_entry_file("dev", tmp_path, None)

    assert resolved == tmp_path / "stream.py"


def test_an_app_py_still_launches_where_there_is_no_stream_py(tmp_path: Path):
    write_app(tmp_path, "app.py")

    resolved = cli.resolve_app_entry_file("run", tmp_path, None)

    assert resolved == tmp_path / "app.py"


def test_explicit_entry_file_overrides_the_convention(tmp_path: Path):
    write_app(tmp_path, "stream.py", MINIMAL_STREAM_SOURCE)
    write_app(tmp_path, "other.py", MINIMAL_STREAM_SOURCE)

    resolved = cli.resolve_app_entry_file("run", tmp_path, Path("other.py"))

    assert resolved == tmp_path / "other.py"


def test_explicit_entry_file_may_be_absolute(tmp_path: Path):
    absolute_entry = write_app(tmp_path, "elsewhere.py", MINIMAL_STREAM_SOURCE)

    resolved = cli.resolve_app_entry_file("dev", tmp_path, absolute_entry)

    assert resolved == absolute_entry


def test_a_missing_conventional_entry_names_the_convention_and_the_anchor(tmp_path: Path):
    with pytest.raises(cli.AppLaunchError) as resolution_failure:
        cli.resolve_app_entry_file("dev", tmp_path, None)

    message = str(resolution_failure.value)
    assert "no `stream.py`" in message, "the error must name the convention"
    assert "app.py" in message, "the error must name the fallback the expand step keeps"
    assert str(tmp_path) in message, "the error must name the anchor it searched"
    assert "streamlib dev" in message, "the error must name the verb the user typed"
    assert "-f " in message, "the error must offer the `-f` escape hatch"
    assert "--dir <project-root>" in message


def test_a_missing_explicit_entry_names_the_path_it_tried(tmp_path: Path):
    with pytest.raises(cli.AppLaunchError, match="gone.py"):
        cli.resolve_app_entry_file("run", tmp_path, Path("gone.py"))


def test_resolution_never_walks_up_to_a_parent(tmp_path: Path):
    write_app(tmp_path, "stream.py", MINIMAL_STREAM_SOURCE)
    write_app(tmp_path, "app.py")
    nested = tmp_path / "nested"
    nested.mkdir()

    with pytest.raises(cli.AppLaunchError, match="never searches parent"):
        cli.resolve_app_entry_file("run", nested, None)


def test_a_directory_named_like_the_entry_is_not_an_entry(tmp_path: Path):
    (tmp_path / "stream.py").mkdir()
    (tmp_path / "app.py").mkdir()

    with pytest.raises(cli.AppLaunchError):
        cli.resolve_app_entry_file("run", tmp_path, None)


def test_the_anchor_is_the_cwd_when_dir_is_absent():
    assert cli.resolve_app_anchor_directory(None) == Path.cwd()


def test_the_anchor_is_the_dir_flag_when_given(tmp_path: Path):
    assert cli.resolve_app_anchor_directory(tmp_path) == tmp_path


# ---------------------------------------------------------------------------
# The positional target — `<file>.py[:<function>]` or `<module>:<function>`
# ---------------------------------------------------------------------------


def test_a_file_target_resolves_against_the_anchor(tmp_path: Path):
    write_app(tmp_path, "rigs/front.py", MINIMAL_STREAM_SOURCE)

    resolved = cli.resolve_launch_entry("run", tmp_path, None, "rigs/front.py")

    assert resolved == cli.ResolvedLaunchEntryFile(
        entry_file=tmp_path / "rigs" / "front.py", stream_function_name=None
    )


def test_a_file_target_may_name_its_function(tmp_path: Path):
    absolute_entry = write_app(tmp_path, "front.py", MINIMAL_STREAM_SOURCE)

    resolved = cli.resolve_launch_entry("run", tmp_path, None, f"{absolute_entry}:main")

    assert resolved == cli.ResolvedLaunchEntryFile(
        entry_file=absolute_entry, stream_function_name="main"
    )


def test_a_module_target_names_a_module_and_its_function(tmp_path: Path):
    resolved = cli.resolve_launch_entry("dev", tmp_path, None, "rigs.front:main")

    assert resolved == cli.ResolvedLaunchEntryModule(
        entry_module_name="rigs.front", stream_function_name="main"
    )


def test_a_missing_file_target_names_the_path_it_tried(tmp_path: Path):
    with pytest.raises(cli.AppLaunchError) as resolution_failure:
        cli.resolve_launch_entry("run", tmp_path, None, "gone.py:main")

    message = str(resolution_failure.value)
    assert str(tmp_path / "gone.py") in message
    assert "streamlib run gone.py:main" in message, "the error names what the user typed"


@pytest.mark.parametrize(
    "malformed_target", ["main", "front.py:", ":main", "rigs/front:main", "rigs.front:"]
)
def test_a_target_in_no_known_form_is_refused_naming_the_forms(
    tmp_path: Path, malformed_target: str
):
    with pytest.raises(cli.AppLaunchError) as resolution_failure:
        cli.resolve_launch_entry("run", tmp_path, None, malformed_target)

    message = str(resolution_failure.value)
    assert f"`{malformed_target}` names no stream" in message
    assert "streamlib run <file>.py:<function>" in message
    assert "streamlib run <module>:<function>" in message


def test_a_target_and_an_entry_file_together_are_refused_naming_both(tmp_path: Path):
    write_app(tmp_path, "stream.py", MINIMAL_STREAM_SOURCE)
    write_app(tmp_path, "other.py", MINIMAL_STREAM_SOURCE)

    with pytest.raises(cli.AppLaunchError) as resolution_failure:
        cli.resolve_launch_entry("dev", tmp_path, Path("other.py"), "stream.py:main")

    message = str(resolution_failure.value)
    assert "`-f other.py`" in message
    assert "`stream.py:main`" in message


# ---------------------------------------------------------------------------
# Executing the entry — the `setup(rt)` convention
# ---------------------------------------------------------------------------


def test_the_entry_file_executes_and_yields_its_setup_function(tmp_path: Path):
    entry_file = write_app(
        tmp_path, "app.py", "def setup(rt):\n    return 'called'\n"
    )

    namespace = cli.execute_app_entry_file(entry_file)
    app_setup_function = cli.read_app_setup_function(namespace, entry_file)

    # The runtime this `setup` never touches: what is under test is that the
    # entry file's own function came back, not what it does with the argument.
    assert app_setup_function(cast(Runtime, None)) == "called"


def test_the_entry_runs_as_main_with_its_own_directory_importable(tmp_path: Path):
    """`streamlib dev` and `python stream.py` must be the same arrangement.

    An app importing its own `nodes/` package is the case that breaks if
    the entry's directory is not what leads `sys.path`.
    """
    write_app(tmp_path, "nodes/__init__.py", "")
    write_app(tmp_path, "nodes/effect.py", "EFFECT_NAME = 'blur'\n")
    entry_file = write_app(
        tmp_path,
        "stream.py",
        "from nodes.effect import EFFECT_NAME\n"
        "MODULE_NAME = __name__\n",
    )

    namespace = cli.execute_app_entry_file(entry_file)

    assert namespace["EFFECT_NAME"] == "blur"
    assert namespace["MODULE_NAME"] == "__main__", (
        "the entry must run under the name `python stream.py` gives it"
    )


def test_an_entry_without_setup_names_the_convention(tmp_path: Path):
    entry_file = write_app(tmp_path, "app.py", "PIPELINE = 1\n")

    with pytest.raises(cli.AppLaunchError, match="defines no `setup"):
        cli.read_app_setup_function({"PIPELINE": 1}, entry_file)


def test_a_non_callable_setup_is_named_rather_than_called(tmp_path: Path):
    entry_file = write_app(tmp_path, "app.py", "setup = 3\n")

    with pytest.raises(cli.AppLaunchError, match="not a function"):
        cli.read_app_setup_function({"setup": 3}, entry_file)


# ---------------------------------------------------------------------------
# Selecting and launching a stream
# ---------------------------------------------------------------------------

# Logged when a `Runtime` is constructed, before any device is touched — its
# absence is what proves a failure cost no engine at all.
ENGINE_CONSTRUCTED_LOG_LINE = "Creating Runner named"

FRONT_STREAM_SOURCE = (
    "from streamlib import DisplayWindow, Stream, TestPatternSource, stream\n"
    "\n"
    "\n"
    "@stream\n"
    "def front(stream: Stream) -> None:\n"
    '    """Front camera, in a window.\n'
    "\n"
    '    The second paragraph is not listed."""\n'
    "    source = stream.add(TestPatternSource)\n"
    "    window = stream.add(DisplayWindow)\n"
    '    stream.connect(source.output("video"), window.input("video"))\n'
)
TWO_STREAM_SOURCE = FRONT_STREAM_SOURCE + (
    "\n"
    "\n"
    "@stream\n"
    "def back(stream: Stream) -> None:\n"
    '    stream.add(TestPatternSource, name="Back Pattern")\n'
)

FRONT_STREAM_GRAPH = {
    "stream": "front",
    "nodes": [
        {
            "name": "testpatternsource",
            "type": "streamlib_media_builtins::test_pattern_source::TestPatternSource",
            "config": {},
        },
        {
            "name": "displaywindow",
            "type": "streamlib_media_builtins::display_window::DisplayWindow",
            "config": {},
        },
    ],
    "links": [
        {
            "source": {"node": "testpatternsource", "port": "video"},
            "target": {"node": "displaywindow", "port": "video"},
        }
    ],
    "exposed": [],
}


class RecordedLaunchRuntimeCalls:
    """What the launcher asked of its `Runtime`, in order — and a load refusal to answer with."""

    def __init__(self) -> None:
        self.calls: "list[tuple[Any, ...]]" = []
        self.load_refusal: "str | None" = None

    def call_names(self) -> "list[str]":
        return [call[0] for call in self.calls]

    def loaded_graph(self) -> "dict[str, Any]":
        (graph,) = [call[1] for call in self.calls if call[0] == "load"]
        return graph


@pytest.fixture
def recorded_launch_runtime_calls(
    monkeypatch: pytest.MonkeyPatch,
) -> RecordedLaunchRuntimeCalls:
    """Stand a recorder in for `Runtime` in the launcher, so no engine is built here.

    A real `Runtime` reads `sys.path[0]` once per process as the directory its
    child interpreters import from, so constructing one in this process would
    pin a test's temporary directory for every later suite. The launch also
    exports the app directory, which is restored with the rest.
    """
    recorded = RecordedLaunchRuntimeCalls()
    monkeypatch.delenv(cli.APP_DIRECTORY_ENVIRONMENT_VARIABLE, raising=False)

    class RecordingLaunchRuntime:
        def __init__(self, **runtime_keyword_arguments: Any) -> None:
            recorded.calls.append(("construct", sys.path[0], runtime_keyword_arguments))

        def load(self, graph: "dict[str, Any]", *, name: "str | None" = None) -> None:
            recorded.calls.append(("load", graph))
            if recorded.load_refusal is not None:
                raise RuntimeError(recorded.load_refusal)

        def host_control_plane(self, *, bind_host: str, bind_port: int) -> None:
            recorded.calls.append(("host_control_plane", bind_host, bind_port))

        def run(self) -> None:
            recorded.calls.append(("run",))

        def shutdown(self) -> None:
            recorded.calls.append(("shutdown",))

    monkeypatch.setattr(cli, "Runtime", RecordingLaunchRuntime)
    return recorded


@pytest.fixture
def forget_the_modules_imported_from_tmp_path(tmp_path: Path):
    """Drop what a `<module>:<function>` target imported, so no later test finds it cached."""
    yield
    for module_name, module in list(sys.modules.items()):
        module_file = getattr(module, "__file__", None)
        if module_file is not None and Path(module_file).is_relative_to(tmp_path):
            del sys.modules[module_name]


def test_the_sole_stream_is_compiled_then_loaded_then_hosted_then_run(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    """compile → `Runtime(...)` → `load` → host the control plane → `run()`, in that order."""
    write_app(tmp_path, "stream.py", FRONT_STREAM_SOURCE)

    exit_code = cli.main(
        [
            "dev",
            "--dir", str(tmp_path),
            "--host", "127.0.0.1",
            "--port", "9123",
            "--runtime-name", "desk-rig",
        ]
    )  # fmt: skip

    assert exit_code == 0, capsys.readouterr().err
    assert recorded_launch_runtime_calls.calls == [
        (
            "construct",
            str(tmp_path),
            {"runtime_name": "desk-rig"},
        ),
        ("load", FRONT_STREAM_GRAPH),
        ("host_control_plane", "127.0.0.1", 9123),
        ("run",),
    ]


def test_a_file_target_with_a_function_loads_that_stream(
    tmp_path: Path, recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls
):
    write_app(tmp_path, "rig.py", TWO_STREAM_SOURCE)

    assert cli.main(["run", "--dir", str(tmp_path), "rig.py:back"]) == 0

    assert recorded_launch_runtime_calls.loaded_graph() == {
        "stream": "back",
        "nodes": [
            {
                "name": "back-pattern",
                "type": "streamlib_media_builtins::test_pattern_source::TestPatternSource",
                "config": {},
            }
        ],
        "links": [],
        "exposed": [],
    }


def test_a_file_target_without_a_function_takes_its_sole_stream(
    tmp_path: Path, recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls
):
    write_app(tmp_path, "stream.py", TWO_STREAM_SOURCE)
    write_app(tmp_path, "solo.py", MINIMAL_STREAM_SOURCE)

    assert cli.main(["run", "--dir", str(tmp_path), "solo.py"]) == 0

    assert recorded_launch_runtime_calls.loaded_graph()["stream"] == "main"


@pytest.mark.usefixtures("forget_the_modules_imported_from_tmp_path")
def test_a_module_target_loads_the_stream_its_module_defines(
    tmp_path: Path, recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls
):
    """The module imports with the anchor leading `sys.path`, and that is still
    the slot's value when the `Runtime` is built — the slot it reads once."""
    write_app(tmp_path, "module_target_rigs/__init__.py", "")
    write_app(tmp_path, "module_target_rigs/desk.py", TWO_STREAM_SOURCE)

    assert (
        cli.main(["dev", "--dir", str(tmp_path), "module_target_rigs.desk:front"]) == 0
    )

    construct_call = recorded_launch_runtime_calls.calls[0]
    assert construct_call[:2] == ("construct", str(tmp_path))
    assert recorded_launch_runtime_calls.loaded_graph() == FRONT_STREAM_GRAPH


@pytest.mark.parametrize(
    "missing_module_name", ["no_such_rig_module", "no_such_rig_package.desk"]
)
def test_a_module_target_that_does_not_import_is_refused_naming_the_anchor(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
    missing_module_name: str,
):
    exit_code = cli.main(["run", "--dir", str(tmp_path), f"{missing_module_name}:main"])

    assert exit_code == 1
    refusal = capsys.readouterr().err
    assert f"no module `{missing_module_name}` is importable" in refusal
    assert str(tmp_path) in refusal
    assert "Traceback (most recent call last)" not in refusal
    assert recorded_launch_runtime_calls.calls == []


@pytest.mark.usefixtures("forget_the_modules_imported_from_tmp_path")
def test_a_module_target_sees_its_own_file_as_argv(
    tmp_path: Path, recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls
):
    """`sys.argv` is the module's file while it imports, as `python -m` sets it."""
    write_app(tmp_path, "argv_probe_rigs/__init__.py", "")
    write_app(
        tmp_path,
        "argv_probe_rigs/desk.py",
        "import sys\nARGV = list(sys.argv)\n" + MINIMAL_STREAM_SOURCE,
    )
    launcher_argv = list(sys.argv)

    assert cli.main(["run", "--dir", str(tmp_path), "argv_probe_rigs.desk:main"]) == 0

    assert sys.modules["argv_probe_rigs.desk"].ARGV == [
        str(tmp_path / "argv_probe_rigs" / "desk.py")
    ]
    assert sys.argv == launcher_argv


def test_a_module_target_another_module_shadows_is_refused_naming_both_files(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    """`platform` is already imported, so `import platform` never reaches the project's."""
    write_app(tmp_path, "platform.py", MINIMAL_STREAM_SOURCE)

    exit_code = cli.main(["run", "--dir", str(tmp_path), "platform:main"])

    assert exit_code == 1
    refusal = capsys.readouterr().err
    assert (
        f"`platform` (from `streamlib run platform:main`) resolves to "
        f"`{platform.__file__}`, not to `platform.py` in `{tmp_path}`"
    ) in refusal
    assert "Rename the project's module, or launch its file instead: " in refusal
    assert f"`streamlib run --dir {tmp_path} platform.py:main`" in refusal
    assert recorded_launch_runtime_calls.calls == []

    assert cli.main(["run", "--dir", str(tmp_path), "platform.py:main"]) == 0
    assert recorded_launch_runtime_calls.loaded_graph()["stream"] == "main"


def test_a_module_target_whose_parent_package_is_held_elsewhere_names_that_package(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    """`json` is already imported, so `json.desk` is searched inside the stdlib's
    `json`, never the project's — though `json/desk.py` is there."""
    write_app(tmp_path, "json/desk.py", MINIMAL_STREAM_SOURCE)

    exit_code = cli.main(["run", "--dir", str(tmp_path), "json.desk:main"])

    assert exit_code == 1
    refusal = capsys.readouterr().err
    assert (
        f"`json.desk` (from `streamlib run json.desk:main`) does not resolve to "
        f"`json/desk.py` in `{tmp_path}`: its parent package `json` resolves to "
        f"`{json.__file__}`, outside the project"
    ) in refusal
    assert "no module `json.desk` is importable" not in refusal
    assert (
        f"Rename the project's package, or launch its file instead: "
        f"`streamlib run --dir {tmp_path} json/desk.py:main`."
    ) in refusal
    assert recorded_launch_runtime_calls.calls == []

    assert cli.main(["run", "--dir", str(tmp_path), "json/desk.py:main"]) == 0
    assert recorded_launch_runtime_calls.loaded_graph()["stream"] == "main"


def test_a_module_target_naming_a_module_with_no_import_spec_is_refused_naming_the_file_form(
    tmp_path: Path,
):
    """Under `python -c`, as under the console script, `__main__` runs with no
    `__spec__`, and `find_spec` refuses it with a `ValueError` of its own."""
    finished = subprocess.run(
        [
            sys.executable,
            "-c",
            "import sys\nfrom streamlib import cli\nsys.exit(cli.main(sys.argv[1:]))",
            "run",
            "--dir",
            str(tmp_path),
            "__main__:main",
        ],
        capture_output=True,
        text=True,
        timeout=RESOLUTION_FAILURE_TIMEOUT_SECONDS,
        check=False,
    )

    assert finished.returncode == 1, finished.stderr
    assert (
        "error: `__main__` (from `streamlib run __main__:main`) is a module already "
        "running with no import spec"
    ) in finished.stderr
    assert (
        f"Name the file that defines the stream instead: "
        f"`streamlib run --dir {tmp_path} <file>.py:main`."
    ) in finished.stderr
    assert "Traceback (most recent call last)" not in finished.stderr
    assert ENGINE_CONSTRUCTED_LOG_LINE not in finished.stdout + finished.stderr


def test_a_module_targets_raising_parent_package_is_the_first_frame_of_its_traceback(
    tmp_path: Path,
):
    """Locating a dotted target runs its parent packages through `importlib.util`,
    frozen since CPython 3.11 — none of those frames may sit above the package's."""
    write_app(
        tmp_path, "raising_parent_rigs/__init__.py", "raise ValueError('bad package')\n"
    )
    write_app(tmp_path, "raising_parent_rigs/desk.py", MINIMAL_STREAM_SOURCE)

    finished = run_cli("run", "--dir", str(tmp_path), "raising_parent_rigs.desk:main")

    assert finished.returncode == 1, finished.stderr
    assert "ValueError: bad package" in finished.stderr
    first_frame = finished.stderr.index("File ")
    assert finished.stderr.startswith(
        f'File "{tmp_path / "raising_parent_rigs" / "__init__.py"}", line 1, in <module>',
        first_frame,
    ), finished.stderr
    assert "importlib" not in finished.stderr, finished.stderr
    assert ENGINE_CONSTRUCTED_LOG_LINE not in finished.stdout + finished.stderr


@pytest.mark.usefixtures("forget_the_modules_imported_from_tmp_path")
def test_a_module_targets_refusal_names_the_file_its_module_resolved_to(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    write_app(tmp_path, "refusal_naming_rigs/__init__.py", "")
    write_app(tmp_path, "refusal_naming_rigs/desk.py", TWO_STREAM_SOURCE)

    exit_code = cli.main(
        ["run", "--dir", str(tmp_path), "refusal_naming_rigs.desk:side"]
    )

    assert exit_code == 1
    refusal = capsys.readouterr().err
    assert (
        f"`refusal_naming_rigs.desk` (`{tmp_path / 'refusal_naming_rigs' / 'desk.py'}`) "
        f"defines no @stream function named `side`"
    ) in refusal
    assert (
        f"Name one of them: `streamlib run --dir {tmp_path} "
        f"refusal_naming_rigs.desk:<function>`."
    ) in refusal
    assert recorded_launch_runtime_calls.calls == []


def test_name_overrides_the_streams_own_name_and_is_cast(
    tmp_path: Path, recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls
):
    write_app(tmp_path, "stream.py", MINIMAL_STREAM_SOURCE)

    assert cli.main(["run", "--dir", str(tmp_path), "--name", "Front Rig"]) == 0

    assert recorded_launch_runtime_calls.loaded_graph()["stream"] == "front-rig"


def test_a_name_that_casts_to_nothing_is_refused_before_the_entry_runs(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    ran = tmp_path / "entry-ran.txt"
    write_app(
        tmp_path,
        "stream.py",
        f"open({str(ran)!r}, 'w').write('ran')\n" + MINIMAL_STREAM_SOURCE,
    )

    exit_code = cli.main(["run", "--dir", str(tmp_path), "--name", "!!!"])

    assert exit_code == 1
    assert "--name '!!!' cannot name a stream" in capsys.readouterr().err
    assert not ran.exists(), "a refused flag must not cost the entry file a run"
    assert recorded_launch_runtime_calls.calls == []


def test_a_refused_load_shuts_the_runtime_down_and_hosts_nothing(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    write_app(tmp_path, "stream.py", MINIMAL_STREAM_SOURCE)
    recorded_launch_runtime_calls.load_refusal = "unknown processor type `nowhere:Nothing`"

    exit_code = cli.main(["run", "--dir", str(tmp_path)])

    assert exit_code == 1
    refusal = capsys.readouterr().err
    assert "error: the stream `main`" in refusal
    assert "did not load: unknown processor type `nowhere:Nothing`" in refusal
    assert "Traceback (most recent call last)" not in refusal
    assert recorded_launch_runtime_calls.call_names() == ["construct", "load", "shutdown"], (
        "a refused load hosts no control plane and never runs"
    )


def test_several_streams_are_refused_listing_each_with_its_description(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    write_app(tmp_path, "stream.py", TWO_STREAM_SOURCE)

    exit_code = cli.main(["run", "--dir", str(tmp_path)])

    assert exit_code == 1
    refusal = capsys.readouterr().err
    assert "defines 2 @stream functions" in refusal
    assert "    front — Front camera, in a window.\n" in refusal
    assert "The second paragraph" not in refusal, "only the description's first line is listed"
    assert "    back\n" in refusal
    assert f"`streamlib run --dir {tmp_path} stream.py:<function>`" in refusal
    assert recorded_launch_runtime_calls.calls == [], "selection is refused before any engine"


def test_the_several_streams_suggestion_quotes_the_dir_it_was_given(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    project_directory = tmp_path / "front rig"
    write_app(project_directory, "stream.py", TWO_STREAM_SOURCE)

    assert cli.main(["dev", "--dir", str(project_directory)]) == 1

    assert (
        f"`streamlib dev --dir '{project_directory}' stream.py:<function>`"
        in capsys.readouterr().err
    )


@pytest.mark.parametrize(
    "launch_arguments",
    [["my rig.py"], ["my rig.py:side"]],
    ids=["several-streams", "a-function-the-file-lacks"],
)
def test_a_suggestion_quotes_an_entry_file_whose_name_holds_a_space(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
    launch_arguments: "list[str]",
):
    write_app(tmp_path, "my rig.py", TWO_STREAM_SOURCE)

    assert cli.main(["run", "--dir", str(tmp_path), *launch_arguments]) == 1

    suggestion = f"streamlib run --dir {tmp_path} 'my rig.py':<function>"
    assert f"`{suggestion}`" in capsys.readouterr().err
    _, *suggested_arguments = shlex.split(suggestion.replace("<function>", "back"))
    assert cli.main(suggested_arguments) == 0
    assert recorded_launch_runtime_calls.loaded_graph()["stream"] == "back"


def test_the_several_streams_suggestion_names_no_dir_when_none_was_given(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
    monkeypatch: pytest.MonkeyPatch,
):
    write_app(tmp_path, "stream.py", TWO_STREAM_SOURCE)
    monkeypatch.chdir(tmp_path)

    assert cli.main(["run"]) == 1

    assert "`streamlib run stream.py:<function>`" in capsys.readouterr().err


def test_a_second_name_bound_to_a_stream_is_still_one_stream(
    tmp_path: Path, recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls
):
    write_app(tmp_path, "stream.py", MINIMAL_STREAM_SOURCE + "\n\ndefault = main\n")

    assert cli.main(["run", "--dir", str(tmp_path)]) == 0

    assert recorded_launch_runtime_calls.loaded_graph()["stream"] == "main"


def test_a_target_naming_a_second_name_of_a_stream_defined_in_the_file_selects_it(
    tmp_path: Path, recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls
):
    write_app(tmp_path, "stream.py", TWO_STREAM_SOURCE + "\n\ndefault = back\n")

    assert cli.main(["run", "--dir", str(tmp_path), "stream.py:default"]) == 0

    assert recorded_launch_runtime_calls.loaded_graph()["stream"] == "back"


def test_an_entry_that_defines_no_stream_is_refused_with_a_sample(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    write_app(tmp_path, "stream.py", "PIPELINE = 1\n")

    exit_code = cli.main(["dev", "--dir", str(tmp_path)])

    assert exit_code == 1
    refusal = capsys.readouterr().err
    assert "stream.py` defines no @stream function" in refusal
    assert "    @stream\n    def main(stream: Stream) -> None:\n" in refusal
    assert "stream.add(CameraSource)" in refusal
    assert "-f <file>" in refusal
    assert "streamlib dev <file>.py:<function>" in refusal
    assert recorded_launch_runtime_calls.calls == []


def test_a_named_function_the_file_lacks_is_refused_listing_its_streams(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    write_app(tmp_path, "stream.py", TWO_STREAM_SOURCE)

    exit_code = cli.main(["run", "--dir", str(tmp_path), "stream.py:side"])

    assert exit_code == 1
    refusal = capsys.readouterr().err
    assert "defines no @stream function named `side`" in refusal
    assert "    front — Front camera, in a window.\n" in refusal
    assert "    back" in refusal
    assert recorded_launch_runtime_calls.calls == []


def test_a_named_function_in_a_setup_file_is_refused_naming_the_launch_without_it(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    write_app(tmp_path, "app.py")

    exit_code = cli.main(["run", "--dir", str(tmp_path), "app.py:main"])

    assert exit_code == 1
    refusal = capsys.readouterr().err
    assert (
        f"`{tmp_path / 'app.py'}` defines no @stream function named `main`, nor any "
        f"other: it builds its graph in `setup(rt)`. Launch it without `:<function>`: "
        f"`streamlib run --dir {tmp_path} app.py`."
    ) in refusal
    assert recorded_launch_runtime_calls.calls == []


def test_a_named_function_in_a_file_with_no_stream_is_refused_naming_the_decorator(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    write_app(tmp_path, "stream.py", "PIPELINE = 1\n")

    exit_code = cli.main(["run", "--dir", str(tmp_path), "stream.py:main"])

    assert exit_code == 1
    refusal = capsys.readouterr().err
    assert "defines no @stream function named `main`, nor any other" in refusal
    assert (
        "`@stream` above a module-level `def main(stream: Stream) -> None:`" in refusal
    )
    assert recorded_launch_runtime_calls.calls == []


def test_a_named_function_that_is_not_a_stream_is_refused_naming_the_fix(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    write_app(
        tmp_path,
        "stream.py",
        MINIMAL_STREAM_SOURCE + "\n\ndef helper(stream: Stream) -> None:\n    pass\n",
    )

    exit_code = cli.main(["run", "--dir", str(tmp_path), "stream.py:helper"])

    assert exit_code == 1
    refusal = capsys.readouterr().err
    assert "`helper` in" in refusal and "is not a @stream function" in refusal
    assert "decorate it with `@stream`" in refusal
    assert recorded_launch_runtime_calls.calls == []


@pytest.mark.usefixtures("forget_the_modules_imported_from_tmp_path")
def test_a_stream_imported_into_the_entry_loads_when_a_target_names_it(
    tmp_path: Path, recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls
):
    """An imported stream is never the entry's sole stream, yet a target may name it."""
    write_app(tmp_path, "imported_rigs.py", TWO_STREAM_SOURCE)
    write_app(
        tmp_path, "stream.py", "from imported_rigs import front\n" + MINIMAL_STREAM_SOURCE
    )

    assert cli.main(["run", "--dir", str(tmp_path)]) == 0
    assert recorded_launch_runtime_calls.loaded_graph()["stream"] == "main"

    recorded_launch_runtime_calls.calls.clear()
    assert cli.main(["run", "--dir", str(tmp_path), "stream.py:front"]) == 0
    assert recorded_launch_runtime_calls.loaded_graph() == FRONT_STREAM_GRAPH


@pytest.mark.usefixtures("forget_the_modules_imported_from_tmp_path")
def test_a_stream_imported_under_another_name_loads_by_that_name(
    tmp_path: Path, recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls
):
    write_app(tmp_path, "renamed_import_rigs.py", TWO_STREAM_SOURCE)
    write_app(
        tmp_path,
        "stream.py",
        "from renamed_import_rigs import front as side\n" + MINIMAL_STREAM_SOURCE,
    )

    assert cli.main(["run", "--dir", str(tmp_path), "stream.py:side"]) == 0

    assert recorded_launch_runtime_calls.loaded_graph() == FRONT_STREAM_GRAPH


@pytest.mark.usefixtures("forget_the_modules_imported_from_tmp_path")
def test_a_package_target_loads_a_stream_its_init_re_exports(
    tmp_path: Path, recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls
):
    """`acme_rover:camera_rig`, where `acme_rover/__init__.py` re-exports it."""
    write_app(
        tmp_path,
        "re_exporting_rover/__init__.py",
        "from re_exporting_rover.streams import front\n",
    )
    write_app(tmp_path, "re_exporting_rover/streams.py", TWO_STREAM_SOURCE)

    assert cli.main(["run", "--dir", str(tmp_path), "re_exporting_rover:front"]) == 0

    assert recorded_launch_runtime_calls.loaded_graph() == FRONT_STREAM_GRAPH


@pytest.mark.usefixtures("forget_the_modules_imported_from_tmp_path")
def test_a_package_target_naming_a_value_that_is_not_a_stream_is_refused_naming_the_fix(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    write_app(
        tmp_path,
        "re_exporting_helper_rover/__init__.py",
        "from re_exporting_helper_rover.helpers import wire_cameras\n",
    )
    write_app(
        tmp_path,
        "re_exporting_helper_rover/helpers.py",
        "def wire_cameras(stream):\n    pass\n",
    )

    exit_code = cli.main(
        ["run", "--dir", str(tmp_path), "re_exporting_helper_rover:wire_cameras"]
    )

    assert exit_code == 1
    refusal = capsys.readouterr().err
    assert "`wire_cameras` in" in refusal and "is not a @stream function" in refusal
    assert "decorate it with `@stream`" in refusal
    assert recorded_launch_runtime_calls.calls == []


def test_an_entry_defining_both_a_stream_and_setup_is_refused(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    write_app(tmp_path, "stream.py", MINIMAL_STREAM_SOURCE + "\n\n" + MINIMAL_APP_SOURCE)

    exit_code = cli.main(["run", "--dir", str(tmp_path)])

    assert exit_code == 1
    refusal = capsys.readouterr().err
    assert "defines both @stream functions (`main`) and `setup(rt)`" in refusal
    assert "remove `setup`" in refusal
    assert recorded_launch_runtime_calls.calls == []


def test_name_is_refused_for_an_entry_that_builds_its_graph_in_setup(
    tmp_path: Path,
    recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls,
    capsys: pytest.CaptureFixture[str],
):
    write_app(tmp_path, "app.py")

    exit_code = cli.main(["run", "--dir", str(tmp_path), "--name", "rig"])

    assert exit_code == 1
    assert (
        f"--name names a stream, and `{tmp_path / 'app.py'}` builds its graph in "
        f"`setup(rt)`: drop `--name`, or convert the file to a `@stream` function."
    ) in capsys.readouterr().err
    assert recorded_launch_runtime_calls.calls == []


def test_an_app_py_with_setup_still_launches_through_setup(
    tmp_path: Path, recorded_launch_runtime_calls: RecordedLaunchRuntimeCalls
):
    """The expand step's fallback: no `stream.py`, an `app.py` with `setup(rt)`."""
    write_app(
        tmp_path,
        "app.py",
        "SETUP_CALLS = []\ndef setup(rt):\n    SETUP_CALLS.append(type(rt).__name__)\n",
    )

    assert cli.main(["run", "--dir", str(tmp_path)]) == 0

    assert recorded_launch_runtime_calls.call_names() == [
        "construct",
        "host_control_plane",
        "run",
    ], "the setup path builds the graph in `setup`, never through `load`"


def test_a_stream_that_adds_nothing_is_refused_at_load_and_publishes_no_node(
    tmp_path: Path,
):
    """A real `Runtime`, in a child: `load` refuses the empty graph by name before
    any control plane is hosted, so no node entry is ever published."""
    write_app(
        tmp_path,
        "stream.py",
        "from streamlib import Stream, stream\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream: Stream) -> None:\n"
        "    pass\n",
    )
    # Short, as the engine's surface-share socket path must fit `sun_path`.
    isolated_runtime_directory = Path(tempfile.mkdtemp(prefix="sl-"))
    try:
        finished = run_cli(
            "run",
            "--dir",
            str(tmp_path),
            environment={**os.environ, "XDG_RUNTIME_DIR": str(isolated_runtime_directory)},
        )
        published_node_entries = sorted(
            (isolated_runtime_directory / "streamlib" / "nodes").glob("*.json")
        )
    finally:
        shutil.rmtree(isolated_runtime_directory, ignore_errors=True)

    output = finished.stdout + finished.stderr
    assert finished.returncode == 1, output
    assert "error: the stream `main`" in finished.stderr
    assert "holds no node" in finished.stderr, output
    assert "Traceback (most recent call last)" not in finished.stderr, output
    assert ENGINE_STARTING_LOG_LINE not in output, "a refused load never runs the engine"
    assert published_node_entries == [], "a refused load hosts no control plane"


def test_a_raising_stream_function_prints_its_traceback_and_builds_no_engine(
    tmp_path: Path,
):
    write_app(
        tmp_path,
        "stream.py",
        "from streamlib import Stream, stream\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream: Stream) -> None:\n"
        "    raise ValueError('bad wiring')\n",
    )

    finished = run_cli("dev", "--dir", str(tmp_path))

    output = finished.stdout + finished.stderr
    assert finished.returncode == 1, output
    assert "ValueError: bad wiring" in finished.stderr
    assert "Traceback (most recent call last)" in finished.stderr
    assert "cli.py" not in finished.stderr, "the launcher's frames are stripped"
    assert "_stream_graph_builder.py" not in finished.stderr, (
        "the compile call into the stream function is the launcher's, not the author's"
    )
    assert f'File "{tmp_path / "stream.py"}", line 6, in main' in finished.stderr
    assert ENGINE_CONSTRUCTED_LOG_LINE not in output, "compiling fails before any engine"


def test_a_typed_duplicate_is_refused_on_the_authors_own_line(tmp_path: Path):
    write_app(
        tmp_path,
        "stream.py",
        "from streamlib import Stream, TestPatternSource, stream\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream: Stream) -> None:\n"
        '    stream.add(TestPatternSource, name="Pattern")\n'
        '    stream.add(TestPatternSource, name="pattern")\n',
    )

    finished = run_cli("run", "--dir", str(tmp_path))

    assert finished.returncode == 1, finished.stderr
    first_frame = finished.stderr.index("File ")
    assert finished.stderr.startswith(
        f'File "{tmp_path / "stream.py"}", line 7, in main', first_frame
    ), finished.stderr
    assert "casts to `pattern`" in finished.stderr
    assert ENGINE_CONSTRUCTED_LOG_LINE not in finished.stdout + finished.stderr


# ---------------------------------------------------------------------------
# The failure surfaces — a bad save must cost nothing
# ---------------------------------------------------------------------------


def test_a_syntax_error_prints_the_apps_traceback_and_builds_no_engine(tmp_path: Path):
    """The bad-save path. A broken entry file is the user's typo, not a crash.

    Reaching the timeout is the regression this guards: an engine built before
    the entry file ran would boot a node and block instead of exiting.
    """
    write_app(
        tmp_path,
        "stream.py",
        "from streamlib import Stream, stream\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream: Stream) -> None\n"
        "    pass\n",
    )

    finished = run_cli("dev", "--dir", str(tmp_path))

    assert finished.returncode == 1, f"stderr was:\n{finished.stderr}"
    assert "SyntaxError" in finished.stderr, (
        f"the user's own error must be the headline; stderr was:\n{finished.stderr}"
    )
    assert "stream.py" in finished.stderr, "the traceback must name the file"
    assert "Initializing GPU context" not in finished.stdout + finished.stderr, (
        "a file that cannot be executed must not cost an engine boot"
    )


def test_a_bad_save_in_the_effect_module_names_that_module_not_the_entry_file(
    tmp_path: Path,
):
    """The bad save the scaffold actually invites.

    `stream.py` holds wiring the user rarely touches; the file they edit is the
    node module, which reaches the launcher only as an import from the
    entry file. So the traceback has to walk through `stream.py` and land in the
    module — naming only the entry file would point at the wrong file.
    """
    app_directory = tmp_path / "demo"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=True)
    (app_directory / cli.SCAFFOLDED_EFFECT_MODULE_PATH).write_text(
        "def process(self ctx:\n    this does not parse\n"
    )

    finished = run_cli("dev", "--dir", str(app_directory))

    assert finished.returncode == 1, f"stderr was:\n{finished.stderr}"
    assert "SyntaxError" in finished.stderr, (
        f"the user's own error must be the headline; stderr was:\n{finished.stderr}"
    )
    assert "inverting_effect.py" in finished.stderr, (
        f"the traceback must name the module the user edited; stderr was:\n"
        f"{finished.stderr}"
    )
    assert ENGINE_STARTING_LOG_LINE not in finished.stdout + finished.stderr, (
        "a module that cannot be imported must not cost an engine boot"
    )


def test_a_raise_at_import_time_surfaces_as_the_apps_traceback(tmp_path: Path):
    write_app(tmp_path, "stream.py", "raise ValueError('bad wiring')\n")

    finished = run_cli("run", "--dir", str(tmp_path))

    assert finished.returncode == 1
    assert "ValueError: bad wiring" in finished.stderr
    assert "bad wiring" in finished.stderr


def test_a_missing_entry_exits_without_a_python_traceback(tmp_path: Path):
    """A missing `app.py` is a usage error, not an internal failure."""
    finished = run_cli("dev", "--dir", str(tmp_path))

    assert finished.returncode == 1
    assert "app.py" in finished.stderr
    assert "never searches parent directories" in finished.stderr
    assert "Traceback (most recent call last)" not in finished.stderr, (
        f"a usage error must not print a launcher traceback; stderr was:\n{finished.stderr}"
    )


def test_the_apps_traceback_carries_none_of_the_launchers_frames(tmp_path: Path):
    """The user's own line must be the first frame they read.

    `runpy` is frozen since CPython 3.11 and its frames report
    `<frozen runpy>`, so matching only `runpy.__file__` leaves three of its
    frames sitting on top of the app's.
    """
    write_app(tmp_path, "stream.py", "raise ValueError('bad wiring')\n")

    finished = run_cli("run", "--dir", str(tmp_path))

    assert "runpy" not in finished.stderr, (
        f"no launcher frame may appear in the app's traceback; stderr was:\n{finished.stderr}"
    )
    assert "cli.py" not in finished.stderr, (
        f"the launcher's own frames must be stripped; stderr was:\n{finished.stderr}"
    )
    assert "stream.py" in finished.stderr


def test_the_app_does_not_see_the_launchers_arguments(tmp_path: Path):
    """`sys.argv` belongs to the app, not to `streamlib run`."""
    entry_file = write_app(tmp_path, "stream.py", "import sys\nARGV = list(sys.argv)\n")

    namespace = cli.execute_app_entry_file(entry_file)

    assert namespace["ARGV"] == [str(entry_file)], (
        "the app must see only its own path, as `python stream.py` gives it"
    )


def test_the_launcher_restores_its_own_argv(tmp_path: Path):
    write_app(tmp_path, "stream.py", MINIMAL_STREAM_SOURCE)
    launcher_argv = list(sys.argv)

    cli.execute_app_entry_file(tmp_path / "stream.py")

    assert sys.argv == launcher_argv


def test_an_app_that_exits_on_purpose_keeps_its_own_exit_code(tmp_path: Path):
    """`sys.exit()` at module scope is a choice, not a failure to report."""
    write_app(tmp_path, "stream.py", "import sys\nsys.exit(3)\n")

    finished = run_cli("run", "--dir", str(tmp_path))

    assert finished.returncode == 3, f"stderr was:\n{finished.stderr}"
    assert "error:" not in finished.stderr, (
        f"a deliberate exit must not be reported as a failure; stderr was:\n{finished.stderr}"
    )


def test_a_setup_that_exits_on_purpose_keeps_its_own_exit_code(tmp_path: Path):
    """The same on the `setup` path, which builds an engine first."""
    write_app(tmp_path, "app.py", "import sys\ndef setup(rt):\n    sys.exit(4)\n")

    finished = run_cli("run", "--dir", str(tmp_path))

    assert finished.returncode == 4, f"stderr was:\n{finished.stderr}"


def test_the_observation_verbs_are_served_by_this_wheel(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """`nodes` / `graph` / `tap` / `logs` are this CLI's, not another binary's.

    This replaces the stopgap that used to name where the verbs "actually
    lived": they live here now. `nodes` is the one that answers without a
    running node to talk to, so it proves the verb is wired end to end rather
    than merely present in the parser.

    `nodes` runs in-process against a replaced resolver: it liveness-checks
    and prunes every entry it finds, and this test must not reach a real node
    on a developer's machine, let alone delete its registry entry. Only Linux
    reads `XDG_RUNTIME_DIR`, so a child process cannot be pointed elsewhere.
    """
    monkeypatch.setattr(_node_registry, "runtime_directory", lambda: tmp_path / "streamlib")
    assert cli.main(["nodes"]) == 0

    listed = run_cli("--help")
    for verb in ("graph", "tap", "logs", "exchange"):
        assert verb in listed.stdout, f"`streamlib {verb}` must be a served verb"


def test_this_wheel_is_the_only_streamlib_cli():
    """The served verb set is exactly the decided one — nothing missing, nothing extra.

    Successor to the two guards the Rust `streamlib-cli` binary carried before
    it was deleted: they asserted that binary owned no observation verb and no
    app-launch verb, which is unprovable once the binary is gone. The invariant
    they protected — one CLI answering to `streamlib`, not two clients racing
    for the same name against the same control plane — is asserted here
    instead, against the CLI that actually exists. Pinning the set exactly is
    what makes it a guard: a verb reappearing here is as much a regression as
    one going missing.
    """
    subcommand_actions = [
        action
        for action in cli.build_argument_parser()._actions  # noqa: SLF001
        if isinstance(action, argparse._SubParsersAction)  # noqa: SLF001
    ]
    assert len(subcommand_actions) == 1
    served = set(subcommand_actions[0].choices)

    assert served == {
        "new",
        "run",
        "dev",
        "nodes",
        "graph",
        "tap",
        "logs",
        "exchange",
        # The one machine-setup verb: touches no node, speaks no control plane.
        "enable-virtual-camera",
    }


def test_the_wheel_serves_no_mcp_verb(tmp_path: Path):
    """MCP is served by a node's own control plane at `POST /mcp`, on the node's
    lifecycle — there is no CLI verb to start one or attach to one."""
    finished = run_cli("mcp")

    assert finished.returncode != 0
    assert "invalid choice" in finished.stderr, (
        f"`streamlib mcp` must not be a subcommand; stderr was:\n{finished.stderr}"
    )


def test_the_control_plane_binds_every_interface_by_default():
    """§Control plane: "`dev` and `run` bind the control plane identically: all
    interfaces". Reachability is not the lever that scopes exposure — auth is —
    so no narrower bind default is set ahead of the auth posture.
    """
    assert cli.DEFAULT_CONTROL_PLANE_BIND_HOST == "0.0.0.0"


# ---------------------------------------------------------------------------
# `streamlib new`
# ---------------------------------------------------------------------------

SCAFFOLDED_FILE_NAMES = (
    "stream.py",
    "nodes/__init__.py",
    "nodes/inverting_effect.py",
    "nodes/brightness_meter.py",
    "pyproject.toml",
    ".python-version",
    ".gitignore",
)


def test_new_writes_a_working_app(tmp_path: Path):
    app_directory = tmp_path / "demo"

    cli.scaffold_new_app(app_directory, use_test_pattern_source=False)

    for file_name in SCAFFOLDED_FILE_NAMES:
        assert (app_directory / file_name).is_file(), f"`new` must write {file_name}"
    assert not (app_directory / "processors").exists(), (
        "a scaffolded app keeps its node classes under `nodes/`"
    )
    assert not (app_directory / "app.py").exists(), (
        "the scaffold's entry is `stream.py`; `app.py` is only a fallback `run` keeps"
    )
    assert (app_directory / ".python-version").read_text().strip() == "3.12", (
        "the scaffold pins the Python version the plan names"
    )


@pytest.mark.parametrize("use_test_pattern_source", [False, True])
def test_new_writes_exactly_the_rendered_templates(
    tmp_path: Path, use_test_pattern_source: bool
):
    app_directory = tmp_path / "demo"

    cli.scaffold_new_app(app_directory, use_test_pattern_source=use_test_pattern_source)

    written_files = {
        path.relative_to(app_directory).as_posix(): path.read_bytes()
        for path in app_directory.rglob("*")
        if path.is_file()
    }
    rendered_files = {
        file_name: contents.encode("utf-8")
        for file_name, contents in cli.render_scaffold_template_files(
            distribution_name="demo", use_test_pattern_source=use_test_pattern_source
        ).items()
    }
    assert written_files == rendered_files
    assert not any(b"SPDX-License-Identifier" in contents for contents in written_files.values()), (
        "the app `new` writes is the user's own code, not StreamLib's"
    )


# The source tree's copy, so ruff resolves the wheel's `[tool.ruff]` config.
SCAFFOLD_TEMPLATE_SOURCE_DIRECTORY = (
    Path(__file__).resolve().parents[1] / "python" / "streamlib" / "_scaffold_template"
)


@pytest.mark.parametrize("use_test_pattern_source", [False, True])
@pytest.mark.parametrize("ruff_arguments", [("check",), ("format", "--check")])
def test_every_scaffolded_python_file_passes_ruff(
    use_test_pattern_source: bool, ruff_arguments: "tuple[str, ...]"
):
    """Linted as rendered, not as templated: the test-pattern variant is text no
    template file holds."""
    rendered_files = cli.render_scaffold_template_files(
        distribution_name="demo", use_test_pattern_source=use_test_pattern_source
    )

    for file_name, contents in rendered_files.items():
        if not file_name.endswith(".py"):
            continue
        finished = subprocess.run(
            [
                sys.executable,
                "-m",
                "ruff",
                *ruff_arguments,
                "--stdin-filename",
                str(SCAFFOLD_TEMPLATE_SOURCE_DIRECTORY / file_name),
                "-",
            ],
            input=contents,
            capture_output=True,
            text=True,
            check=False,
        )
        assert finished.returncode == 0, (
            f"ruff {' '.join(ruff_arguments)} rejects the scaffolded {file_name}:\n"
            f"{finished.stdout}{finished.stderr}"
        )


def test_every_scaffold_template_file_is_one_new_writes():
    """A template file the mapping does not name would ship in the wheel and never
    reach an app."""
    template_files = {
        path.relative_to(cli.SCAFFOLD_TEMPLATE_DIRECTORY).as_posix()
        for path in cli.SCAFFOLD_TEMPLATE_DIRECTORY.rglob("*")
        if path.is_file() and "__pycache__" not in path.parts
    }

    assert template_files == set(cli.SCAFFOLDED_FILE_PATH_FOR_TEMPLATE_FILE)


def test_the_scaffolded_stream_declares_one_stream_named_main(tmp_path: Path):
    """The scaffold is the first code the user reads — `dev` with no argument
    loads the sole `@stream` in it, so it must declare exactly one, bare."""
    app_directory = tmp_path / "demo"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=False)

    entry_source = (app_directory / "stream.py").read_text()
    declared = ast.parse(entry_source)
    stream_functions = [
        node.name
        for node in declared.body
        if isinstance(node, ast.FunctionDef)
        and [ast.unparse(decorator) for decorator in node.decorator_list] == ["stream"]
    ]

    assert stream_functions == ["main"]
    assert not any(
        isinstance(node, ast.FunctionDef) and node.name == "setup" for node in declared.body
    ), "an entry defining both a stream and `setup` is refused"
    assert "CameraSource" in entry_source
    assert "DisplayWindow" in entry_source


def test_every_scaffolded_python_file_is_valid_python_that_explains_itself(
    tmp_path: Path,
):
    """The scaffold is the first code the user reads, `__init__.py` included.

    An empty package init parses but teaches nothing, and this is where a
    reader first meets the rule that keeps processor classes out of the entry
    file.
    """
    app_directory = tmp_path / "demo"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=False)

    for file_name in SCAFFOLDED_FILE_NAMES:
        if not file_name.endswith(".py"):
            continue
        source = (app_directory / file_name).read_text()
        parsed = ast.parse(source)
        assert ast.get_docstring(parsed), (
            f"{file_name} carries no module docstring — every file `new` writes "
            f"explains what it is for"
        )


@pytest.mark.parametrize(
    ("module_path", "class_name"),
    [
        (cli.SCAFFOLDED_EFFECT_MODULE_PATH, "InvertingEffect"),
        (cli.SCAFFOLDED_METER_MODULE_PATH, "BrightnessMeter"),
    ],
)
def test_each_scaffolded_processor_lives_outside_the_entry_file(
    tmp_path: Path, module_path: str, class_name: str
):
    """A processor class in the entry file identifies as `__main__:<Type>`,
    which is a wiring error — the entry runs as `__main__`, and the child
    interpreter that runs the processor imports its class by name.

    So the scaffold must teach the shape that works: wiring in `stream.py`, the
    class in an importable module beside it.
    """
    app_directory = tmp_path / "demo"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=False)

    entry_source = (app_directory / "stream.py").read_text()
    processor_source = (app_directory / module_path).read_text()
    module_name = module_path.removesuffix(".py").replace("/", ".")

    assert "@node" not in entry_source, (
        "a processor class in the entry file would identify as `__main__:<Type>`"
    )
    assert f"class {class_name}" not in entry_source
    assert "@node" in processor_source, "the class belongs in the importable module"
    assert f"class {class_name}" in processor_source
    assert f"from {module_name} import {class_name}" in entry_source, (
        "the entry file imports the class it wires"
    )
    # Both halves must parse — the entry is useless if its processor module is not.
    ast.parse(entry_source)
    ast.parse(processor_source)


def test_the_scaffold_models_pixels_on_the_gpu_and_logic_on_the_cpu(tmp_path: Path):
    """The pathway the plan sets: the effect in the video path runs on the GPU,
    and the numpy processor reads an explicit CPU view off a fan-out, so the
    slow door never sits between the camera and the window."""
    app_directory = tmp_path / "demo"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=False)

    entry_source = (app_directory / "stream.py").read_text()
    effect_source = (app_directory / cli.SCAFFOLDED_EFFECT_MODULE_PATH).read_text()
    meter_source = (app_directory / cli.SCAFFOLDED_METER_MODULE_PATH).read_text()

    assert "GlslPixelEffect.compile(" in effect_source
    assert "numpy" not in effect_source, "the effect in the video path touches no host pixels"
    assert "frame.cpu()" in meter_source, "the meter's host view of the pixels is explicit"
    assert "ctx.time" in meter_source, "the meter paces itself on the monotonic clock"
    readers_of_the_effect_output = sorted(
        ast.unparse(call.args[1])
        for call in ast.walk(ast.parse(entry_source))
        if isinstance(call, ast.Call)
        and ast.unparse(call.func) == "stream.connect"
        and ast.unparse(call.args[0]) == "effect.output('video_to_downstream')"
    )
    assert readers_of_the_effect_output == [
        "meter.input('video_from_upstream')",
        "window.input('video')",
    ], "the meter reads a fan-out of the effect's output, off the window's path"


# Run in a child with the scaffold as its working directory: compiling imports
# the scaffold's `nodes` package, and a `Runtime` reads `sys.path[0]` once per
# process, so neither may happen in this one.
SCAFFOLDED_STREAM_COMPILE_AND_LOAD_SCRIPT = """
import json
from pathlib import Path

from streamlib import Runtime, cli, compile_stream_to_graph

entry_namespace = cli.execute_app_entry_file(Path("stream.py").resolve())
compiled_graph = compile_stream_to_graph(entry_namespace["main"])
runtime = Runtime()
try:
    runtime.load(compiled_graph)
finally:
    runtime.shutdown()
print("COMPILED_GRAPH=" + json.dumps(compiled_graph))
"""


@pytest.mark.parametrize(
    ("use_test_pattern_source", "source_node_name", "source_node_type"),
    [
        (
            False,
            "camerasource",
            "streamlib_media_builtins::camera_source::CameraSource",
        ),
        (
            True,
            "testpatternsource",
            "streamlib_media_builtins::test_pattern_source::TestPatternSource",
        ),
    ],
)
def test_the_scaffolded_stream_compiles_to_its_graph_and_loads_without_a_device(
    tmp_path: Path,
    use_test_pattern_source: bool,
    source_node_name: str,
    source_node_type: str,
):
    """What `new` writes compiles to the one-shape graph and `load` takes it.

    `load` resolves every node's type — the scaffold's own `@node` classes by
    their import paths — and checks every link's ports and the exposure, all
    before `run()` would touch a device.
    """
    app_directory = tmp_path / "demo"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=use_test_pattern_source)

    finished = subprocess.run(
        [sys.executable, "-c", SCAFFOLDED_STREAM_COMPILE_AND_LOAD_SCRIPT],
        cwd=app_directory,
        capture_output=True,
        text=True,
        timeout=RESOLUTION_FAILURE_TIMEOUT_SECONDS,
    )

    assert finished.returncode == 0, finished.stdout + finished.stderr
    (compiled_graph_line,) = [
        line
        for line in finished.stdout.splitlines()
        if line.startswith("COMPILED_GRAPH=")
    ]
    assert json.loads(compiled_graph_line.removeprefix("COMPILED_GRAPH=")) == {
        "stream": "main",
        "nodes": [
            {"name": source_node_name, "type": source_node_type, "config": {}},
            {
                "name": "invertingeffect",
                "type": "nodes.inverting_effect:InvertingEffect",
                "config": {},
            },
            {
                "name": "brightnessmeter",
                "type": "nodes.brightness_meter:BrightnessMeter",
                "config": {},
            },
            {
                "name": "displaywindow",
                "type": "streamlib_media_builtins::display_window::DisplayWindow",
                "config": {"title": "StreamLib", "scaling": "fit"},
            },
        ],
        "links": [
            {
                "source": {"node": source_node_name, "port": "video"},
                "target": {"node": "invertingeffect", "port": "video_from_upstream"},
            },
            {
                "source": {"node": "invertingeffect", "port": "video_to_downstream"},
                "target": {"node": "displaywindow", "port": "video"},
            },
            {
                "source": {"node": "invertingeffect", "port": "video_to_downstream"},
                "target": {"node": "brightnessmeter", "port": "video_from_upstream"},
            },
        ],
        "exposed": [{"node": "invertingeffect", "port": "video_to_downstream"}],
    }


def test_the_scaffold_depends_on_streamlib_and_numpy_only(tmp_path: Path):
    app_directory = tmp_path / "demo"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=False)

    manifest = (app_directory / "pyproject.toml").read_text()

    assert 'dependencies = ["streamlib", "numpy>=2.1"]\n' in manifest, (
        "the pixel effect needs no GPU package of the user's own — torch never enters"
    )


def test_the_test_pattern_scaffold_needs_no_capture_device(tmp_path: Path):
    app_directory = tmp_path / "demo"

    cli.scaffold_new_app(app_directory, use_test_pattern_source=True)

    entry_source = (app_directory / "stream.py").read_text()
    ast.parse(entry_source)
    assert "TestPatternSource" in entry_source
    assert "CameraSource" not in entry_source
    assert "camera" not in entry_source.lower(), (
        "the test-pattern stream's docstrings describe a test pattern, not a camera"
    )
    # The nodes are source-agnostic, so the split must not have made them vary.
    ast.parse((app_directory / cli.SCAFFOLDED_EFFECT_MODULE_PATH).read_text())
    ast.parse((app_directory / cli.SCAFFOLDED_METER_MODULE_PATH).read_text())


def test_the_scaffold_pins_streamlib_to_its_own_index(tmp_path: Path):
    app_directory = tmp_path / "demo"

    cli.scaffold_new_app(app_directory, use_test_pattern_source=True)

    manifest = (app_directory / "pyproject.toml").read_text()
    assert 'url = "https://tatolab.github.io/streamlib/simple/"' in manifest
    assert 'name = "demo"' in manifest, "the project takes its directory's name"


def test_new_refuses_to_overwrite_an_existing_app(tmp_path: Path):
    app_directory = tmp_path / "demo"
    app_directory.mkdir()
    (app_directory / "stream.py").write_text("# the user's own work\n")

    with pytest.raises(cli.AppLaunchError, match="already has stream.py"):
        cli.scaffold_new_app(app_directory, use_test_pattern_source=False)

    assert (app_directory / "stream.py").read_text() == "# the user's own work\n", (
        "a refused scaffold must leave the directory untouched"
    )
    assert not (app_directory / "pyproject.toml").exists(), (
        "nothing may be written before the whole scaffold is known to be safe"
    )


# ---------------------------------------------------------------------------
# `enable-virtual-camera` — the one machine-setup verb
# ---------------------------------------------------------------------------


def test_enable_virtual_camera_print_writes_the_three_files_and_runs_nothing(
    monkeypatch, capsys
):
    """`--print` is the hand-install path: every file, its destination, the
    commands — and no process, no privilege, no change to the machine."""

    def refuse_to_run(*_arguments, **_keywords):
        raise AssertionError("--print must run nothing")

    monkeypatch.setattr(cli.subprocess, "run", refuse_to_run)

    assert cli.enable_virtual_camera(print_only=True) == 0

    printed = capsys.readouterr()
    for destination in (
        "/etc/modules-load.d/streamlib-virtual-camera.conf",
        "/etc/modprobe.d/streamlib-virtual-camera.conf",
        "/etc/udev/rules.d/70-streamlib-virtual-camera.rules",
    ):
        assert destination in printed.out, f"{destination} missing from:\n{printed.out}"
    assert "options v4l2loopback devices=0" in printed.out
    assert 'KERNEL=="v4l2loopback", SUBSYSTEM=="misc", TAG+="uaccess"' in printed.out
    assert "modprobe v4l2loopback devices=0" in printed.out
    assert "udevadm control --reload" in printed.out
    # The trigger must select the control node, or a rule written after the
    # module loaded never applies; `--attr-match=name=` once matched nothing.
    assert "udevadm trigger --subsystem-match=misc --sysname-match=v4l2loopback" in printed.out
    assert printed.err == ""


def test_the_shipped_entry_point_carries_the_setup_verb():
    printed = run_cli("enable-virtual-camera", "--print")

    assert printed.returncode == 0, printed.stderr
    assert "70-streamlib-virtual-camera.rules" in printed.stdout


def test_enable_virtual_camera_refuses_by_name_without_pkexec_or_sudo(monkeypatch):
    """Where neither helper exists the verb names both, offers `--print`, and
    changes nothing — a machine it cannot ask for privilege on is told so."""
    monkeypatch.setattr(cli.platform, "system", lambda: "Linux")
    monkeypatch.setattr(cli, "virtual_camera_module_is_installed", lambda _release: True)
    monkeypatch.setattr(cli.shutil, "which", lambda _name: None)

    def refuse_to_run(*_arguments, **_keywords):
        raise AssertionError("with no helper nothing may run")

    monkeypatch.setattr(cli.subprocess, "run", refuse_to_run)

    with pytest.raises(cli.MachineSetupError) as refusal:
        cli.enable_virtual_camera(print_only=False)

    message = str(refusal.value)
    assert "pkexec" in message and "sudo" in message
    assert "--print" in message, "the hand-install path is offered"


def test_enable_virtual_camera_refuses_by_name_off_linux(monkeypatch):
    monkeypatch.setattr(cli.platform, "system", lambda: "Darwin")

    with pytest.raises(cli.MachineSetupError, match="Linux-only"):
        cli.enable_virtual_camera(print_only=False)


def test_enable_virtual_camera_names_the_package_when_the_module_is_not_installed(
    monkeypatch,
):
    monkeypatch.setattr(cli.platform, "system", lambda: "Linux")
    monkeypatch.setattr(cli.platform, "release", lambda: "9.9.9-test")
    monkeypatch.setattr(cli, "virtual_camera_module_is_installed", lambda _release: False)

    with pytest.raises(cli.MachineSetupError) as refusal:
        cli.enable_virtual_camera(print_only=False)

    message = str(refusal.value)
    assert "v4l2loopback-dkms" in message, message
    assert "linux-modules-9.9.9-test" in message, message


def test_the_privilege_helper_prefers_pkexec_under_a_session_and_sudo_without_one():
    available = {"pkexec": "/usr/bin/pkexec", "sudo": "/usr/bin/sudo"}
    which = lambda name: available.get(name)  # noqa: E731

    assert cli.choose_privilege_helper(which, {"DISPLAY": ":1"}) == ["pkexec"]
    assert cli.choose_privilege_helper(which, {}) == ["sudo"]
    assert cli.choose_privilege_helper(lambda name: available.get(name) if name == "pkexec" else None, {}) == ["pkexec"]
    assert cli.choose_privilege_helper(lambda _name: None, {"DISPLAY": ":1"}) is None


def test_the_control_node_probe_opens_a_character_device_without_seeking(tmp_path: Path):
    """The node is a character device: a buffered `open` seeks it and raises
    `UnsupportedOperation` — which is what the verb once crashed with after a
    successful install. A FIFO is the non-seekable stand-in a test can make."""
    import os

    fifo = tmp_path / "not-seekable"
    os.mkfifo(fifo)

    assert cli.control_node_is_writable_by_this_user(fifo) is True
    assert cli.control_node_is_writable_by_this_user(tmp_path / "absent") is False


def test_the_launcher_names_the_apps_directory_for_the_built_ins(tmp_path: Path, monkeypatch):
    """`run` and `dev` export `STREAMLIB_APP_DIRECTORY` before the app's code
    runs, so a built-in that names itself to the machine — a virtual camera's
    default label — keys on the app rather than on the shell's working
    directory. The entry file records what it sees and stops before any engine
    is built."""
    monkeypatch.delenv(cli.APP_DIRECTORY_ENVIRONMENT_VARIABLE, raising=False)
    recorded = tmp_path / "recorded-app-directory.txt"
    write_app(
        tmp_path,
        "stream.py",
        "import os\n"
        f"open({str(recorded)!r}, 'w').write(os.environ.get('STREAMLIB_APP_DIRECTORY', ''))\n"
        "raise RuntimeError('stop before the engine')\n",
    )

    exit_code = cli.launch_app_node(
        "run",
        requested_anchor_directory=tmp_path,
        requested_entry_file=None,
        requested_stream_target=None,
        requested_stream_name=None,
        bind_host=cli.DEFAULT_CONTROL_PLANE_BIND_HOST,
        bind_port=cli.DEFAULT_CONTROL_PLANE_BIND_PORT,
        runtime_name=None,
    )

    assert exit_code == 1, "the entry file stopped the launch on purpose"
    assert recorded.read_text() == str(tmp_path), (
        "the app's anchor directory must reach the app's own code through the environment"
    )


def test_a_runtime_name_the_engine_refuses_reads_as_a_launcher_error(tmp_path):
    """A refused `--runtime-name` is a wiring mistake, not a launcher crash.

    The engine owns the grammar, so the CLI cannot pre-check the name without
    keeping a second copy of it — it reports what the engine said instead, the
    way it reports a bad config or a missing camera.
    """
    write_app(tmp_path, "stream.py", MINIMAL_STREAM_SOURCE)

    with pytest.raises(cli.AppLaunchError, match="'/'") as refusal:
        cli.launch_app_node(
            "run",
            requested_anchor_directory=tmp_path,
            requested_entry_file=None,
            requested_stream_target=None,
            requested_stream_name=None,
            bind_host=cli.DEFAULT_CONTROL_PLANE_BIND_HOST,
            bind_port=cli.DEFAULT_CONTROL_PLANE_BIND_PORT,
            runtime_name="a/b",
        )

    assert "a/b" in str(refusal.value), (
        "the refusal must name the runtime name the caller asked for"
    )


def test_the_nodes_help_names_every_column_it_prints(capsys):
    with pytest.raises(SystemExit):
        cli.main(["nodes", "--help"])

    printed = capsys.readouterr().out
    for column in ("runtime_name", "runtime_id", "control_url", "pid", "alive?", "hint"):
        assert column in printed, f"`nodes --help` must document {column}"


def _v4l2loopback_is_loaded() -> bool:
    try:
        return "v4l2loopback" in Path("/proc/modules").read_text()
    except OSError:
        return False


@pytest.mark.linux_only_capability(reason="v4l2loopback and udev are Linux")
@pytest.mark.skipif(
    sys.platform == "linux" and not _v4l2loopback_is_loaded(),
    reason="v4l2loopback is not loaded here",
)
def test_the_udev_trigger_selects_the_control_node():
    """The re-trigger the verb runs must name the module's misc device, so the
    freshly written `uaccess` rule is applied to a node that already exists.
    `--dry-run --verbose` prints what would be triggered and touches nothing."""
    script = cli.virtual_camera_privileged_script()
    trigger = next(line for line in script.splitlines() if line.startswith("udevadm trigger"))
    words = trigger.split()

    listed = subprocess.run(
        [*words[:2], "--dry-run", "--verbose", *words[2:]],
        capture_output=True,
        text=True,
        check=True,
    )

    assert "/sys/devices/virtual/misc/v4l2loopback" in listed.stdout, (
        f"the trigger selects nothing; command: {trigger}; output: {listed.stdout!r}"
    )


@pytest.mark.linux_only_capability(reason="v4l2loopback and udev are Linux")
@pytest.mark.skipif(
    sys.platform == "linux" and os.environ.get("STREAMLIB_RUN_PRIVILEGED_VERB") != "1",
    reason=(
        "runs the privileged verb (a password prompt); set "
        "STREAMLIB_RUN_PRIVILEGED_VERB=1 in a terminal to opt in"
    ),
)
def test_enable_virtual_camera_makes_the_control_node_writable():
    """The rig check: the verb, run for real, leaves the control node openable
    read-write by this user in this same session — no re-login."""
    finished = subprocess.run(
        [sys.executable, "-m", "streamlib.cli", "enable-virtual-camera"],
        text=True,
        timeout=180,
    )

    assert finished.returncode == 0
    assert cli.control_node_is_writable_by_this_user() is True


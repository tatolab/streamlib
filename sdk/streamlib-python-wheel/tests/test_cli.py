# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `streamlib` console script: entry resolution, `setup(rt)`, and `new`.

Nothing here boots an engine. Entry resolution and scaffolding are pure
functions over the filesystem, and the failure paths are the point: an app that
cannot be executed must produce the user's traceback and exit, never a GPU
context and a stack dump. The launch-to-a-live-node half needs a device and
lives in `test_cli_launch.py`.
"""

import ast
import subprocess
import sys
from pathlib import Path
from typing import cast

import pytest
from app_under_test import ENGINE_STARTING_LOG_LINE

from streamlib import Runtime, cli

MINIMAL_APP_SOURCE = "def setup(rt):\n    pass\n"

# Bounded: a resolution failure exits before anything is built, so a run that
# reaches this deadline has booted a node instead of failing.
RESOLUTION_FAILURE_TIMEOUT_SECONDS = 60.0


def write_app(directory: Path, file_name: str, source: str = MINIMAL_APP_SOURCE) -> Path:
    entry_file = directory / file_name
    entry_file.parent.mkdir(parents=True, exist_ok=True)
    entry_file.write_text(source, encoding="utf-8")
    return entry_file


def run_cli(*arguments: str) -> "subprocess.CompletedProcess[str]":
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
    )


# ---------------------------------------------------------------------------
# Entry resolution — the `app.py` convention
# ---------------------------------------------------------------------------


def test_no_args_resolves_the_conventional_entry_at_the_anchor(tmp_path: Path):
    write_app(tmp_path, "app.py")

    resolved = cli.resolve_app_entry_file("run", tmp_path, None)

    assert resolved == tmp_path / "app.py"


def test_explicit_entry_file_overrides_the_convention(tmp_path: Path):
    write_app(tmp_path, "app.py")
    write_app(tmp_path, "other.py")

    resolved = cli.resolve_app_entry_file("run", tmp_path, Path("other.py"))

    assert resolved == tmp_path / "other.py"


def test_explicit_entry_file_may_be_absolute(tmp_path: Path):
    absolute_entry = write_app(tmp_path, "elsewhere.py")

    resolved = cli.resolve_app_entry_file("dev", tmp_path, absolute_entry)

    assert resolved == absolute_entry


def test_a_missing_conventional_entry_names_the_convention_and_the_anchor(tmp_path: Path):
    with pytest.raises(cli.AppLaunchError) as resolution_failure:
        cli.resolve_app_entry_file("dev", tmp_path, None)

    message = str(resolution_failure.value)
    assert "app.py" in message, "the error must name the convention"
    assert str(tmp_path) in message, "the error must name the anchor it searched"
    assert "streamlib dev" in message, "the error must name the verb the user typed"
    assert "-f " in message, "the error must offer the `-f` escape hatch"


def test_a_missing_explicit_entry_names_the_path_it_tried(tmp_path: Path):
    with pytest.raises(cli.AppLaunchError, match="gone.py"):
        cli.resolve_app_entry_file("run", tmp_path, Path("gone.py"))


def test_resolution_never_walks_up_to_a_parent(tmp_path: Path):
    write_app(tmp_path, "app.py")
    nested = tmp_path / "nested"
    nested.mkdir()

    with pytest.raises(cli.AppLaunchError, match="never searches parent"):
        cli.resolve_app_entry_file("run", nested, None)


def test_a_directory_named_like_the_entry_is_not_an_entry(tmp_path: Path):
    (tmp_path / "app.py").mkdir()

    with pytest.raises(cli.AppLaunchError):
        cli.resolve_app_entry_file("run", tmp_path, None)


def test_the_anchor_is_the_cwd_when_dir_is_absent():
    assert cli.resolve_app_anchor_directory(None) == Path.cwd()


def test_the_anchor_is_the_dir_flag_when_given(tmp_path: Path):
    assert cli.resolve_app_anchor_directory(tmp_path) == tmp_path


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
    """`streamlib dev` and `python app.py` must be the same arrangement.

    An app importing its own `processors/` package is the case that breaks if
    the entry's directory is not what leads `sys.path`.
    """
    write_app(tmp_path, "processors/__init__.py", "")
    write_app(tmp_path, "processors/effect.py", "EFFECT_NAME = 'blur'\n")
    entry_file = write_app(
        tmp_path,
        "app.py",
        "from processors.effect import EFFECT_NAME\n"
        "MODULE_NAME = __name__\n"
        "def setup(rt):\n    pass\n",
    )

    namespace = cli.execute_app_entry_file(entry_file)

    assert namespace["EFFECT_NAME"] == "blur"
    assert namespace["MODULE_NAME"] == "__main__", (
        "the entry must run under the name `python app.py` gives it"
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
# The failure surfaces — a bad save must cost nothing
# ---------------------------------------------------------------------------


def test_a_syntax_error_prints_the_apps_traceback_and_builds_no_engine(tmp_path: Path):
    """The bad-save path. A broken entry file is the user's typo, not a crash.

    Reaching the timeout is the regression this guards: an engine built before
    the entry file ran would boot a node and block instead of exiting.
    """
    write_app(tmp_path, "app.py", "def setup(rt)\n    pass\n")

    finished = run_cli("dev", "--dir", str(tmp_path))

    assert finished.returncode == 1, f"stderr was:\n{finished.stderr}"
    assert "SyntaxError" in finished.stderr, (
        f"the user's own error must be the headline; stderr was:\n{finished.stderr}"
    )
    assert "app.py" in finished.stderr, "the traceback must name the file"
    assert "Initializing GPU context" not in finished.stdout + finished.stderr, (
        "a file that cannot be executed must not cost an engine boot"
    )


def test_a_bad_save_in_the_effect_module_names_that_module_not_the_entry_file(
    tmp_path: Path,
):
    """The bad save the scaffold actually invites.

    `app.py` holds wiring the user rarely touches; the file they edit is the
    processor module, which reaches the launcher only as an import from the
    entry file. So the traceback has to walk through `app.py` and land in the
    module — naming only the entry file would point at the wrong file.
    """
    app_directory = tmp_path / "demo"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=True)
    (app_directory / "processors" / "inverting_effect.py").write_text(
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
    write_app(tmp_path, "app.py", "raise ValueError('bad wiring')\n")

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
    write_app(tmp_path, "app.py", "raise ValueError('bad wiring')\n")

    finished = run_cli("run", "--dir", str(tmp_path))

    assert "runpy" not in finished.stderr, (
        f"no launcher frame may appear in the app's traceback; stderr was:\n{finished.stderr}"
    )
    assert "cli.py" not in finished.stderr, (
        f"the launcher's own frames must be stripped; stderr was:\n{finished.stderr}"
    )
    assert "app.py" in finished.stderr


def test_the_app_does_not_see_the_launchers_arguments(tmp_path: Path):
    """`sys.argv` belongs to the app, not to `streamlib run`."""
    write_app(
        tmp_path,
        "app.py",
        "import sys\nARGV = list(sys.argv)\ndef setup(rt):\n    pass\n",
    )
    entry_file = tmp_path / "app.py"

    namespace = cli.execute_app_entry_file(entry_file)

    assert namespace["ARGV"] == [str(entry_file)], (
        "the app must see only its own path, as `python app.py` gives it"
    )


def test_the_launcher_restores_its_own_argv(tmp_path: Path):
    write_app(tmp_path, "app.py")
    launcher_argv = list(sys.argv)

    cli.execute_app_entry_file(tmp_path / "app.py")

    assert sys.argv == launcher_argv


def test_an_app_that_exits_on_purpose_keeps_its_own_exit_code(tmp_path: Path):
    """`sys.exit()` at module scope is a choice, not a failure to report."""
    write_app(tmp_path, "app.py", "import sys\nsys.exit(3)\n")

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


def test_an_observation_verb_names_itself_rather_than_looking_like_a_typo(
    tmp_path: Path,
):
    """`nodes` / `graph` / `tap` / `logs` / `mcp` are plan-promised but still
    live in the engine's own CLI. Argparse's `invalid choice` reads like the
    user mistyped; this says where the verb actually is."""
    finished = run_cli("nodes")

    assert finished.returncode == 1
    assert "not in this wheel yet" in finished.stderr, (
        f"stderr was:\n{finished.stderr}"
    )
    assert "invalid choice" not in finished.stderr


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
    "app.py",
    "processors/__init__.py",
    "processors/inverting_effect.py",
    "pyproject.toml",
    ".python-version",
    ".gitignore",
)


def test_new_writes_a_working_app(tmp_path: Path):
    app_directory = tmp_path / "demo"

    cli.scaffold_new_app(app_directory, use_test_pattern_source=False)

    for file_name in SCAFFOLDED_FILE_NAMES:
        assert (app_directory / file_name).is_file(), f"`new` must write {file_name}"
    assert (app_directory / ".python-version").read_text().strip() == "3.12", (
        "the scaffold pins the Python version the plan names"
    )


def test_the_scaffolded_app_parses_and_declares_setup(tmp_path: Path):
    """The scaffold is the first code the user reads — it must at least parse.

    Parsed rather than executed: importing it would need a GPU and a camera,
    and what this locks is that `dev` finds a `setup` in what `new` wrote.
    """
    app_directory = tmp_path / "demo"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=False)

    entry_source = (app_directory / "app.py").read_text()
    declared = ast.parse(entry_source)
    top_level_functions = [
        node.name for node in declared.body if isinstance(node, ast.FunctionDef)
    ]

    assert "setup" in top_level_functions, "`dev` finds `setup(rt)` by convention"
    assert "CameraSource" in entry_source
    assert "DisplayWindow" in entry_source


def test_the_scaffolded_processor_lives_outside_the_entry_file(tmp_path: Path):
    """A processor class in the entry file identifies as `__main__:<Type>`,
    which is a wiring error — the entry runs as `__main__`, and the child
    interpreter that runs the processor imports its class by name.

    So the scaffold must teach the shape that works: wiring in `app.py`, the
    class in an importable module beside it.
    """
    app_directory = tmp_path / "demo"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=False)

    entry_source = (app_directory / "app.py").read_text()
    effect_source = (app_directory / "processors" / "inverting_effect.py").read_text()

    assert "@processor" not in entry_source, (
        "a processor class in the entry file would identify as `__main__:<Type>`"
    )
    assert "class InvertingEffect" not in entry_source
    assert "@processor" in effect_source, "the class belongs in the importable module"
    assert "class InvertingEffect" in effect_source
    assert "from processors.inverting_effect import InvertingEffect" in entry_source, (
        "the entry file imports the class it wires"
    )
    # Both halves must parse — the entry is useless if its effect module is not.
    ast.parse(entry_source)
    ast.parse(effect_source)


def test_the_test_pattern_scaffold_needs_no_capture_device(tmp_path: Path):
    app_directory = tmp_path / "demo"

    cli.scaffold_new_app(app_directory, use_test_pattern_source=True)

    entry_source = (app_directory / "app.py").read_text()
    ast.parse(entry_source)
    assert "TestPatternSource" in entry_source
    assert "CameraSource" not in entry_source
    # The effect is source-agnostic, so the split must not have made it vary.
    ast.parse((app_directory / "processors" / "inverting_effect.py").read_text())


def test_the_scaffold_pins_streamlib_to_its_own_index(tmp_path: Path):
    app_directory = tmp_path / "demo"

    cli.scaffold_new_app(app_directory, use_test_pattern_source=True)

    manifest = (app_directory / "pyproject.toml").read_text()
    assert cli.STREAMLIB_SIMPLE_INDEX_URL in manifest
    assert 'name = "demo"' in manifest, "the project takes its directory's name"


def test_new_refuses_to_overwrite_an_existing_app(tmp_path: Path):
    app_directory = tmp_path / "demo"
    app_directory.mkdir()
    (app_directory / "app.py").write_text("# the user's own work\n")

    with pytest.raises(cli.AppLaunchError, match="already has app.py"):
        cli.scaffold_new_app(app_directory, use_test_pattern_source=False)

    assert (app_directory / "app.py").read_text() == "# the user's own work\n", (
        "a refused scaffold must leave the directory untouched"
    )
    assert not (app_directory / "pyproject.toml").exists(), (
        "nothing may be written before the whole scaffold is known to be safe"
    )

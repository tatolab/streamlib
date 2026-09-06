# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `streamlib` console script: entry resolution, `setup(rt)`, and `new`.

Nothing here boots an engine. Entry resolution and scaffolding are pure
functions over the filesystem, and the failure paths are the point: an app that
cannot be executed must produce the user's traceback and exit, never a GPU
context and a stack dump. The launch-to-a-live-node half needs a device and
lives in `test_cli_launch.py`.
"""

import argparse
import ast
import os
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


def test_the_observation_verbs_are_served_by_this_wheel(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """`nodes` / `graph` / `tap` / `logs` are this CLI's, not another binary's.

    This replaces the stopgap that used to name where the verbs "actually
    lived": they live here now. `nodes` is the one that answers without a
    running node to talk to, so it proves the verb is wired end to end rather
    than merely present in the parser.

    `XDG_RUNTIME_DIR` is redirected first: `nodes` liveness-checks and prunes
    every entry it finds, and this test must not reach a real node on a
    developer's machine, let alone delete its registry entry.
    """
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path))
    finished = run_cli("nodes")

    assert finished.returncode == 0, f"stderr was:\n{finished.stderr}"
    assert "invalid choice" not in finished.stderr
    assert "not in this wheel yet" not in finished.stderr

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

    assert "linux-modules-9.9.9-test" in str(refusal.value)


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
        "app.py",
        "import os\n"
        f"open({str(recorded)!r}, 'w').write(os.environ.get('STREAMLIB_APP_DIRECTORY', ''))\n"
        "raise RuntimeError('stop before the engine')\n",
    )

    exit_code = cli.launch_app_node(
        "run",
        requested_anchor_directory=tmp_path,
        requested_entry_file=None,
        bind_host=cli.DEFAULT_CONTROL_PLANE_BIND_HOST,
        bind_port=cli.DEFAULT_CONTROL_PLANE_BIND_PORT,
        node_name=None,
    )

    assert exit_code == 1, "the entry file stopped the launch on purpose"
    assert recorded.read_text() == str(tmp_path), (
        "the app's anchor directory must reach the app's own code through the environment"
    )


def _v4l2loopback_is_loaded() -> bool:
    try:
        return "v4l2loopback" in Path("/proc/modules").read_text()
    except OSError:
        return False


@pytest.mark.skipif(not _v4l2loopback_is_loaded(), reason="v4l2loopback is not loaded here")
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


@pytest.mark.skipif(
    os.environ.get("STREAMLIB_RUN_PRIVILEGED_VERB") != "1",
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


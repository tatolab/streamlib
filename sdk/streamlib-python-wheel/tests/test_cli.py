# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `streamlib` console script: the verbs it serves, and `enable-virtual-camera`.

Nothing here boots an engine.
"""

import argparse
import os
import subprocess
import sys
from pathlib import Path

import pytest

from tatolab.runtime import _node_registry, cli

CLI_CHILD_PROCESS_TIMEOUT_SECONDS = 60.0


def run_cli(
    *arguments: str, environment: "dict[str, str] | None" = None
) -> "subprocess.CompletedProcess[str]":
    """Drive the console script's module entry in a child interpreter.

    `-m tatolab.runtime.cli` rather than the installed `streamlib` binary so the test
    is about the code, not about whether this environment's `bin/` is on PATH —
    the shipped entry point is checked separately.
    """
    return subprocess.run(
        [sys.executable, "-m", "tatolab.runtime.cli", *arguments],
        capture_output=True,
        text=True,
        timeout=CLI_CHILD_PROCESS_TIMEOUT_SECONDS,
        env=environment,
    )


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
        "nodes",
        "graph",
        "tap",
        "logs",
        "exchange",
        "mcp",
        # The one machine-setup verb: touches no node, speaks no control plane.
        "enable-virtual-camera",
    }



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


def test_the_nodes_help_names_every_column_it_prints(capsys):
    with pytest.raises(SystemExit):
        cli.main(["nodes", "--help"])

    printed = capsys.readouterr().out
    for column in ("runtime_name", "runtime_id", "local_api_socket", "pid", "alive?", "hint"):
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
        [sys.executable, "-m", "tatolab.runtime.cli", "enable-virtual-camera"],
        text=True,
        timeout=180,
    )

    assert finished.returncode == 0
    assert cli.control_node_is_writable_by_this_user() is True


# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Where a runtime keeps its live files, read from Python.

`streamlib nodes` finds what a runtime wrote only if the wheel's reader resolves
the runtime directory exactly as the engine does. `Runtime()` opens its iceoryx2
node and surface socket without starting, so the agreement is checked against
a real engine on every pull request, with no device.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Iterator

import pytest

from streamlib._node_registry import (
    UntrustedRuntimeDirectoryError,
    _resolve_runtime_directory,
    registry_directory,
    runtime_directory,
)

PER_USER_FALLBACK = Path("/tmp") / f"streamlib-{os.getuid()}"


@pytest.fixture
def short_xdg_runtime_dir() -> Iterator[Path]:
    """Short enough that the socket and the iceoryx2 root fit their path budgets."""
    directory = Path(tempfile.mkdtemp(prefix="sl-"))
    try:
        yield directory
    finally:
        shutil.rmtree(directory, ignore_errors=True)


@pytest.fixture
def shared_temporary_directory(tmp_path: Path) -> Path:
    """A stand-in for `/tmp`, so a refusal never touches the machine's own."""
    return tmp_path


def test_a_set_xdg_runtime_dir_resolves_to_its_streamlib_folder(
    monkeypatch: pytest.MonkeyPatch, short_xdg_runtime_dir: Path
):
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(short_xdg_runtime_dir))
    expected = short_xdg_runtime_dir / "streamlib"
    if sys.platform != "linux":
        expected = PER_USER_FALLBACK

    assert runtime_directory() == expected
    assert registry_directory() == expected / "nodes"


def test_an_empty_xdg_runtime_dir_takes_the_per_user_fallback(
    monkeypatch: pytest.MonkeyPatch,
):
    monkeypatch.setenv("XDG_RUNTIME_DIR", "")

    assert runtime_directory() == PER_USER_FALLBACK


def test_an_unset_xdg_runtime_dir_takes_the_per_user_fallback(
    monkeypatch: pytest.MonkeyPatch,
):
    monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)

    assert runtime_directory() == PER_USER_FALLBACK


def test_macos_takes_the_per_user_fallback_whatever_xdg_runtime_dir_says(
    shared_temporary_directory: Path,
):
    resolved = _resolve_runtime_directory(
        "/run/user/1000", "darwin", shared_temporary_directory, os.getuid()
    )

    assert resolved == shared_temporary_directory / f"streamlib-{os.getuid()}"


def test_a_fallback_that_does_not_exist_yet_is_resolved_without_being_created(
    shared_temporary_directory: Path,
):
    resolved = _resolve_runtime_directory(
        None, "linux", shared_temporary_directory, os.getuid()
    )

    assert resolved == shared_temporary_directory / f"streamlib-{os.getuid()}"
    assert not resolved.exists()


def test_a_fallback_that_is_a_symlink_is_refused_by_name(
    shared_temporary_directory: Path, tmp_path_factory: pytest.TempPathFactory
):
    somewhere_else = tmp_path_factory.mktemp("somewhere-else")
    somewhere_else.chmod(0o700)
    fallback = shared_temporary_directory / f"streamlib-{os.getuid()}"
    fallback.symlink_to(somewhere_else)

    with pytest.raises(UntrustedRuntimeDirectoryError, match="symlink") as refusal:
        _resolve_runtime_directory(None, "linux", shared_temporary_directory, os.getuid())
    assert str(fallback) in str(refusal.value)


def test_a_fallback_owned_by_another_uid_is_refused_by_name(
    shared_temporary_directory: Path,
):
    another_uid = os.getuid() + 1
    fallback = shared_temporary_directory / f"streamlib-{another_uid}"
    fallback.mkdir(mode=0o700)

    with pytest.raises(
        UntrustedRuntimeDirectoryError,
        match=f"owned by uid {os.getuid()}, not uid {another_uid}",
    ) as refusal:
        _resolve_runtime_directory(None, "linux", shared_temporary_directory, another_uid)
    assert str(fallback) in str(refusal.value)


@pytest.mark.parametrize("mode", [0o755, 0o770])
def test_a_fallback_open_to_other_users_or_its_group_is_refused_by_name(
    shared_temporary_directory: Path, mode: int
):
    fallback = shared_temporary_directory / f"streamlib-{os.getuid()}"
    fallback.mkdir()
    fallback.chmod(mode)

    with pytest.raises(UntrustedRuntimeDirectoryError, match=f"mode is {mode:o}") as refusal:
        _resolve_runtime_directory(None, "linux", shared_temporary_directory, os.getuid())
    assert str(fallback) in str(refusal.value)


RUNTIME_THAT_REPORTS_WHERE_IT_OPENED = """
import json, os, sys
from pathlib import Path

import streamlib
from streamlib._node_registry import runtime_directory

resolved = runtime_directory()
sockets_before = {str(path) for path in resolved.glob("surface-share-*.sock")}
runtime = streamlib.Runtime()
try:
    print(json.dumps({
        "resolved_by_the_reader": str(resolved),
        "domain_files": [str(path) for path in (resolved / "iox2").rglob("*")],
        "new_sockets": sorted(
            {str(path) for path in resolved.glob("surface-share-*.sock")} - sockets_before
        ),
    }))
finally:
    runtime.shutdown()
"""


@pytest.mark.parametrize("xdg_runtime_dir_arm", ["set", "empty", "unset"])
def test_the_reader_resolves_the_directory_a_runtime_opened_its_domain_in(
    short_xdg_runtime_dir: Path, xdg_runtime_dir_arm: str
):
    """The engine's half of the agreement is what it actually created: its
    iceoryx2 domain and, on Linux, its surface socket. The reader must name the
    directory holding both.

    A child process each: the first `Runtime()` in a process pins the process's
    event bus to its own domain, so one arm's directory must not outlive into
    another's runtime.
    """
    environment = {**os.environ}
    if xdg_runtime_dir_arm == "set":
        environment["XDG_RUNTIME_DIR"] = str(short_xdg_runtime_dir)
    elif xdg_runtime_dir_arm == "empty":
        environment["XDG_RUNTIME_DIR"] = ""
    else:
        environment.pop("XDG_RUNTIME_DIR", None)

    finished = subprocess.run(
        [sys.executable, "-c", RUNTIME_THAT_REPORTS_WHERE_IT_OPENED],
        env=environment,
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert finished.returncode == 0, finished.stderr
    report = json.loads(finished.stdout.strip().splitlines()[-1])

    resolved = Path(report["resolved_by_the_reader"])
    if xdg_runtime_dir_arm == "set" and sys.platform == "linux":
        assert resolved == short_xdg_runtime_dir / "streamlib"
    else:
        assert resolved == PER_USER_FALLBACK
    assert any(
        Path(domain_file).name.startswith(f"sl{os.getuid()}_")
        for domain_file in report["domain_files"]
    ), f"the runtime's iceoryx2 domain must be in {resolved / 'iox2'}: {report}"
    if sys.platform == "linux":
        assert len(report["new_sockets"]) == 1, (
            f"the runtime's surface socket must be in {resolved}: {report}"
        )

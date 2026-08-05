# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`streamlib run` / `dev` booting a real node, end to end.

What these lock is that a Python-launched app is a first-class node: its
`setup(rt)` built the graph, it published a node-registry entry the observation
verbs discover, and a clean interrupt takes the entry away again. Booting
initializes a GPU context, so the whole module needs a device.
"""

import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import pytest

pytestmark = pytest.mark.requires_gpu

# Boot is process start + engine init + GPU context + socket bind.
NODE_READY_TIMEOUT_SECONDS = 90.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0

APP_WITH_ONE_NATIVE_SOURCE = '''\
from streamlib import TestPatternSource


def setup(rt):
    rt.add(TestPatternSource, config={"width": 320, "height": 180})
'''


def free_port() -> int:
    """A port the OS reports free. The control plane increments on collision,
    so a caller that loses the race still binds nearby."""
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def registry_entry_paths(runtime_directory: Path) -> "list[Path]":
    nodes_directory = runtime_directory / "streamlib" / "nodes"
    if not nodes_directory.is_dir():
        return []
    return sorted(nodes_directory.glob("*.json"))


def await_sole_registry_entry(runtime_directory: Path, timeout: float) -> dict:
    """Poll until exactly one node entry exists, and return it decoded."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        paths = registry_entry_paths(runtime_directory)
        if len(paths) == 1:
            try:
                return json.loads(paths[0].read_text())
            except (json.JSONDecodeError, OSError):
                # A partially-written entry: the writer is mid-publish.
                pass
        time.sleep(0.2)
    raise AssertionError(
        f"no node-registry entry appeared within {timeout}s — the launched app "
        f"never hosted its control plane"
    )


class LaunchedNode:
    """A `streamlib <verb>` child, killed by its process group however the test ends."""

    def __init__(self, process: "subprocess.Popen[str]") -> None:
        self.process = process

    def interrupt(self) -> None:
        self.process.send_signal(signal.SIGINT)

    def await_exit(self, timeout: float) -> int:
        return self.process.wait(timeout=timeout)

    def kill_process_group(self) -> None:
        try:
            os.killpg(os.getpgid(self.process.pid), signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass


@pytest.fixture
def isolated_runtime_directory():
    """A private `XDG_RUNTIME_DIR`, kept short enough to hold a unix socket.

    Not `tmp_path`: the engine's surface-share socket lives in here, and
    pytest's per-test directory names are long enough that the resulting path
    blows `sun_path`'s 108-byte limit — the engine then fails to start, and
    every assertion downstream reports the wrong thing.
    """
    runtime_directory = Path(tempfile.mkdtemp(prefix="sl-"))
    try:
        yield runtime_directory
    finally:
        shutil.rmtree(runtime_directory, ignore_errors=True)


@pytest.fixture
def launch_node(isolated_runtime_directory: Path):
    """Launches nodes and leaves nothing holding a GPU context behind."""
    launched: "list[LaunchedNode]" = []

    def launch(verb: str, app_directory: Path, port: int) -> LaunchedNode:
        node = LaunchedNode(
            subprocess.Popen(
                [
                    sys.executable, "-m", "streamlib.cli", verb,
                    "--dir", str(app_directory),
                    "--host", "127.0.0.1",
                    "--port", str(port),
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                text=True,
                start_new_session=True,
                env={**os.environ, "XDG_RUNTIME_DIR": str(isolated_runtime_directory)},
            )
        )
        launched.append(node)
        return node

    try:
        yield launch
    finally:
        for node in launched:
            node.kill_process_group()


@pytest.mark.parametrize("verb", ["run", "dev"])
def test_a_launched_app_registers_as_a_node_and_tears_down(
    verb: str, tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """The MVP minute's observable half, for both verbs.

    `run` and `dev` share one launch path — the only differences are the
    diagnostics `dev` arms — so a divergence in boot, registration or teardown
    between them is a defect in the path itself.
    """
    app_directory = tmp_path / "app"
    app_directory.mkdir()
    (app_directory / "app.py").write_text(APP_WITH_ONE_NATIVE_SOURCE)
    runtime_directory = isolated_runtime_directory

    node = launch_node(verb, app_directory, free_port())
    entry = await_sole_registry_entry(runtime_directory, NODE_READY_TIMEOUT_SECONDS)

    assert entry["pid"] == node.process.pid, "the entry must name the hosting process"
    assert entry["control_url"].startswith("http://127.0.0.1:"), (
        f"the entry must carry a reachable control URL; got {entry['control_url']}"
    )

    node.interrupt()
    assert node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        f"`streamlib {verb}` must exit cleanly on SIGINT"
    )
    assert registry_entry_paths(runtime_directory) == [], (
        "clean teardown must remove the node-registry entry"
    )


def test_a_setup_that_raises_publishes_no_node(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """A graph that failed to build must not leave a node advertising itself."""
    app_directory = tmp_path / "app"
    app_directory.mkdir()
    (app_directory / "app.py").write_text(
        "def setup(rt):\n    raise ValueError('bad wiring')\n"
    )
    runtime_directory = isolated_runtime_directory

    node = launch_node("dev", app_directory, free_port())

    assert node.await_exit(NODE_READY_TIMEOUT_SECONDS) == 1, (
        "a raising `setup` must exit non-zero"
    )
    assert registry_entry_paths(runtime_directory) == [], (
        "the control plane must be hosted only after `setup` succeeded"
    )

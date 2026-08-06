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
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import pytest

from streamlib import cli

pytestmark = pytest.mark.requires_gpu

# Boot is process start + engine init + GPU context + socket bind.
NODE_READY_TIMEOUT_SECONDS = 90.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0
# Many frames at any watchable rate — long enough that a per-frame failure or a
# per-frame slowdown cannot hide inside it.
SCAFFOLD_OBSERVATION_WINDOW_SECONDS = 6.0
# The source runs at 30fps, so a healthy effect delivers ~180 frames in the
# window. The floor sits far below that and far above the ~25 the in-place
# strided edit managed, so it fails on a regression and not on a slow machine.
MINIMUM_FRAMES_FOR_LIVE_VIDEO = 60

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

    def __init__(
        self, process: "subprocess.Popen[str]", output_file: "Path | None" = None
    ) -> None:
        self.process = process
        self.output_file = output_file

    def captured_output(self) -> str:
        """Everything the child wrote, for a test that asserts on its report."""
        assert self.output_file is not None, "this node was launched without capture"
        return self.output_file.read_text(errors="replace")

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

    def launch(
        verb: str, app_directory: Path, port: int, capture_output: bool = False
    ) -> LaunchedNode:
        # A file rather than a pipe: nothing here reads the child while it runs,
        # and a full pipe buffer would wedge a node the test is still polling.
        output_file = app_directory / "node-output.log" if capture_output else None
        output_sink = (
            open(output_file, "w", encoding="utf-8") if output_file is not None else None
        )
        try:
            process = subprocess.Popen(
                [
                    sys.executable, "-m", "streamlib.cli", verb,
                    "--dir", str(app_directory),
                    "--host", "127.0.0.1",
                    "--port", str(port),
                ],
                stdout=output_sink if output_sink is not None else subprocess.DEVNULL,
                stderr=(
                    subprocess.STDOUT if output_sink is not None else subprocess.DEVNULL
                ),
                text=True,
                start_new_session=True,
                env={**os.environ, "XDG_RUNTIME_DIR": str(isolated_runtime_directory)},
            )
        finally:
            if output_sink is not None:
                output_sink.close()
        node = LaunchedNode(process, output_file)
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


def test_a_native_block_added_without_config_reaches_a_running_graph(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """`rt.add(TestPatternSource)` with no `config` — the spelling the plan
    blesses for a block that needs no configuration.

    The config travels to the engine as JSON and every field of a built-in's
    config struct carries a serde default, so `{}` deserializes and `null` does
    not. Sending null made this exact line fail at graph-compile time, which is
    after `setup` returned and therefore after every Python-side check passed.
    """
    app_directory = tmp_path / "app"
    app_directory.mkdir()
    (app_directory / "app.py").write_text(
        "from streamlib import TestPatternSource\n"
        "\n"
        "\n"
        "def setup(rt):\n"
        "    rt.add(TestPatternSource)\n"
    )

    node = launch_node("run", app_directory, free_port())
    entry = await_sole_registry_entry(
        isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS
    )

    assert entry["pid"] == node.process.pid, (
        "the graph must compile and start with no config given"
    )


@pytest.mark.xfail(
    strict=True,
    reason=(
        "the scaffolded effect processor reaches for GPU pixels, and the "
        "cross-process pixel path is owed by #1714 — the child spawns and imports "
        "its class, then every frame refuses at `ctx.gpu_limited_access`"
    ),
)
def test_the_scaffolded_app_reaches_a_running_graph(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """What `streamlib new` writes must actually run, frame after frame.

    Run exactly as scaffolded — window included, which is why this is rig-only.
    A registry entry alone proves almost nothing here: it appears whether or not
    `process()` ever succeeds, so the assertions that carry this test are the
    ones on the child's own output. `process() failed` catches an effect that
    raises every frame; the delivered-frame count catches an effect that is
    correct but so slow the demo is a slideshow — which is what editing the
    write-combined mapping in place through a strided view produced (~4fps).
    """
    app_directory = tmp_path / "app"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=True)

    node = launch_node("dev", app_directory, free_port(), capture_output=True)
    entry = await_sole_registry_entry(
        isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS
    )
    assert entry["pid"] == node.process.pid

    # Long enough for the source to have driven many frames through the effect.
    time.sleep(SCAFFOLD_OBSERVATION_WINDOW_SECONDS)
    node.interrupt()
    node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS)
    output = node.captured_output()

    assert "process() failed" not in output, (
        f"the scaffolded effect raised on a live frame; output was:\n{output}"
    )
    # The window reports what it actually put on screen, which is the honest
    # measure of "live video" — the in-place effect managed roughly 4 a second.
    frames_shown = re.search(r"DisplayWindow: stopped \((\d+) frames\)", output)
    assert frames_shown, f"the window never reported a frame count; output was:\n{output}"
    assert int(frames_shown.group(1)) >= MINIMUM_FRAMES_FOR_LIVE_VIDEO, (
        f"the app `streamlib new` writes showed only {frames_shown.group(1)} frames in "
        f"{SCAFFOLD_OBSERVATION_WINDOW_SECONDS}s — that is a slideshow, not live video"
    )


def test_a_bad_config_is_reported_without_a_launcher_traceback(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """The engine compiles the graph at `run()`, so a bad config surfaces after
    `setup` returned. It is still the app's problem, not a launcher crash."""
    app_directory = tmp_path / "app"
    app_directory.mkdir()
    (app_directory / "app.py").write_text(
        "from streamlib import TestPatternSource\n"
        "\n"
        "\n"
        "def setup(rt):\n"
        '    rt.add(TestPatternSource, config={"width": "not a number"})\n'
    )

    node = launch_node("run", app_directory, free_port(), capture_output=True)

    assert node.await_exit(NODE_READY_TIMEOUT_SECONDS) == 1
    output = node.captured_output()
    assert "Traceback (most recent call last)" not in output, (
        f"an engine-side failure must not arrive as a launcher traceback; output was:\n{output}"
    )
    assert "error:" in output


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

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`streamlib run` / `dev` booting a real node, end to end.

What these lock is that a Python-launched app is a first-class node: its
`setup(rt)` built the graph, it published a node-registry entry the observation
verbs discover, and a clean interrupt takes the entry away again. Booting
initializes a GPU context, so the whole module needs a device.

The MVP minute is measured here too, with every processor in its own child
interpreter: what `new` writes runs frame after frame, a graph of helpers goes
live inside the startup budget their interpreters cost, and the edit loop —
which is re-running `dev` — survives a bad save and shows a good one.
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
from typing import Callable

import pytest

from streamlib import cli

pytestmark = pytest.mark.requires_gpu

# Boot is process start + engine init + GPU context + socket bind.
NODE_READY_TIMEOUT_SECONDS = 90.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0
# Long enough that a per-frame failure or slowdown cannot hide inside it — and
# long enough to outlast a warm-up. A window that ends before steady state
# proves less than its length suggests: #1764 is a per-frame defect that takes
# ~280 delivered frames to appear at all, which the 6s this ran at could not
# have seen however many times it was run.
SCAFFOLD_OBSERVATION_WINDOW_SECONDS = 12.0
# Measured through the window below, effect in its own interpreter: 360 frames
# in 12.0s — 30fps, the source's full rate, so the helper hop (escalate acquire
# + surface-share checkout per frame) costs the demo no frames at all. The
# floor keeps a third of that, which is still well above the ~4fps a
# pathologically slow per-frame edit manages, so it fails on a regression and
# not on a slow machine.
MINIMUM_FRAMES_FOR_LIVE_VIDEO = 120

APP_WITH_ONE_NATIVE_SOURCE = '''\
from streamlib import TestPatternSource


def setup(rt):
    rt.add(TestPatternSource, config={"width": 320, "height": 180})
'''

# Enough helpers that a serialized spawn, or a per-child stall, separates from
# a parallel one by more than measurement noise.
HELPER_PLACED_PROCESSOR_COUNT = 6
# The MVP sentence gives a minute for install, scaffold and run. Booting is the
# only part of that this test can measure, so the budget is the half of the
# minute the other parts do not need — a ceiling the measurement must fit
# inside, not the measurement itself. Measured on the rig: 6 helpers go live
# 0.80s after launch, 16 in 1.03s, so spawn is parallel and the margin is wide
# on purpose. What this fails on is a regression that makes it serial.
MAXIMUM_SECONDS_FOR_EVERY_HELPER_TO_GO_LIVE = 30.0

FIRST_FRAME_REPORTER_MODULE = '''\
import os

from streamlib import input, log, processor  # noqa: A004


@processor
class ReportsItsProcessOnFirstFrame:
    """Announces its own process the first time a frame reaches it."""

    def __init__(self) -> None:
        self.announced = False

    @input(delivery_profile="latest")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        if self.announced or ctx.inputs.read("video_from_upstream") is None:
            return
        self.announced = True
        log.info(f"MARKER:LIVE {os.getpid()}")
'''

# The reporter module sits beside the entry file rather than in a package: that
# is the other import shape the child's `PYTHONPATH` has to resolve, and the
# scaffold suite already covers the packaged one.
APP_WITH_A_FLEET_OF_HELPER_PLACED_PROCESSORS = f'''\
from first_frame_reporter import ReportsItsProcessOnFirstFrame
from streamlib import TestPatternSource


def setup(rt):
    source = rt.add(TestPatternSource, config={{"width": 320, "height": 180}})
    for _ in range({HELPER_PLACED_PROCESSOR_COUNT}):
        reporter = rt.add(ReportsItsProcessOnFirstFrame)
        rt.connect(source.output("video"), reporter.input("video_from_upstream"))
'''

LIVE_HELPER_MARKER = re.compile(r"MARKER:LIVE (\d+)")


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
        self.launched_at = time.monotonic()

    def captured_output(self) -> str:
        """Everything the child wrote, for a test that asserts on its report."""
        assert self.output_file is not None, "this node was launched without capture"
        return self.output_file.read_text(errors="replace")

    def await_output_satisfying(
        self, satisfied: "Callable[[str], bool]", what: str, timeout: float
    ) -> float:
        """Wait until the captured output satisfies `satisfied`, and return the
        seconds since launch that took.

        Polled off the capture file rather than read off a pipe: the launcher
        writes to a file precisely because nothing drains the child while it
        runs, and a reader that stopped draining would wedge the node the test
        is waiting on.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if satisfied(self.captured_output()):
                return time.monotonic() - self.launched_at
            if self.process.poll() is not None:
                raise AssertionError(
                    f"the node exited before {what}; output was:\n"
                    f"{self.captured_output()}"
                )
            time.sleep(0.1)
        raise AssertionError(
            f"timed out after {timeout}s waiting for {what}; output was:\n"
            f"{self.captured_output()}"
        )

    def await_output_containing(self, awaited: str, timeout: float) -> float:
        return self.await_output_satisfying(
            lambda output: awaited in output, f"`{awaited}`", timeout
        )

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


def test_every_helper_interpreter_goes_live_inside_the_startup_budget(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """The N-child-interpreter startup budget the MVP minute has to pay.

    Every Python processor is its own child interpreter, so a graph's boot cost
    now grows with its processor count. What this pins is that the growth is
    parallel rather than serial, and that it is the *pipeline* that went live —
    each helper reports only once a frame has actually reached it, so a child
    that started but never received traffic does not count.

    The distinct-pid assertion is the placement half: N processors must be N
    processes, so a spawn path that quietly reused one would fail here instead
    of passing on the timing alone.
    """
    app_directory = tmp_path / "app"
    app_directory.mkdir()
    (app_directory / "app.py").write_text(APP_WITH_A_FLEET_OF_HELPER_PLACED_PROCESSORS)
    (app_directory / "first_frame_reporter.py").write_text(FIRST_FRAME_REPORTER_MODULE)

    node = launch_node("dev", app_directory, free_port(), capture_output=True)
    seconds_to_live = node.await_output_satisfying(
        lambda output: len(set(LIVE_HELPER_MARKER.findall(output)))
        >= HELPER_PLACED_PROCESSOR_COUNT,
        f"all {HELPER_PLACED_PROCESSOR_COUNT} helpers to report a frame",
        MAXIMUM_SECONDS_FOR_EVERY_HELPER_TO_GO_LIVE,
    )

    reporting_pids = set(LIVE_HELPER_MARKER.findall(node.captured_output()))
    assert len(reporting_pids) == HELPER_PLACED_PROCESSOR_COUNT, (
        f"{HELPER_PLACED_PROCESSOR_COUNT} processors reported from "
        f"{len(reporting_pids)} processes — every Python processor gets its own"
    )
    assert str(node.process.pid) not in reporting_pids, (
        "a processor reported from the app's own process"
    )
    assert seconds_to_live < MAXIMUM_SECONDS_FOR_EVERY_HELPER_TO_GO_LIVE

    node.interrupt()
    assert node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        f"a graph of {HELPER_PLACED_PROCESSOR_COUNT} helpers must still tear down cleanly"
    )


def edit_the_scaffolded_effect(app_directory: Path) -> None:
    """Make the edit the demo asks for, in the file `new` wrote.

    Applied to the scaffold's own source rather than overwriting it with a
    copy: a copy would keep passing after the scaffold changed underneath it,
    proving something about a module `new` no longer writes.
    """
    effect_module = app_directory / "processors" / "inverting_effect.py"
    scaffolded = effect_module.read_text()
    edited = (
        scaffolded.replace("    input,\n", "    input,\n    log,\n")
        .replace(
            '    @input(delivery_profile="latest")',
            '    announced = False\n\n    @input(delivery_profile="latest")',
        )
        .replace(
            '        ctx.outputs.write("video_to_downstream", bag)',
            "        if not self.announced:\n"
            "            self.announced = True\n"
            '            log.info("MARKER:EDITED_EFFECT")\n'
            '        ctx.outputs.write("video_to_downstream", bag)',
        )
    )
    assert (
        "    log,\n" in edited
        and "announced = False" in edited
        and "MARKER:EDITED_EFFECT" in edited
    ), "the scaffolded effect module no longer carries the anchors this edit needs"
    effect_module.write_text(edited)


def test_the_edit_loop_survives_a_bad_save_and_shows_a_good_one(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """The MVP edit loop, which is re-running `dev`.

    A save is a file write, and the node running when it lands has already
    imported what it needs — in the app process and in every helper. So a
    broken save must cost the running pipeline nothing, and a good one must
    show up on the next run. Both halves are asserted against the same app in
    sequence because the first is only meaningful if the second follows: a
    pipeline that survives a bad save by ignoring the file entirely would pass
    the first alone.
    """
    app_directory = tmp_path / "app"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=True)
    effect_module = app_directory / "processors" / "inverting_effect.py"
    last_good_effect_source = effect_module.read_text()

    surviving_node = launch_node("dev", app_directory, free_port(), capture_output=True)
    await_sole_registry_entry(isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS)

    time.sleep(SCAFFOLD_OBSERVATION_WINDOW_SECONDS / 2)
    effect_module.write_text("def process(self ctx:\n    this does not parse\n")
    time.sleep(SCAFFOLD_OBSERVATION_WINDOW_SECONDS / 2)

    surviving_node.interrupt()
    assert surviving_node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        "a bad save must not take the running node down"
    )
    survived_output = surviving_node.captured_output()
    assert "process() failed" not in survived_output, (
        f"the running effect must not have noticed the save; output was:\n{survived_output}"
    )
    frames_shown = re.search(r"DisplayWindow: stopped \((\d+) frames\)", survived_output)
    assert frames_shown, f"the window never reported a frame count:\n{survived_output}"
    assert int(frames_shown.group(1)) >= MINIMUM_FRAMES_FOR_LIVE_VIDEO, (
        f"the pipeline delivered only {frames_shown.group(1)} frames across a save "
        f"that never touched it"
    )

    effect_module.write_text(last_good_effect_source)
    edit_the_scaffolded_effect(app_directory)

    edited_node = launch_node("dev", app_directory, free_port(), capture_output=True)
    edited_node.await_output_containing("MARKER:EDITED_EFFECT", NODE_READY_TIMEOUT_SECONDS)

    edited_node.interrupt()
    assert edited_node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0


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

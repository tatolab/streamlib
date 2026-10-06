# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`streamlib run` / `dev` booting a real node, end to end.

What these lock is that a Python-launched app is a first-class node: its
stream's graph was loaded, it published a node-registry entry the observation
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
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Callable

import pytest
from app_under_test import ENGINE_READY_LOG_LINE

from streamlib import cli
from streamlib._control_plane_client import LocalApiSocket, call_tool
from streamlib._surface_image_exchange import (
    DEFAULT_SURFACE_ID_BAG_FIELD_NAME,
    _surface_id_in_bag,
    _tapped_bag_frames,
)

pytestmark = pytest.mark.requires_gpu

# Boot is process start + engine init + GPU context + socket bind.
NODE_READY_TIMEOUT_SECONDS = 90.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0
# Long enough that a per-frame failure or slowdown cannot hide inside it — and
# long enough to outlast a warm-up. A window that ends before steady state
# proves less than its length suggests: #1764 is a per-frame defect that needs
# ~280 delivered frames, nine seconds of them, before it appears at all.
SCAFFOLD_OBSERVATION_WINDOW_SECONDS = 12.0
# The save has to land on a node that is already running, not one still booting.
SECONDS_OF_LIVE_VIDEO_BEFORE_THE_BAD_SAVE_LANDS = SCAFFOLD_OBSERVATION_WINDOW_SECONDS / 2
# Measured through the window below, effect in its own interpreter: 360 frames
# in 12.0s — 30fps, the source's full rate, so the helper hop (escalate acquire
# + surface-share checkout per frame) costs the demo no frames at all. The
# floor keeps a third of that, which is still well above the ~4fps a
# pathologically slow per-frame edit manages, so it fails on a regression and
# not on a slow machine.
MINIMUM_FRAMES_FOR_LIVE_VIDEO = 120

STREAM_WITH_ONE_NATIVE_SOURCE = '''\
from streamlib import Stream, TestPatternSource, stream


@stream
def main(stream: Stream) -> None:
    stream.add(TestPatternSource, config={"width": 320, "height": 180})
'''

# A fleet rather than a pair, and few enough that the rig pays for it in
# seconds.
HELPER_PLACED_PROCESSOR_COUNT = 6
# The MVP sentence gives a minute for install, scaffold and run, and booting is
# the only part of that this test can measure — so the budget is the half of
# the minute the other parts do not need.
#
# It is a ceiling and nothing subtler. An app boot is ~520ms of GPU context
# init and a child interpreter adds ~56ms at the margin, so six helpers reach
# live traffic 0.6s after launch and even starting them one after another
# would land near 1s. What this catches is a helper that stopped starting, or
# a per-child cost that grew by an order of magnitude — not the difference
# between a parallel spawn and a serial one, which lives inside the noise at
# this count. The distinct-pid assertions are what pin placement.
MAXIMUM_SECONDS_FOR_EVERY_HELPER_TO_GO_LIVE = 30.0

FIRST_FRAME_REPORTER_MODULE = Path(__file__).parent / "first_frame_reporter.py"

# The reporter is copied beside the entry file rather than into a package: that
# is the other import shape the child's `PYTHONPATH` has to resolve, and the
# scaffold suite already covers the packaged one.
STREAM_WITH_HELPER_PLACED_PROCESSORS_TEMPLATE = '''\
from first_frame_reporter import ReportsItsProcessOnFirstFrame
from streamlib import Stream, TestPatternSource, stream


@stream
def main(stream: Stream) -> None:
    source = stream.add(TestPatternSource, config={"width": 320, "height": 180})
    for _ in range(%d):
        reporter = stream.add(ReportsItsProcessOnFirstFrame)
        stream.connect(source.output("video"), reporter.input("video_from_upstream"))
'''

LIVE_HELPER_MARKER = re.compile(r"MARKER:LIVE (\d+)")
SCAFFOLDED_METER_REPORT = re.compile(r"brightness\b.*\bmean=")
# The meter reports once a second, so the observation window holds about a
# dozen. Both bounds are loose on purpose: the floor catches a meter that
# stopped after its first frame, the ceiling one that reports per frame
# (~360 in the window), and neither is a claim about wake latency.
MINIMUM_METER_REPORTS = 5
MAXIMUM_METER_REPORTS = 2 * int(SCAFFOLD_OBSERVATION_WINDOW_SECONDS)
DISPLAY_WINDOW_FRAME_COUNT = re.compile(r"DisplayWindow: stopped \((\d+) frames\)")

# Enough tail to carry a traceback and the lines around it.
RECENT_OUTPUT_CHARACTERS = 4000

# A `tap` collects its sample server-side, so a verb can legitimately take a moment.
CLI_VERB_TIMEOUT_SECONDS = 60.0
# The id form races the pool: a frame tapped at 30fps can be recycled before
# its exchange lands, which is a `410` the caller answers by tapping again.
SURFACE_ID_EXCHANGE_ATTEMPTS = 10
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
LOCAL_API_SOCKET_FILE_MODE = 0o600


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

    def recent_output(self) -> str:
        """The tail of the capture, plus where to read the rest.

        Whole captures stopped being printable: a window long enough to outlast
        a warm-up is also long enough for #1764 to put most of a megabyte of
        iceoryx2 warnings between a failure and the line that explains it.
        """
        captured = self.captured_output()
        if len(captured) <= RECENT_OUTPUT_CHARACTERS:
            return captured
        return (
            f"[first {len(captured) - RECENT_OUTPUT_CHARACTERS} characters elided — "
            f"the whole capture is at {self.output_file}]\n"
            f"{captured[-RECENT_OUTPUT_CHARACTERS:]}"
        )

    def await_captured_output_satisfying(
        self,
        captured_output_satisfies: "Callable[[str], bool]",
        awaited_description: str,
        timeout: float,
    ) -> float:
        """Wait for the capture to satisfy the predicate; return the seconds
        since launch that took.

        Polled off the capture file rather than read off a pipe: the launcher
        writes to a file precisely because nothing drains the child while it
        runs, and a reader that stopped draining would wedge the node the test
        is waiting on.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if captured_output_satisfies(self.captured_output()):
                return time.monotonic() - self.launched_at
            if self.process.poll() is not None:
                raise AssertionError(
                    f"the node exited before {awaited_description}; output ended:\n"
                    f"{self.recent_output()}"
                )
            time.sleep(0.1)
        raise AssertionError(
            f"timed out after {timeout}s waiting for {awaited_description}; "
            f"output ended:\n{self.recent_output()}"
        )

    def await_captured_output_containing(self, awaited: str, timeout: float) -> float:
        return self.await_captured_output_satisfying(
            lambda captured: awaited in captured, f"`{awaited}`", timeout
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
        verb: str,
        app_directory: Path,
        capture_output: bool = False,
        extra_arguments: "tuple[str, ...]" = (),
        extra_environment: "dict[str, str] | None" = None,
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
                    *extra_arguments,
                ],
                stdout=output_sink if output_sink is not None else subprocess.DEVNULL,
                stderr=(
                    subprocess.STDOUT if output_sink is not None else subprocess.DEVNULL
                ),
                text=True,
                start_new_session=True,
                env={
                    **os.environ,
                    "XDG_RUNTIME_DIR": str(isolated_runtime_directory),
                    **(extra_environment or {}),
                },
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


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
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
    (app_directory / "stream.py").write_text(STREAM_WITH_ONE_NATIVE_SOURCE)
    runtime_directory = isolated_runtime_directory

    node = launch_node(verb, app_directory)
    entry = await_sole_registry_entry(runtime_directory, NODE_READY_TIMEOUT_SECONDS)

    assert entry["pid"] == node.process.pid, "the entry must name the hosting process"
    assert entry["schema_version"] == 3
    local_api_socket_path = Path(entry["local_api_socket_path"])
    assert local_api_socket_path == (
        runtime_directory / "streamlib" / f"local-api-{entry['runtime_id']}.sock"
    ), f"the local API socket sits in the runtime directory; got {local_api_socket_path}"
    assert_only_its_owner_can_open(local_api_socket_path)
    # The engine replaces every character an address chunk may not carry,
    # so a host whose own name carries one is compared against the same
    # substitution rather than against the raw `gethostname`.
    this_host = re.sub(r"[/*$#?]", "-", socket.gethostname())
    assert re.fullmatch(rf"{re.escape(this_host)}-app-[0-9a-z]{{4}}", entry["runtime_name"]), (
        "an unnamed runtime is named after this host, its app directory and that "
        f"directory's path; got {entry['runtime_name']}"
    )

    node.interrupt()
    assert node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        f"`streamlib {verb}` must exit cleanly on SIGINT"
    )
    assert registry_entry_paths(runtime_directory) == [], (
        "clean teardown must remove the node-registry entry"
    )
    assert not local_api_socket_path.exists(), "clean teardown must remove the local API socket"


def socket_inodes_held_by(pid: int) -> "set[str]":
    """The inode of every socket `pid` holds a descriptor on."""
    held_socket_inodes: "set[str]" = set()
    for descriptor in Path(f"/proc/{pid}/fd").iterdir():
        try:
            descriptor_target = os.readlink(descriptor)
        except OSError:
            # Closed between the listing and the read.
            continue
        socket_inode = re.fullmatch(r"socket:\[(\d+)\]", descriptor_target)
        if socket_inode is not None:
            held_socket_inodes.add(socket_inode.group(1))
    return held_socket_inodes


def listening_tcp_socket_inodes_in_the_network_namespace_of(pid: int) -> "set[str]":
    """The inode of every TCP socket in LISTEN, IPv4 and IPv6, that `pid` can see."""
    listening_tcp_socket_inodes: "set[str]" = set()
    for tcp_table_name in ("tcp", "tcp6"):
        tcp_table_path = Path(f"/proc/{pid}/net/{tcp_table_name}")
        if not tcp_table_path.exists():
            # A kernel booted without IPv6 has no `tcp6` table.
            continue
        # Columns: sl, local_address, rem_address, st, tx_queue:rx_queue,
        # tr:tm->when, retrnsmt, uid, timeout, inode. `st` 0A is TCP_LISTEN.
        for row in tcp_table_path.read_text().splitlines()[1:]:
            columns = row.split()
            if columns[3] == "0A":
                listening_tcp_socket_inodes.add(columns[9])
    return listening_tcp_socket_inodes


def assert_the_tcp_listener_scan_sees_the_listeners_this_process_holds() -> None:
    """Bind loopback TCP listeners here and require the scan to find each one."""
    tcp_listeners = [socket.create_server(("127.0.0.1", 0))]
    try:
        tcp_listeners.append(socket.create_server(("::1", 0), family=socket.AF_INET6))
    except OSError:
        # A host without IPv6 loopback has no `tcp6` row to check the scan against.
        pass
    try:
        this_pid = os.getpid()
        listening_tcp_socket_inodes_this_process_holds = socket_inodes_held_by(
            this_pid
        ) & listening_tcp_socket_inodes_in_the_network_namespace_of(this_pid)
        for tcp_listener in tcp_listeners:
            tcp_listener_inode = str(os.fstat(tcp_listener.fileno()).st_ino)
            assert tcp_listener_inode in listening_tcp_socket_inodes_this_process_holds, (
                f"the TCP scan must see the listener this test holds at "
                f"{tcp_listener.getsockname()} (inode {tcp_listener_inode}); it saw "
                f"{sorted(listening_tcp_socket_inodes_this_process_holds)}"
            )
    finally:
        for tcp_listener in tcp_listeners:
            tcp_listener.close()


def unix_socket_listener_inode_at(pid: int, unix_socket_path: str) -> "str | None":
    """The inode of the Unix socket listening at `unix_socket_path`, as `pid` sees it."""
    # Columns: Num, RefCount, Protocol, Flags, Type, St, Inode, Path. An accepted
    # connection carries the listener's path too; only a listener has Flags
    # 00010000 (__SO_ACCEPTCON).
    for row in Path(f"/proc/{pid}/net/unix").read_text().splitlines()[1:]:
        columns = row.split(maxsplit=7)
        if len(columns) == 8 and columns[7] == unix_socket_path and columns[3] == "00010000":
            return columns[6]
    return None


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_launched_node_listens_on_no_tcp_socket(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """Nothing on the network can reach a node's control API: the node holds no
    TCP socket in LISTEN, on any address, loopback included.

    The same scan, read off Linux's `/proc`, has to find the TCP listeners the
    test itself holds and the local API socket's listener, and the node has to
    answer over it, so an empty TCP answer is the node's own and not a scan
    that saw nothing.
    """
    app_directory = tmp_path / "app"
    app_directory.mkdir()
    (app_directory / "stream.py").write_text(STREAM_WITH_ONE_NATIVE_SOURCE)

    node = launch_node("run", app_directory, capture_output=True)
    entry = await_sole_registry_entry(isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS)
    node.await_captured_output_containing(ENGINE_READY_LOG_LINE, NODE_READY_TIMEOUT_SECONDS)
    local_api_socket_path = entry["local_api_socket_path"]
    graph = json.loads(call_tool(LocalApiSocket(local_api_socket_path), "graph", {}))
    assert graph["runtime_name"] == entry["runtime_name"]

    assert_the_tcp_listener_scan_sees_the_listeners_this_process_holds()
    node_pid = node.process.pid
    held_socket_inodes = socket_inodes_held_by(node_pid)
    local_api_listener_inode = unix_socket_listener_inode_at(node_pid, local_api_socket_path)
    assert local_api_listener_inode is not None, (
        f"no Unix socket listens at {local_api_socket_path} in /proc/{node_pid}/net/unix"
    )
    assert local_api_listener_inode in held_socket_inodes, (
        f"the node's descriptors must include its local API listener (inode "
        f"{local_api_listener_inode}); they hold sockets {sorted(held_socket_inodes)}"
    )
    listening_tcp_socket_inodes_the_node_holds = (
        held_socket_inodes & listening_tcp_socket_inodes_in_the_network_namespace_of(node_pid)
    )
    assert listening_tcp_socket_inodes_the_node_holds == set(), (
        f"the node holds TCP sockets in LISTEN (inodes "
        f"{sorted(listening_tcp_socket_inodes_the_node_holds)}); output ended:\n"
        f"{node.recent_output()}"
    )

    node.interrupt()
    assert node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0


def assert_only_its_owner_can_open(local_api_socket_path: Path) -> None:
    status = os.stat(local_api_socket_path)
    assert stat.S_ISSOCK(status.st_mode), f"{local_api_socket_path} is not a socket"
    assert stat.S_IMODE(status.st_mode) == LOCAL_API_SOCKET_FILE_MODE, (
        f"the local API socket must be {LOCAL_API_SOCKET_FILE_MODE:o}; "
        f"got {stat.S_IMODE(status.st_mode):o}"
    )
    assert status.st_uid == os.getuid()


def run_cli_verb(
    isolated_runtime_directory: Path, *arguments: str
) -> "subprocess.CompletedProcess[str]":
    """One `streamlib` verb in its own process, seeing the launched node's runtime directory."""
    return subprocess.run(
        [sys.executable, "-m", "streamlib.cli", *arguments],
        capture_output=True,
        text=True,
        timeout=CLI_VERB_TIMEOUT_SECONDS,
        env={**os.environ, "XDG_RUNTIME_DIR": str(isolated_runtime_directory)},
    )


def succeeded(completed: "subprocess.CompletedProcess[str]") -> str:
    """The verb's stdout, once it exited 0 — or a failure carrying what it said."""
    assert completed.returncode == 0, (
        f"`streamlib {' '.join(completed.args[3:])}` exited {completed.returncode}:\n"
        f"stdout: {completed.stdout}\nstderr: {completed.stderr}"
    )
    return completed.stdout


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_every_observation_verb_reaches_a_launched_node_through_its_local_api_socket(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """`nodes`, `graph`, `tap`, `logs` and both forms of `exchange`, driven the
    way a user drives them: a separate process, with only the registry to find
    the node by. The source is wired to a reader, since a channel is tappable
    only once a connect has wired its output."""
    app_directory = tmp_path / "app"
    write_app_with_helper_placed_processors(app_directory, 1)
    node = launch_node("run", app_directory, capture_output=True)
    entry = await_sole_registry_entry(isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS)
    node.await_captured_output_containing(ENGINE_READY_LOG_LINE, NODE_READY_TIMEOUT_SECONDS)
    runtime_name = entry["runtime_name"]
    local_api_socket_path = entry["local_api_socket_path"]

    listed = succeeded(run_cli_verb(isolated_runtime_directory, "nodes"))
    assert listed.splitlines()[0].split()[2] == "LOCAL_API_SOCKET", listed
    assert local_api_socket_path in listed

    graph = json.loads(
        succeeded(run_cli_verb(isolated_runtime_directory, "graph", "--node", runtime_name))
    )
    assert graph["runtime_name"] == runtime_name
    source_name = next(
        graph_node["name"]
        for graph_node in graph["nodes"]
        if graph_node["name"].startswith("testpatternsource")
    )
    channel = f"{runtime_name}/{source_name}/video"

    tapped = json.loads(
        succeeded(
            run_cli_verb(
                isolated_runtime_directory, "tap", channel, "--count", "2", "--node", runtime_name
            )
        )
    )
    assert tapped["received"] > 0, f"no bags reached the tap over the socket: {tapped}"

    succeeded(run_cli_verb(isolated_runtime_directory, "logs", "--node", runtime_name, "--count", "1"))

    channel_form_directory = tmp_path / "channel-form"
    channel_form_written = succeeded(
        run_cli_verb(
            isolated_runtime_directory,
            "exchange", "--channel", channel, "--count", "1",
            "--out", str(channel_form_directory), "--node", runtime_name,
        )
    ).split()
    assert len(channel_form_written) == 1, channel_form_written
    assert Path(channel_form_written[0]).read_bytes().startswith(PNG_SIGNATURE)

    id_form_directory = tmp_path / "id-form"
    id_form_attempts: "list[str]" = []
    for _ in range(SURFACE_ID_EXCHANGE_ATTEMPTS):
        frames = _tapped_bag_frames(LocalApiSocket(local_api_socket_path), channel, 1)
        published_surface_id = _surface_id_in_bag(
            frames[0].framed_bytes, channel, DEFAULT_SURFACE_ID_BAG_FIELD_NAME
        )
        assert published_surface_id is not None, f"{channel} published no surface id"
        exchanged = run_cli_verb(
            isolated_runtime_directory,
            "exchange", published_surface_id,
            "--out", str(id_form_directory), "--node", runtime_name,
        )
        if exchanged.returncode == 0:
            assert Path(exchanged.stdout.strip()).read_bytes().startswith(PNG_SIGNATURE)
            break
        id_form_attempts.append(exchanged.stderr.strip())
    else:
        raise AssertionError(
            f"the id form never exchanged a frame in {SURFACE_ID_EXCHANGE_ATTEMPTS} "
            f"attempts: {id_form_attempts}"
        )

    node.interrupt()
    assert node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0
    assert not Path(local_api_socket_path).exists()


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_second_runtime_with_a_live_runtimes_id_is_refused_naming_its_local_api_socket(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """The surface socket refuses a pinned duplicate before the local API is
    reached, so the local API's own refusal is driven with the surface socket
    out of the way: a live listener at the pinned id's local API path stands in
    for a second runtime. The refusal fails the api-server's start, which the
    engine logs; the node publishes no registry entry."""
    app_directory = tmp_path / "app"
    app_directory.mkdir()
    (app_directory / "stream.py").write_text(STREAM_WITH_ONE_NATIVE_SOURCE)
    pinned_runtime_id = f"Rpinned{os.getpid()}"
    local_api_socket_path = (
        isolated_runtime_directory / "streamlib" / f"local-api-{pinned_runtime_id}.sock"
    )
    local_api_socket_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    squatting_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    squatting_listener.bind(str(local_api_socket_path))
    squatting_listener.listen(1)
    try:
        node = launch_node(
            "run",
            app_directory,
            capture_output=True,
            extra_environment={"STREAMLIB_RUNTIME_ID": pinned_runtime_id},
        )
        node.await_captured_output_satisfying(
            lambda captured: str(local_api_socket_path) in captured
            and "already bound by a live process" in captured,
            "the local API's refusal naming its socket",
            NODE_READY_TIMEOUT_SECONDS,
        )
        assert registry_entry_paths(isolated_runtime_directory) == []
        node.interrupt()
        node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS)
        assert local_api_socket_path.exists(), "a refused bind must leave the live socket alone"
    finally:
        squatting_listener.close()


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_stale_local_api_socket_file_is_replaced(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    app_directory = tmp_path / "app"
    app_directory.mkdir()
    (app_directory / "stream.py").write_text(STREAM_WITH_ONE_NATIVE_SOURCE)
    pinned_runtime_id = f"Rstale{os.getpid()}"
    local_api_socket_path = (
        isolated_runtime_directory / "streamlib" / f"local-api-{pinned_runtime_id}.sock"
    )
    local_api_socket_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    crashed_runs_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    crashed_runs_listener.bind(str(local_api_socket_path))
    crashed_runs_listener.close()
    assert local_api_socket_path.exists(), "a closed listener leaves its file, as a crash does"

    node = launch_node(
        "run",
        app_directory,
        extra_environment={"STREAMLIB_RUNTIME_ID": pinned_runtime_id},
    )
    entry = await_sole_registry_entry(isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS)

    assert entry["local_api_socket_path"] == str(local_api_socket_path)
    assert_only_its_owner_can_open(local_api_socket_path)
    graph = json.loads(call_tool(LocalApiSocket(str(local_api_socket_path)), "graph", {}))
    assert graph["runtime_name"] == entry["runtime_name"]

    node.interrupt()
    assert node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0
    assert not local_api_socket_path.exists()


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_launched_app_takes_the_runtime_name_its_command_line_gave_it(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """`--runtime-name` is the name the registry publishes, verbatim."""
    app_directory = tmp_path / "app"
    app_directory.mkdir()
    (app_directory / "stream.py").write_text(STREAM_WITH_ONE_NATIVE_SOURCE)

    node = launch_node(
        "run",
        app_directory,
        extra_arguments=("--runtime-name", "desk rig"),
    )
    entry = await_sole_registry_entry(
        isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS
    )

    assert entry["runtime_name"] == "desk rig"

    node.interrupt()
    assert node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_node_launched_with_xdg_runtime_dir_unset_keeps_everything_live_in_the_per_user_fallback(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """A runtime starts anywhere with nothing set — a container, a CI runner —
    and `streamlib nodes` still finds it.

    The node carries a Python processor, so a frame reaching it proves parent
    and helper opened their nodes in one iceoryx2 domain. Discovery goes through
    the wheel's own registry reader, never a hand-built path, because the
    reader resolving exactly as the engine does is the contract. The launch
    tests above all set `XDG_RUNTIME_DIR`, so none of them reaches this arm.
    """
    from streamlib._node_registry import registry_directory, runtime_directory, scan_check_and_prune

    monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
    per_user_fallback = Path("/tmp") / f"streamlib-{os.getuid()}"

    def iceoryx2_node_details() -> "set[Path]":
        return set((per_user_fallback / "iox2" / "nodes").glob("*/*node.details"))

    iceoryx2_node_details_before = iceoryx2_node_details()
    app_directory = tmp_path / "app"
    write_app_with_helper_placed_processors(app_directory, 1)
    output_file = app_directory / "node-output.log"
    with open(output_file, "w", encoding="utf-8") as output_sink:
        process = subprocess.Popen(
            [
                sys.executable, "-m", "streamlib.cli", "run",
                "--dir", str(app_directory),
            ],
            stdout=output_sink,
            stderr=subprocess.STDOUT,
            text=True,
            start_new_session=True,
            env={**os.environ},
        )  # fmt: skip
    node = LaunchedNode(process, output_file)
    try:
        seconds_until_every_helper_reports(node, 1)

        assert runtime_directory() == per_user_fallback
        deadline = time.monotonic() + NODE_READY_TIMEOUT_SECONDS
        discovered = []
        while not discovered and time.monotonic() < deadline:
            discovered = [
                found for found in scan_check_and_prune()
                if found.entry.pid == process.pid and found.reachable
            ]  # fmt: skip
            time.sleep(0.2)
        assert discovered, (
            f"the reader never found the node in {registry_directory()}; output "
            f"ended:\n{node.recent_output()}"
        )
        runtime_id = discovered[0].entry.runtime_id

        entry_file = registry_directory() / f"{runtime_id}.json"
        assert entry_file.is_file()
        assert (per_user_fallback / f"surface-share-{runtime_id}.sock").exists()
        new_node_details = iceoryx2_node_details() - iceoryx2_node_details_before
        assert {details.name for details in new_node_details} == {f"sl{os.getuid()}_node.details"}
        assert len(new_node_details) == 2, (
            f"the parent and its helper must each open a node in the per-user fallback's "
            f"domain; found {sorted(map(str, new_node_details))}"
        )

        node.interrupt()
        assert node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
            f"the node must exit cleanly on SIGINT; output ended:\n{node.recent_output()}"
        )
        assert not entry_file.exists(), "clean teardown must remove the node-registry entry"
        assert all(found.entry.pid != process.pid for found in scan_check_and_prune())
    finally:
        node.kill_process_group()


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_native_block_added_without_config_reaches_a_running_graph(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """`stream.add(TestPatternSource)` with no `config` — the spelling the plan
    blesses for a block that needs no configuration.

    The config travels to the engine as JSON and every field of a built-in's
    config struct carries a serde default, so `{}` deserializes and `null` does
    not. The struct is built at graph compile, after `load` accepted the graph
    and every Python-side check passed, so only a running graph proves it.
    """
    app_directory = tmp_path / "app"
    app_directory.mkdir()
    (app_directory / "stream.py").write_text(
        "from streamlib import Stream, TestPatternSource, stream\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream: Stream) -> None:\n"
        "    stream.add(TestPatternSource)\n"
    )

    node = launch_node("run", app_directory)
    entry = await_sole_registry_entry(
        isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS
    )

    assert entry["pid"] == node.process.pid, (
        "the graph must compile and start with no config given"
    )


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_the_scaffolded_app_reaches_a_running_graph(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """What `streamlib new` writes must actually run, frame after frame.

    Run exactly as scaffolded — window included, which is why this is rig-only:
    `dev` compiles the scaffold's `@stream` and loads the graph it builds, so the
    graph the control plane renders carries the stream's name and the exposure
    it declared. A registry entry alone proves almost nothing here: it appears
    whether or not `process()` ever succeeds, so the assertions that carry this
    test are the ones on the child's own output. `process() failed` catches an
    effect that raises every frame; the delivered-frame count catches an effect
    that is correct but so slow the demo is a slideshow — which is what editing
    the write-combined mapping in place through a strided view produced (~4fps).
    The meter's line is the logic half of the first minute: a fan-out reader
    that never reports is a graph that shows the picture and drops the rest.
    """
    app_directory = tmp_path / "app"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=True)

    node = launch_node("dev", app_directory, capture_output=True)
    entry = await_sole_registry_entry(
        isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS
    )
    assert entry["pid"] == node.process.pid
    node.await_captured_output_containing(ENGINE_READY_LOG_LINE, NODE_READY_TIMEOUT_SECONDS)
    live_graph = json.loads(
        call_tool(LocalApiSocket(entry["local_api_socket_path"]), "graph", {})
    )
    assert live_graph["stream"] == "main", (
        f"the node must render the stream it was loaded as; graph was {live_graph}"
    )
    assert live_graph["exposed"] == [
        {"node": "invertingeffect", "port": "video_to_downstream"}
    ], f"the node must render the exposure the stream declared; graph was {live_graph}"
    assert {
        "testpatternsource",
        "invertingeffect",
        "brightnessmeter",
        "displaywindow",
    } <= {graph_node["name"] for graph_node in live_graph["nodes"]}

    # Long enough for the source to have driven many frames through the effect.
    time.sleep(SCAFFOLD_OBSERVATION_WINDOW_SECONDS)
    node.interrupt()
    node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS)

    assert_the_window_showed_live_video(node, "the app `streamlib new` writes")
    meter_reports = len(SCAFFOLDED_METER_REPORT.findall(node.captured_output()))
    assert meter_reports >= MINIMUM_METER_REPORTS, (
        f"the scaffolded meter logged a brightness {meter_reports} times in "
        f"{SCAFFOLD_OBSERVATION_WINDOW_SECONDS}s; output ended:\n{node.recent_output()}"
    )
    assert meter_reports <= MAXIMUM_METER_REPORTS, (
        f"the scaffolded meter logged a brightness {meter_reports} times in "
        f"{SCAFFOLD_OBSERVATION_WINDOW_SECONDS}s — that is per frame, not once a second"
    )


def test_a_scaffolded_app_with_a_cross_floor_finding_warns_and_starts_anyway(
    tmp_path: Path, launch_node
):
    """The cross-floor check informs; it never walls off a start.

    The finding sits in a function nothing calls, so the app needs neither
    cupy nor CUDA to run: the check reads source, and this proves a finding in
    it reaches the agent's stdout without costing the start.
    """
    app_directory = tmp_path / "app"
    cli.scaffold_new_app(app_directory, use_test_pattern_source=True)
    effect_module = app_directory / cli.SCAFFOLDED_EFFECT_MODULE_PATH
    effect_module.write_text(
        effect_module.read_text()
        + "\n\ndef never_called():\n"
        "    import cupy\n"
        "    import torch\n"
        '    return torch.device("cuda")\n'
    )

    node = launch_node("dev", app_directory, capture_output=True)
    node.await_captured_output_containing(ENGINE_READY_LOG_LINE, NODE_READY_TIMEOUT_SECONDS)
    node.interrupt()
    node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS)

    output = node.captured_output()
    assert f"{cli.SCAFFOLDED_EFFECT_MODULE_PATH}:" in output and "imports `cupy`" in output, (
        f"the warning block must name the cupy import; output ended:\n{node.recent_output()}"
    )
    assert "names the device 'cuda'" in output, (
        f"the warning block must name the device literal; output ended:\n{node.recent_output()}"
    )
    assert output.index("cross-floor check") < output.index(ENGINE_READY_LOG_LINE)


def assert_the_window_showed_live_video(node: LaunchedNode, what_ran: str) -> None:
    """Require an interrupted node to have shown live video, not a slideshow.

    The window reports what it actually put on screen, which is the honest
    measure — an effect can be correct and still leave the demo at roughly 4
    frames a second, which is what editing the write-combined mapping in place
    through a strided view produced.
    """
    output = node.captured_output()
    assert "process() failed" not in output, (
        f"{what_ran}: the effect raised on a live frame; output ended:\n"
        f"{node.recent_output()}"
    )
    frames_shown = DISPLAY_WINDOW_FRAME_COUNT.search(output)
    assert frames_shown, (
        f"{what_ran}: the window never reported a frame count; output ended:\n"
        f"{node.recent_output()}"
    )
    assert int(frames_shown.group(1)) >= MINIMUM_FRAMES_FOR_LIVE_VIDEO, (
        f"{what_ran} showed only {frames_shown.group(1)} frames in "
        f"{SCAFFOLD_OBSERVATION_WINDOW_SECONDS}s — that is a slideshow, not live video"
    )
    # The MVP minute is a terminal as well as a window. `DisplayWindow` drives
    # itself and polls its mailboxes, so it never drains the listener its
    # source notifies. iceoryx2 counts repeat notifications and sends no further
    # wakeup until the listener drains, so nothing fails; a transport queuing a
    # wakeup per send fills that listener and then fails delivery on every
    # frame, each failure a kilobytes-wide iceoryx2 dump — measured at 303
    # warnings and 2.4MB over this graph (#1764).
    #
    # The observation window is what makes this assertable: that saturation
    # takes ~280 notifications, and one notification rides every frame the SOURCE
    # publishes. `TestPatternSource` is `continuous(interval_ms = 33)` on its
    # own thread, so it publishes ~30/s whatever the display manages —
    # ~360 by 12s, comfortably past the onset. That rate is independent of
    # MINIMUM_FRAMES_FOR_LIVE_VIDEO above, which measures what the window drew;
    # a slow display shortens neither the source's output nor this margin.
    # Shorten the window below ~9.4s and this assertion stops discriminating.
    # The text iceoryx2 logs for a notify that did not reach a listener.
    undeliverable_notifications = output.count("Unable to send notification")
    assert undeliverable_notifications == 0, (
        f"{what_ran}: {undeliverable_notifications} undeliverable link notifications in "
        f"{SCAFFOLD_OBSERVATION_WINDOW_SECONDS}s — a notify failed to reach its listener; "
        f"output ended:\n{node.recent_output()}"
    )


def write_app_with_helper_placed_processors(
    app_directory: Path, helper_count: int
) -> None:
    """An app whose `stream.py` wires `helper_count` copies of the reporter to one native source."""
    app_directory.mkdir(parents=True, exist_ok=True)
    (app_directory / "stream.py").write_text(
        STREAM_WITH_HELPER_PLACED_PROCESSORS_TEMPLATE % helper_count
    )
    shutil.copy(FIRST_FRAME_REPORTER_MODULE, app_directory / FIRST_FRAME_REPORTER_MODULE.name)


def seconds_until_every_helper_reports(node: LaunchedNode, helper_count: int) -> float:
    """Seconds from launch until `helper_count` distinct processes each saw a frame.

    Waited for on a bound far above the budget rather than on the budget
    itself: a wait that expires exactly at the ceiling can only ever report a
    timeout, and what a blown budget should say is how long it actually took.
    """
    return node.await_captured_output_satisfying(
        lambda captured: len(set(LIVE_HELPER_MARKER.findall(captured))) >= helper_count,
        f"all {helper_count} helpers to report a frame",
        NODE_READY_TIMEOUT_SECONDS,
    )


def test_every_helper_interpreter_goes_live_inside_the_startup_budget(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """The N-child-interpreter startup budget the MVP minute has to pay.

    Every Python processor is its own child interpreter, so a graph's boot cost
    now grows with its processor count, and the sentence gives that growth a
    minute to disappear into. The budget is a flat ceiling on the whole fleet —
    see the constant for what a ceiling this generous can and cannot catch.

    What it measures is live traffic, not liveness: a helper reports only once
    a frame has reached it, so a child that started and received nothing does
    not count. The distinct-pid assertions are the placement half — N
    processors must be N processes, so a spawn path that quietly reused one
    fails here rather than passing on the timing alone.
    """
    fleet_app = tmp_path / "fleet"
    write_app_with_helper_placed_processors(fleet_app, HELPER_PLACED_PROCESSOR_COUNT)
    fleet_node = launch_node("dev", fleet_app, capture_output=True)
    seconds_for_every_helper = seconds_until_every_helper_reports(
        fleet_node, HELPER_PLACED_PROCESSOR_COUNT
    )

    reporting_pids = set(LIVE_HELPER_MARKER.findall(fleet_node.captured_output()))
    assert len(reporting_pids) == HELPER_PLACED_PROCESSOR_COUNT, (
        f"{HELPER_PLACED_PROCESSOR_COUNT} processors reported from "
        f"{len(reporting_pids)} processes — every Python processor gets its own"
    )
    assert str(fleet_node.process.pid) not in reporting_pids, (
        "a processor reported from the app's own process"
    )
    assert seconds_for_every_helper < MAXIMUM_SECONDS_FOR_EVERY_HELPER_TO_GO_LIVE, (
        f"{HELPER_PLACED_PROCESSOR_COUNT} helper interpreters took "
        f"{seconds_for_every_helper:.2f}s to reach live traffic — the minute does "
        f"not absorb that"
    )
    fleet_node.interrupt()
    assert fleet_node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        f"a graph of {HELPER_PLACED_PROCESSOR_COUNT} helpers must still tear down cleanly"
    )


def edit_the_scaffolded_effect(app_directory: Path) -> None:
    """Make the edit the demo asks for, in the file `new` wrote.

    Applied to the scaffold's own source rather than overwriting it with a
    copy: a copy would keep passing after the scaffold changed underneath it,
    proving something about a module `new` no longer writes.
    """
    effect_module = app_directory / cli.SCAFFOLDED_EFFECT_MODULE_PATH
    edited = effect_module.read_text()
    for anchor, replacement in (
        (
            "    input,  # noqa: A004 — streamlib's port decorator\n",
            "    input,  # noqa: A004 — streamlib's port decorator\n    log,\n",
        ),
        (
            '    @input(delivery_profile="newest")',
            '    announced = False\n\n    @input(delivery_profile="newest")',
        ),
        (
            "        ctx.outputs.write(\n",
            "        if not self.announced:\n"
            "            self.announced = True\n"
            '            log.info("MARKER:EDITED_EFFECT")\n'
            "        ctx.outputs.write(\n",
        ),
    ):
        # Named one at a time, and required to be unique: a scaffold that grew
        # a second copy of an anchor would take the edit twice, and a scaffold
        # that dropped one would otherwise fail somewhere downstream of here.
        assert edited.count(anchor) == 1, (
            f"the scaffolded effect module carries {edited.count(anchor)} copies of "
            f"the anchor {anchor!r} this edit needs exactly one of"
        )
        edited = edited.replace(anchor, replacement, 1)
    effect_module.write_text(edited)


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
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
    effect_module = app_directory / cli.SCAFFOLDED_EFFECT_MODULE_PATH
    last_good_effect_source = effect_module.read_text()

    surviving_node = launch_node("dev", app_directory, capture_output=True)
    await_sole_registry_entry(isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS)

    time.sleep(SECONDS_OF_LIVE_VIDEO_BEFORE_THE_BAD_SAVE_LANDS)
    effect_module.write_text("def process(self ctx:\n    this does not parse\n")
    time.sleep(
        SCAFFOLD_OBSERVATION_WINDOW_SECONDS - SECONDS_OF_LIVE_VIDEO_BEFORE_THE_BAD_SAVE_LANDS
    )

    surviving_node.interrupt()
    assert surviving_node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        "a bad save must not take the running node down"
    )
    assert_the_window_showed_live_video(
        surviving_node, "the pipeline running when the bad save landed"
    )

    effect_module.write_text(last_good_effect_source)
    edit_the_scaffolded_effect(app_directory)

    edited_node = launch_node("dev", app_directory, capture_output=True)
    edited_node.await_captured_output_containing(
        "MARKER:EDITED_EFFECT", NODE_READY_TIMEOUT_SECONDS
    )

    edited_node.interrupt()
    assert edited_node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0


def test_a_bad_config_is_reported_without_a_launcher_traceback(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """`load` refuses a config its built-in does not take, naming the node and the
    setting, before `run()`. It is still the app's problem, not a launcher crash."""
    app_directory = tmp_path / "app"
    app_directory.mkdir()
    (app_directory / "stream.py").write_text(
        "from streamlib import Stream, TestPatternSource, stream\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream: Stream) -> None:\n"
        '    stream.add(TestPatternSource, config={"width": "not a number"})\n'
    )

    node = launch_node("run", app_directory, capture_output=True)

    assert node.await_exit(NODE_READY_TIMEOUT_SECONDS) == 1
    output = node.captured_output()
    assert "Traceback (most recent call last)" not in output, (
        f"an engine-side failure must not arrive as a launcher traceback; output was:\n{output}"
    )
    assert "error:" in output
    assert "`tatolab.stream:TestPatternSource`" in output, output
    assert "width: invalid type" in output, output


def test_a_stream_function_that_raises_publishes_no_node(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """A graph that failed to build must not leave a node advertising itself."""
    app_directory = tmp_path / "app"
    app_directory.mkdir()
    (app_directory / "stream.py").write_text(
        "from streamlib import Stream, stream\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream: Stream) -> None:\n"
        "    raise ValueError('bad wiring')\n"
    )
    runtime_directory = isolated_runtime_directory

    node = launch_node("dev", app_directory)

    assert node.await_exit(NODE_READY_TIMEOUT_SECONDS) == 1, (
        "a raising stream function must exit non-zero"
    )
    assert registry_entry_paths(runtime_directory) == [], (
        "the control plane must be hosted only after the stream compiled and loaded"
    )

# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab run` / `dev` starting a stream on `tatolabd`, end to end.

`tatolab` compiles the project's stream in the project's own venv — here a
symlink to the suite venv, holding `tatolab-stream` and nothing of the runtime —
and starts `tatolabd` beside itself. The stream it hosts is a first-class node:
it publishes a registry entry naming `tatolabd`'s pid and an owner-only local
API socket, the observation verbs discover it, and a Ctrl-C to `tatolab` takes
both away. Starting the engine initializes a GPU context, so the module needs a
device.

The MVP minute is measured here too, with every processor in its own processor
interpreter: what `new` writes runs frame after frame, a graph of helpers goes
live inside the startup budget their interpreters cost, and the edit loop —
`dev` restarting `tatolabd` on a save — keeps the running stream through a bad
save and shows a good one.
"""

from __future__ import annotations

import json
import os
import re
import signal
import socket
import stat
import subprocess
import sys
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from conftest import PrivateRuntimeDirectories, StartedRuntimeProcesses
from runtime_process_under_test import (
    ENGINE_GRACEFUL_STOP_LOG_LINE,
    ENGINE_STARTED_LOG_LINE,
    RuntimeProcessUnderTest,
    registry_entry_paths_in,
)
from runtime_unit_under_test import (
    STREAM_ON_RUNTIME_SUITE_DIRECTORY,
    SUITE_VENV_INTERPRETER,
    RuntimeUnitUnderTest,
)
from tatolab.stream import TestPatternSource
from test_processor_interpreter_lend import assert_runs_in_a_process_of_its_own_beneath

pytestmark = pytest.mark.requires_gpu

# Boot is process start + compile + engine init + GPU context + socket bind.
NODE_READY_TIMEOUT_SECONDS = 90.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0
LOCAL_API_SOCKET_FILE_MODE = 0o600
NODE_REGISTRY_SCHEMA_VERSION = 3
# Long enough that a per-frame failure or slowdown cannot hide inside it — and
# long enough to outlast a warm-up. A window that ends before steady state
# proves less than its length suggests: #1764 is a per-frame defect that needs
# ~280 delivered frames, nine seconds of them, before it appears at all.
SCAFFOLD_OBSERVATION_WINDOW_SECONDS = 12.0
# The save has to land on a stream that is already running, not one still booting.
SECONDS_OF_LIVE_VIDEO_BEFORE_THE_BAD_SAVE_LANDS = SCAFFOLD_OBSERVATION_WINDOW_SECONDS / 2
# Measured through the window below, effect in its own interpreter: 360 frames
# in 12.0s — 30fps, the source's full rate, so the helper hop (escalate acquire
# + surface-share checkout per frame) costs the demo no frames at all. The
# floor keeps a third of that, which is still well above the ~4fps a
# pathologically slow per-frame edit manages, so it fails on a regression and
# not on a slow machine.
MINIMUM_FRAMES_FOR_LIVE_VIDEO = 120

STREAM_WITH_ONE_NATIVE_SOURCE = '''\
from tatolab.stream import StreamBuilder, TestPatternSource, stream


@stream
def main(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TestPatternSource, config={"width": 320, "height": 180})
'''

# A fleet rather than a pair, and few enough that the rig pays for it in
# seconds.
HELPER_PLACED_PROCESSOR_COUNT = 6
# The MVP sentence gives a minute for install, scaffold and run, and booting is
# the only part of that this test can measure — so the budget is the half of
# the minute the other parts do not need.
#
# It is a ceiling and nothing subtler. What this catches is a helper that
# stopped starting, or a per-child cost that grew by an order of magnitude —
# not the difference between a parallel spawn and a serial one, which lives
# inside the noise at this count. The distinct-pid assertions are what pin
# placement.
MAXIMUM_SECONDS_FOR_EVERY_HELPER_TO_GO_LIVE = 30.0

FIRST_FRAME_REPORTER_MODULE = STREAM_ON_RUNTIME_SUITE_DIRECTORY / "first_frame_reporter.py"

# The reporter is copied beside the entry file rather than into a package: that
# is the other import shape a processor interpreter has to resolve from its
# project, and the scaffold covers the packaged one.
STREAM_WITH_HELPER_PLACED_PROCESSORS_TEMPLATE = '''\
from first_frame_reporter import ReportsItsProcessOnFirstFrame
from tatolab.stream import StreamBuilder, TestPatternSource, stream


@stream
def main(stream_builder: StreamBuilder) -> None:
    source = stream_builder.add(TestPatternSource, config={"width": 320, "height": 180})
    for _ in range(%d):
        reporter = stream_builder.add(ReportsItsProcessOnFirstFrame)
        stream_builder.connect(source.output("video"), reporter.input("video_from_upstream"))
'''

LIVE_HELPER_MARKER_NAME = "LIVE"
SCAFFOLDED_METER_REPORT = re.compile(r"brightness\b.*\bmean=")
# The meter reports once a second, so the observation window holds about a
# dozen. Both bounds are loose on purpose: the floor catches a meter that
# stopped after its first frame, the ceiling one that reports per frame
# (~360 in the window), and neither is a claim about wake latency.
MINIMUM_METER_REPORTS = 5
MAXIMUM_METER_REPORTS = 2 * int(SCAFFOLD_OBSERVATION_WINDOW_SECONDS)
DISPLAY_WINDOW_FRAME_COUNT = re.compile(r"DisplayWindow: stopped \((\d+) frames\)")
SCAFFOLDED_EFFECT_MODULE_PATH = "nodes/inverting_effect.py"

# A `tap` collects its sample server-side, so a verb can legitimately take a moment.
OBSERVATION_VERB_TIMEOUT_SECONDS = 60.0
# The id form races the pool: a frame tapped at 30fps can be recycled before
# its exchange lands, which is a `410` the caller answers by tapping again.
SURFACE_ID_EXCHANGE_ATTEMPTS = 10
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"

# What `dev` says when a recompile fails and the running stream stays up.
DEV_KEPT_THE_RUNNING_STREAM = "tatolab dev: kept the running stream"
# What `dev` says when the first compile fails and nothing is running.
DEV_NO_STREAM_IS_RUNNING = "tatolab dev: no stream is running"
DEV_RESTARTING_THE_STREAM = "tatolab dev: restarting the stream"
# What `dev` says when a stream it stopped for a restart did not exit 0.
DEV_PREVIOUS_STREAM_EXITED_BADLY = "tatolab dev: the previous stream exited with"

# Run in a processor-interpreter-free Python with the lend on `PYTHONPATH`: the
# lend's own registry reader resolves the runtime directory exactly as the
# engine does, which is the contract the fallback test holds it to.
REGISTRY_READER_REPORT_SOURCE = """\
import json, sys
from tatolab.runtime._node_registry import registry_directory, runtime_directory, scan_check_and_prune

print(json.dumps({
    "runtime_directory": str(runtime_directory()),
    "registry_directory": str(registry_directory()),
    "discovered": [
        {"pid": found.entry.pid, "runtime_id": found.entry.runtime_id, "reachable": found.reachable}
        for found in scan_check_and_prune()
    ],
}))
"""

# The surface id the next bag on a channel publishes, read the way the
# `exchange` verb's channel form reads it.
PUBLISHED_SURFACE_ID_SOURCE = """\
import json, sys
from tatolab.runtime._control_plane_client import LocalApiSocket
from tatolab.runtime._surface_image_exchange import (
    DEFAULT_SURFACE_ID_BAG_FIELD_NAME,
    _surface_id_in_bag,
    _tapped_bag_frames,
)

local_api_socket_path, channel = sys.argv[1:3]
frames = _tapped_bag_frames(LocalApiSocket(local_api_socket_path), channel, 1)
print(json.dumps(_surface_id_in_bag(frames[0].framed_bytes, channel, DEFAULT_SURFACE_ID_BAG_FIELD_NAME)))
"""


def run_python_with_the_lend(
    runtime_unit: RuntimeUnitUnderTest,
    environment: "dict[str, str]",
    python_source: str,
    *script_arguments: str,
) -> Any:
    """Run `python_source` in the suite venv's interpreter with the lend leading
    `PYTHONPATH`, and return the JSON it printed."""
    existing_python_path = environment.get("PYTHONPATH")
    finished = subprocess.run(
        [str(SUITE_VENV_INTERPRETER), "-c", python_source, *script_arguments],
        capture_output=True,
        text=True,
        timeout=OBSERVATION_VERB_TIMEOUT_SECONDS,
        check=False,
        env={
            **environment,
            "PYTHONPATH": os.pathsep.join(
                [str(runtime_unit.lend_directory)]
                + ([existing_python_path] if existing_python_path else [])
            ),
        },
    )
    assert finished.returncode == 0, finished.stdout + finished.stderr
    return json.loads(finished.stdout)


def assert_only_its_owner_can_open(local_api_socket_path: Path) -> None:
    status = os.stat(local_api_socket_path)
    assert stat.S_ISSOCK(status.st_mode), f"{local_api_socket_path} is not a socket"
    assert stat.S_IMODE(status.st_mode) == LOCAL_API_SOCKET_FILE_MODE, (
        f"the local API socket must be {LOCAL_API_SOCKET_FILE_MODE:o}; "
        f"got {stat.S_IMODE(status.st_mode):o}"
    )
    assert status.st_uid == os.getuid()


def project_files_with_helper_placed_processors(helper_count: int) -> "dict[str, str]":
    """A project whose `stream.py` wires `helper_count` copies of the reporter to one native source."""
    return {
        "stream.py": STREAM_WITH_HELPER_PLACED_PROCESSORS_TEMPLATE % helper_count,
        FIRST_FRAME_REPORTER_MODULE.name: FIRST_FRAME_REPORTER_MODULE.read_text(encoding="utf-8"),
    }


def make_scaffolded_test_pattern_project(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
) -> Path:
    """`tatolab new --test-pattern` into a project whose `.venv` is the suite venv."""
    app_directory = make_tatolab_project()
    scaffolded = run_tatolab(
        "new", app_directory, "--test-pattern", working_directory=app_directory.parent
    )
    assert scaffolded.returncode == 0, scaffolded.stdout + scaffolded.stderr
    return app_directory


@pytest.mark.parametrize("verb", ["run", "dev"])
def test_a_launched_app_registers_as_a_node_and_tears_down(
    verb: str,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
    private_runtime_directories: PrivateRuntimeDirectories,
):
    """The MVP minute's observable half, for both verbs.

    `run` and `dev` share one launch path — `dev` only adds the restart on an
    edit — so a divergence in boot, registration or teardown between them is a
    defect in the path itself.
    """
    app_directory = make_tatolab_project({"stream.py": STREAM_WITH_ONE_NATIVE_SOURCE})
    streamlib_runtime_directory = private_runtime_directories.streamlib_runtime_directory

    tatolab = start_tatolab(verb, working_directory=app_directory)
    registry_entry_path = tatolab.registry_entry_path()
    entry = tatolab.registry_entry()
    running_graph = tatolab.local_api_client().await_every_node_running()
    tatolab.await_stderr_containing(ENGINE_STARTED_LOG_LINE)

    assert entry["pid"] != tatolab.pid, "the entry names tatolabd, not the tatolab that started it"
    assert entry["pid"] in tatolab.hosting_tatolabd_process_ids()
    assert entry["schema_version"] == NODE_REGISTRY_SCHEMA_VERSION
    local_api_socket_path = Path(entry["local_api_socket_path"])
    assert local_api_socket_path == (
        streamlib_runtime_directory / f"local-api-{entry['runtime_id']}.sock"
    ), f"the local API socket sits in the runtime directory; got {local_api_socket_path}"
    assert_only_its_owner_can_open(local_api_socket_path)
    assert running_graph["stream"] == "main"
    assert [node["type"] for node in running_graph["nodes"]].count(TestPatternSource.type) == 1, (
        running_graph["nodes"]
    )
    # The engine replaces every character an address chunk may not carry,
    # so a host whose own name carries one is compared against the same
    # substitution rather than against the raw `gethostname`.
    this_host = re.sub(r"[/*$#?]", "-", socket.gethostname())
    assert re.fullmatch(rf"{re.escape(this_host)}-app-[0-9a-z]{{4}}", entry["runtime_name"]), (
        "an unnamed runtime is named after this host, its app directory and that "
        f"directory's path; got {entry['runtime_name']}"
    )

    tatolab.interrupt()
    assert tatolab.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        f"`tatolab {verb}` must exit cleanly on SIGINT; standard error:\n{tatolab.recent_stderr()}"
    )
    if sys.platform == "linux":
        assert registry_entry_paths_in(streamlib_runtime_directory) == [], (
            "clean teardown must leave this test's private registry empty"
        )
    else:
        # The runtime directory is shared with every runtime on the machine.
        assert registry_entry_path not in registry_entry_paths_in(streamlib_runtime_directory), (
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
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
):
    """Nothing on the network can reach a node's control API: neither `tatolabd`
    nor the `tatolab` that started it holds a TCP socket in LISTEN, on any
    address, loopback included.

    The same scan, read off Linux's `/proc`, has to find the TCP listeners the
    test itself holds and the local API socket's listener, and the node has to
    answer over it, so an empty TCP answer is the node's own and not a scan
    that saw nothing.
    """
    app_directory = make_tatolab_project({"stream.py": STREAM_WITH_ONE_NATIVE_SOURCE})

    tatolab = start_tatolab("run", working_directory=app_directory)
    entry = tatolab.registry_entry(timeout=NODE_READY_TIMEOUT_SECONDS)
    tatolab.await_stderr_containing(ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS)
    local_api_socket_path = entry["local_api_socket_path"]
    graph = tatolab.local_api_client().call_tool("graph")
    assert graph["runtime_name"] == entry["runtime_name"]

    assert_the_tcp_listener_scan_sees_the_listeners_this_process_holds()
    tatolabd_pid = entry["pid"]
    held_socket_inodes = socket_inodes_held_by(tatolabd_pid)
    local_api_listener_inode = unix_socket_listener_inode_at(tatolabd_pid, local_api_socket_path)
    assert local_api_listener_inode is not None, (
        f"no Unix socket listens at {local_api_socket_path} in /proc/{tatolabd_pid}/net/unix"
    )
    assert local_api_listener_inode in held_socket_inodes, (
        f"tatolabd's descriptors must include its local API listener (inode "
        f"{local_api_listener_inode}); they hold sockets {sorted(held_socket_inodes)}"
    )
    for process_name, pid in (("tatolabd", tatolabd_pid), ("tatolab", tatolab.pid)):
        listening_tcp_socket_inodes_it_holds = socket_inodes_held_by(
            pid
        ) & listening_tcp_socket_inodes_in_the_network_namespace_of(pid)
        assert listening_tcp_socket_inodes_it_holds == set(), (
            f"{process_name} holds TCP sockets in LISTEN (inodes "
            f"{sorted(listening_tcp_socket_inodes_it_holds)}); standard error ended:\n"
            f"{tatolab.recent_stderr()}"
        )

    tatolab.interrupt()
    assert tatolab.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0


def succeeded(completed: "subprocess.CompletedProcess[str]") -> str:
    """The verb's stdout, once it exited 0 — or a failure carrying what it said."""
    assert completed.returncode == 0, (
        f"`{' '.join(map(str, completed.args[3:]))}` exited {completed.returncode}:\n"
        f"stdout: {completed.stdout}\nstderr: {completed.stderr}"
    )
    return completed.stdout


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_every_observation_verb_reaches_a_launched_node_through_its_local_api_socket(
    tmp_path: Path,
    runtime_unit: RuntimeUnitUnderTest,
    private_runtime_directories: PrivateRuntimeDirectories,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
    run_observation_verb: "Callable[..., subprocess.CompletedProcess[str]]",
):
    """`nodes`, `graph`, `tap`, `logs` and both forms of `exchange` — the lend's
    Python verbs until the native CLI takes them — driven the way a user drives
    them: a separate process, with only the registry to find the node by. The
    source is wired to a reader, since a channel is tappable only once a
    connect has wired its output."""
    app_directory = make_tatolab_project(project_files_with_helper_placed_processors(1))
    tatolab = start_tatolab("run", working_directory=app_directory)
    entry = tatolab.registry_entry(timeout=NODE_READY_TIMEOUT_SECONDS)
    tatolab.await_stderr_containing(ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS)
    runtime_name = entry["runtime_name"]
    local_api_socket_path = entry["local_api_socket_path"]

    listed = succeeded(run_observation_verb("nodes"))
    assert listed.splitlines()[0].split()[2] == "LOCAL_API_SOCKET", listed
    assert local_api_socket_path in listed

    graph = json.loads(succeeded(run_observation_verb("graph", "--node", runtime_name)))
    assert graph["runtime_name"] == runtime_name
    source_name = next(
        graph_node["name"]
        for graph_node in graph["nodes"]
        if graph_node["name"].startswith("testpatternsource")
    )
    channel = f"{runtime_name}/{source_name}/video"

    tapped = json.loads(
        succeeded(
            run_observation_verb(
                "tap", channel, "--count", "2", "--node", runtime_name,
                timeout=OBSERVATION_VERB_TIMEOUT_SECONDS,
            )
        )
    )  # fmt: skip
    assert tapped["received"] > 0, f"no bags reached the tap over the socket: {tapped}"

    succeeded(run_observation_verb("logs", "--node", runtime_name, "--count", "1"))

    channel_form_directory = tmp_path / "channel-form"
    channel_form_written = succeeded(
        run_observation_verb(
            "exchange", "--channel", channel, "--count", "1",
            "--out", str(channel_form_directory), "--node", runtime_name,
            timeout=OBSERVATION_VERB_TIMEOUT_SECONDS,
        )
    ).split()  # fmt: skip
    assert len(channel_form_written) == 1, channel_form_written
    assert Path(channel_form_written[0]).read_bytes().startswith(PNG_SIGNATURE)

    id_form_directory = tmp_path / "id-form"
    id_form_attempts: "list[str]" = []
    for _ in range(SURFACE_ID_EXCHANGE_ATTEMPTS):
        published_surface_id = run_python_with_the_lend(
            runtime_unit,
            private_runtime_directories.environment,
            PUBLISHED_SURFACE_ID_SOURCE,
            local_api_socket_path,
            channel,
        )
        assert published_surface_id is not None, f"{channel} published no surface id"
        exchanged = run_observation_verb(
            "exchange", published_surface_id,
            "--out", str(id_form_directory), "--node", runtime_name,
            timeout=OBSERVATION_VERB_TIMEOUT_SECONDS,
        )  # fmt: skip
        if exchanged.returncode == 0:
            assert Path(exchanged.stdout.strip()).read_bytes().startswith(PNG_SIGNATURE)
            break
        id_form_attempts.append(exchanged.stderr.strip())
    else:
        raise AssertionError(
            f"the id form never exchanged a frame in {SURFACE_ID_EXCHANGE_ATTEMPTS} "
            f"attempts: {id_form_attempts}"
        )

    tatolab.interrupt()
    assert tatolab.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0
    assert not Path(local_api_socket_path).exists()


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_second_runtime_with_a_live_runtimes_id_is_refused_naming_its_local_api_socket(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
    private_runtime_directories: PrivateRuntimeDirectories,
):
    """The surface socket refuses a pinned duplicate before the local API is
    reached, so the local API's own refusal is driven with the surface socket
    out of the way: a live listener at the pinned id's local API path stands in
    for a second runtime. The refusal fails the api-server's start, which the
    engine logs; the node publishes no registry entry."""
    app_directory = make_tatolab_project({"stream.py": STREAM_WITH_ONE_NATIVE_SOURCE})
    streamlib_runtime_directory = private_runtime_directories.streamlib_runtime_directory
    pinned_runtime_id = f"Rpinned{os.getpid()}"
    local_api_socket_path = streamlib_runtime_directory / f"local-api-{pinned_runtime_id}.sock"
    local_api_socket_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    squatting_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    squatting_listener.bind(str(local_api_socket_path))
    squatting_listener.listen(1)
    try:
        tatolab = start_tatolab(
            "run",
            working_directory=app_directory,
            extra_environment={"STREAMLIB_RUNTIME_ID": pinned_runtime_id},
        )
        refusal_line = tatolab.await_stderr_containing(
            "already bound by a live process", timeout=NODE_READY_TIMEOUT_SECONDS
        )
        assert str(local_api_socket_path) in refusal_line, (
            f"the local API's refusal must name its socket; it read:\n{refusal_line}"
        )
        assert registry_entry_paths_in(streamlib_runtime_directory) == []
        tatolab.interrupt()
        tatolab.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS)
        assert registry_entry_paths_in(streamlib_runtime_directory) == []
        assert local_api_socket_path.exists(), "a refused bind must leave the live socket alone"
    finally:
        squatting_listener.close()


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_stale_local_api_socket_file_is_replaced(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
    private_runtime_directories: PrivateRuntimeDirectories,
):
    app_directory = make_tatolab_project({"stream.py": STREAM_WITH_ONE_NATIVE_SOURCE})
    pinned_runtime_id = f"Rstale{os.getpid()}"
    local_api_socket_path = (
        private_runtime_directories.streamlib_runtime_directory
        / f"local-api-{pinned_runtime_id}.sock"
    )
    local_api_socket_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    crashed_runs_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    crashed_runs_listener.bind(str(local_api_socket_path))
    crashed_runs_listener.close()
    assert local_api_socket_path.exists(), "a closed listener leaves its file, as a crash does"

    tatolab = start_tatolab(
        "run",
        working_directory=app_directory,
        extra_environment={"STREAMLIB_RUNTIME_ID": pinned_runtime_id},
    )
    entry = tatolab.registry_entry(timeout=NODE_READY_TIMEOUT_SECONDS)

    assert entry["local_api_socket_path"] == str(local_api_socket_path)
    assert_only_its_owner_can_open(local_api_socket_path)
    graph = tatolab.local_api_client().call_tool("graph")
    assert graph["runtime_name"] == entry["runtime_name"]

    tatolab.interrupt()
    assert tatolab.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0
    assert not local_api_socket_path.exists()


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_launched_app_takes_the_runtime_name_its_command_line_gave_it(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
):
    """`--runtime-name` is the name the registry publishes, verbatim."""
    app_directory = make_tatolab_project({"stream.py": STREAM_WITH_ONE_NATIVE_SOURCE})

    tatolab = start_tatolab("run", "--runtime-name", "desk rig", working_directory=app_directory)
    entry = tatolab.registry_entry(timeout=NODE_READY_TIMEOUT_SECONDS)

    assert entry["runtime_name"] == "desk rig"

    tatolab.interrupt()
    assert tatolab.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0


def iceoryx2_node_details_in(iceoryx2_domain_root: Path) -> "set[Path]":
    return set((iceoryx2_domain_root / "nodes").glob("*/*node.details"))


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_node_launched_with_xdg_runtime_dir_unset_keeps_everything_live_in_the_per_user_fallback(
    runtime_unit: RuntimeUnitUnderTest,
    private_runtime_directories: PrivateRuntimeDirectories,
    started_runtime_processes: StartedRuntimeProcesses,
    make_tatolab_project: "Callable[..., Path]",
):
    """A runtime starts anywhere with nothing set — a container, a CI runner —
    and `nodes` still finds it.

    The node carries a Python processor, so a frame reaching it proves
    `tatolabd` and the processor interpreter opened their nodes in one iceoryx2
    domain. Discovery goes through the lend's own registry reader, never a
    hand-built path, because the reader resolving exactly as the engine does is
    the contract. The launch tests above all set `XDG_RUNTIME_DIR`, so none of
    them reaches this arm.
    """
    per_user_fallback = Path("/tmp") / f"streamlib-{os.getuid()}"
    environment_without_xdg_runtime_dir = {
        name: value
        for name, value in private_runtime_directories.environment.items()
        if name != "XDG_RUNTIME_DIR"
    }
    iceoryx2_node_details_before = iceoryx2_node_details_in(per_user_fallback / "iox2")
    app_directory = make_tatolab_project(project_files_with_helper_placed_processors(1))
    tatolab = RuntimeProcessUnderTest(
        subprocess.Popen(
            [str(runtime_unit.tatolab_executable), "run"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            errors="replace",
            bufsize=1,
            start_new_session=True,
            cwd=app_directory,
            env=environment_without_xdg_runtime_dir,
        ),
        command_description="tatolab run (XDG_RUNTIME_DIR unset)",
        streamlib_runtime_directory=per_user_fallback,
        hosting_tatolabd_is_a_child=True,
    )
    started_runtime_processes.started.append(tatolab)

    tatolab.await_marker(LIVE_HELPER_MARKER_NAME, timeout=NODE_READY_TIMEOUT_SECONDS)
    tatolabd_pid = tatolab.registry_entry()["pid"]

    deadline = time.monotonic() + NODE_READY_TIMEOUT_SECONDS
    registry_reader_report: "dict[str, Any]" = {}
    discovered: "list[dict[str, Any]]" = []
    while not discovered and time.monotonic() < deadline:
        registry_reader_report = run_python_with_the_lend(
            runtime_unit, environment_without_xdg_runtime_dir, REGISTRY_READER_REPORT_SOURCE
        )
        discovered = [
            found
            for found in registry_reader_report["discovered"]
            if found["pid"] == tatolabd_pid and found["reachable"]
        ]
        time.sleep(0.2)
    assert Path(registry_reader_report["runtime_directory"]) == per_user_fallback
    assert discovered, (
        f"the reader never found the node in {registry_reader_report['registry_directory']}; "
        f"standard error ended:\n{tatolab.recent_stderr()}"
    )
    runtime_id = discovered[0]["runtime_id"]

    entry_file = Path(registry_reader_report["registry_directory"]) / f"{runtime_id}.json"
    assert entry_file.is_file()
    assert (per_user_fallback / f"surface-share-{runtime_id}.sock").exists()
    new_node_details = (
        iceoryx2_node_details_in(per_user_fallback / "iox2") - iceoryx2_node_details_before
    )
    assert {details.name for details in new_node_details} == {f"sl{os.getuid()}_node.details"}
    assert len(new_node_details) == 2, (
        f"tatolabd and its processor interpreter must each open a node in the per-user "
        f"fallback's domain; found {sorted(map(str, new_node_details))}"
    )

    tatolab.interrupt()
    assert tatolab.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        f"the node must exit cleanly on SIGINT; standard error ended:\n{tatolab.recent_stderr()}"
    )
    assert not entry_file.exists(), "clean teardown must remove the node-registry entry"
    assert all(
        found["pid"] != tatolabd_pid
        for found in run_python_with_the_lend(
            runtime_unit, environment_without_xdg_runtime_dir, REGISTRY_READER_REPORT_SOURCE
        )["discovered"]
    )


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_native_block_added_without_config_reaches_a_running_graph(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
):
    """`stream_builder.add(TestPatternSource)` with no `config` — the spelling the plan
    blesses for a block that needs no configuration.

    The config travels to the engine as JSON and every field of a built-in's
    config struct carries a serde default, so `{}` deserializes and `null` does
    not. The struct is built at graph compile, after the load accepted the graph,
    so only a running graph proves it.
    """
    app_directory = make_tatolab_project(
        {
            "stream.py": (
                "from tatolab.stream import StreamBuilder, TestPatternSource, stream\n"
                "\n"
                "\n"
                "@stream\n"
                "def main(stream_builder: StreamBuilder) -> None:\n"
                "    stream_builder.add(TestPatternSource)\n"
            )
        }
    )

    tatolab = start_tatolab("run", working_directory=app_directory)
    entry = tatolab.registry_entry(timeout=NODE_READY_TIMEOUT_SECONDS)

    assert entry["pid"] in tatolab.hosting_tatolabd_process_ids(), (
        "the graph must compile and start with no config given"
    )
    running_graph = tatolab.local_api_client().await_every_node_running()
    assert [node["type"] for node in running_graph["nodes"]].count(TestPatternSource.type) == 1

    tatolab.interrupt()
    assert tatolab.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_the_scaffolded_app_reaches_a_running_graph(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
):
    """What `tatolab new` writes must actually run, frame after frame.

    Run exactly as scaffolded — window included, which is why this is rig-only:
    `dev` compiles the scaffold's `@stream` and `tatolabd` loads the graph it
    builds, so the graph the local API renders carries the stream's name and the
    exposure it declared. A registry entry alone proves almost nothing here: it
    appears whether or not `process()` ever succeeds, so the assertions that
    carry this test are the ones on the run's own output. `process() failed`
    catches an effect that raises every frame; the delivered-frame count catches
    an effect that is correct but so slow the demo is a slideshow. The meter's
    line is the logic half of the first minute: a fan-out reader that never
    reports is a graph that shows the picture and drops the rest.
    """
    app_directory = make_scaffolded_test_pattern_project(make_tatolab_project, run_tatolab)

    tatolab = start_tatolab("dev", working_directory=app_directory)
    entry = tatolab.registry_entry(timeout=NODE_READY_TIMEOUT_SECONDS)
    assert entry["pid"] in tatolab.hosting_tatolabd_process_ids()
    tatolab.await_stderr_containing(ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS)
    live_graph = tatolab.local_api_client().call_tool("graph")
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
    tatolab.interrupt()
    tatolab.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS)

    assert_the_window_showed_live_video(
        tatolab, tatolab.stderr_text, "the app `tatolab new` writes"
    )
    meter_reports = len(SCAFFOLDED_METER_REPORT.findall(tatolab.stderr_text))
    assert meter_reports >= MINIMUM_METER_REPORTS, (
        f"the scaffolded meter logged a brightness {meter_reports} times in "
        f"{SCAFFOLD_OBSERVATION_WINDOW_SECONDS}s; standard error ended:\n{tatolab.recent_stderr()}"
    )
    assert meter_reports <= MAXIMUM_METER_REPORTS, (
        f"the scaffolded meter logged a brightness {meter_reports} times in "
        f"{SCAFFOLD_OBSERVATION_WINDOW_SECONDS}s — that is per frame, not once a second"
    )


def test_a_scaffolded_app_with_a_cross_floor_finding_warns_and_starts_anyway(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
):
    """The cross-floor check informs; it never walls off a start.

    The finding sits in a function nothing calls, so the app needs neither
    cupy nor CUDA to run: the check reads source, and this proves a finding in
    it reaches the agent's terminal without costing the start.
    """
    app_directory = make_scaffolded_test_pattern_project(make_tatolab_project, run_tatolab)
    effect_module = app_directory / SCAFFOLDED_EFFECT_MODULE_PATH
    effect_module.write_text(
        effect_module.read_text()
        + "\n\ndef never_called():\n"
        "    import cupy\n"
        "    import torch\n"
        '    return torch.device("cuda")\n'
    )

    tatolab = start_tatolab("dev", working_directory=app_directory)
    tatolab.await_stderr_containing(ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS)
    tatolab.interrupt()
    tatolab.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS)

    output = tatolab.stderr_text
    assert f"{SCAFFOLDED_EFFECT_MODULE_PATH}:" in output and "imports `cupy`" in output, (
        f"the warning block must name the cupy import; standard error ended:\n"
        f"{tatolab.recent_stderr()}"
    )
    assert "names the device 'cuda'" in output, (
        f"the warning block must name the device literal; standard error ended:\n"
        f"{tatolab.recent_stderr()}"
    )
    assert output.index("cross-floor check") < output.index(ENGINE_STARTED_LOG_LINE)


def assert_the_window_showed_live_video(
    tatolab: RuntimeProcessUnderTest, output: str, what_ran: str
) -> None:
    """Require one stopped stream's output to show live video, not a slideshow.

    The window reports what it actually put on screen, which is the honest
    measure — an effect can be correct and still leave the demo at roughly 4
    frames a second, which is what editing the write-combined mapping in place
    through a strided view produced.
    """
    assert "process() failed" not in output, (
        f"{what_ran}: the effect raised on a live frame; standard error ended:\n"
        f"{tatolab.recent_stderr()}"
    )
    frames_shown = DISPLAY_WINDOW_FRAME_COUNT.search(output)
    assert frames_shown, (
        f"{what_ran}: the window never reported a frame count; standard error ended:\n"
        f"{tatolab.recent_stderr()}"
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
    # frame, each failure a kilobytes-wide iceoryx2 dump (#1764).
    #
    # The observation window is what makes this assertable: that saturation
    # takes ~280 notifications, and one notification rides every frame the SOURCE
    # publishes. `TestPatternSource` publishes ~30/s whatever the display
    # manages — ~360 by 12s, comfortably past the onset. Shorten the window
    # below ~9.4s and this assertion stops discriminating.
    # The text iceoryx2 logs for a notify that did not reach a listener.
    undeliverable_notifications = output.count("Unable to send notification")
    assert undeliverable_notifications == 0, (
        f"{what_ran}: {undeliverable_notifications} undeliverable link notifications in "
        f"{SCAFFOLD_OBSERVATION_WINDOW_SECONDS}s — a notify failed to reach its listener; "
        f"standard error ended:\n{tatolab.recent_stderr()}"
    )


def test_every_helper_interpreter_goes_live_inside_the_startup_budget(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
):
    """The N-processor-interpreter startup budget the MVP minute has to pay.

    Every Python processor is its own processor interpreter, so a graph's boot
    cost grows with its processor count, and the sentence gives that growth a
    minute to disappear into. The budget is a flat ceiling on the whole fleet —
    see the constant for what a ceiling this generous can and cannot catch.

    What it measures is live traffic, not liveness: a helper reports only once
    a frame has reached it, so an interpreter that started and received nothing
    does not count. The distinct-pid assertions are the placement half — N
    processors must be N processes beneath `tatolabd`, so a spawn path that
    quietly reused one fails here rather than passing on the timing alone.
    """
    fleet_app = make_tatolab_project(
        project_files_with_helper_placed_processors(HELPER_PLACED_PROCESSOR_COUNT),
        directory_name="fleet",
    )
    launched_at = time.monotonic()
    fleet = start_tatolab("dev", working_directory=fleet_app)
    # Waited for on a bound far above the budget rather than on the budget
    # itself: a wait that expires exactly at the ceiling can only ever report a
    # timeout, and what a blown budget should say is how long it actually took.
    fleet.await_marker(
        LIVE_HELPER_MARKER_NAME,
        occurrence=HELPER_PLACED_PROCESSOR_COUNT,
        timeout=NODE_READY_TIMEOUT_SECONDS,
    )
    seconds_for_every_helper = time.monotonic() - launched_at

    reporting_pids = set(fleet.marker_payloads(LIVE_HELPER_MARKER_NAME))
    assert len(reporting_pids) == HELPER_PLACED_PROCESSOR_COUNT, (
        f"{HELPER_PLACED_PROCESSOR_COUNT} processors reported from "
        f"{len(reporting_pids)} processes — every Python processor gets its own"
    )
    tatolabd_pid = fleet.registry_entry()["pid"]
    assert fleet.pid not in reporting_pids, "a processor reported from tatolab's own process"
    for reporting_pid in reporting_pids:
        assert_runs_in_a_process_of_its_own_beneath(reporting_pid, tatolabd_pid)
    assert seconds_for_every_helper < MAXIMUM_SECONDS_FOR_EVERY_HELPER_TO_GO_LIVE, (
        f"{HELPER_PLACED_PROCESSOR_COUNT} processor interpreters took "
        f"{seconds_for_every_helper:.2f}s to reach live traffic — the minute does "
        f"not absorb that"
    )
    fleet.interrupt()
    assert fleet.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        f"a graph of {HELPER_PLACED_PROCESSOR_COUNT} helpers must still tear down cleanly"
    )


def the_scaffolded_effect_edited(scaffolded_effect_source: str) -> str:
    """The edit the demo asks for, made to the source `new` wrote.

    Applied to the scaffold's own source rather than replacing it with a copy:
    a copy would keep passing after the scaffold changed underneath it, proving
    something about a module `new` no longer writes.
    """
    edited = scaffolded_effect_source
    for anchor, replacement in (
        (
            "    VideoFrame,\n",
            "    VideoFrame,\n    log,\n",
        ),
        (
            '    @node.input(delivery_profile="newest")',
            '    announced = False\n\n    @node.input(delivery_profile="newest")',
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
    return edited


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_the_edit_loop_survives_a_bad_save_and_shows_a_good_one(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
):
    """The MVP edit loop: `dev` recompiling on a save and restarting `tatolabd`.

    A save is a file write. A broken one fails the recompile, so `dev` keeps the
    stream already running — the same `tatolabd`, still showing live video —
    and a good one restarts the stream on the edited code. Both halves are
    asserted against the same `dev` in sequence because the first is only
    meaningful if the second follows: a loop that survives a bad save by
    ignoring the file entirely would pass the first alone.
    """
    app_directory = make_scaffolded_test_pattern_project(make_tatolab_project, run_tatolab)
    effect_module = app_directory / SCAFFOLDED_EFFECT_MODULE_PATH
    last_good_effect_source = effect_module.read_text()

    tatolab = start_tatolab("dev", working_directory=app_directory)
    tatolabd_pid_before_the_bad_save = tatolab.registry_entry(timeout=NODE_READY_TIMEOUT_SECONDS)[
        "pid"
    ]
    tatolab.await_stderr_containing(ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS)

    time.sleep(SECONDS_OF_LIVE_VIDEO_BEFORE_THE_BAD_SAVE_LANDS)
    effect_module.write_text("def process(self ctx:\n    this does not parse\n")
    tatolab.await_stderr_containing(DEV_KEPT_THE_RUNNING_STREAM, timeout=NODE_READY_TIMEOUT_SECONDS)
    time.sleep(
        SCAFFOLD_OBSERVATION_WINDOW_SECONDS - SECONDS_OF_LIVE_VIDEO_BEFORE_THE_BAD_SAVE_LANDS
    )
    assert tatolab.process.poll() is None, (
        f"a bad save must not take `dev` down; standard error ended:\n{tatolab.recent_stderr()}"
    )
    assert tatolab.registry_entry()["pid"] == tatolabd_pid_before_the_bad_save, (
        "a bad save must leave the running tatolabd in place"
    )
    assert DISPLAY_WINDOW_FRAME_COUNT.search(tatolab.stderr_text) is None, (
        "the window stopped although the bad save was to keep the stream running"
    )

    effect_module.write_text(the_scaffolded_effect_edited(last_good_effect_source))
    tatolab.await_stderr_containing(DEV_RESTARTING_THE_STREAM, timeout=NODE_READY_TIMEOUT_SECONDS)
    the_first_streams_stop = tatolab.await_stderr_containing(
        "DisplayWindow: stopped", timeout=CLEAN_EXIT_TIMEOUT_SECONDS
    )
    first_stream_output = tatolab.stderr_text.split(the_first_streams_stop, 1)[0] + (
        the_first_streams_stop
    )
    assert_the_window_showed_live_video(
        tatolab, first_stream_output, "the stream running when the bad save landed"
    )

    tatolab.await_marker("EDITED_EFFECT", timeout=NODE_READY_TIMEOUT_SECONDS)
    assert DEV_PREVIOUS_STREAM_EXITED_BADLY not in tatolab.stderr_text, (
        "a bad save must not take the running node down: the stream that survived "
        f"it exited unclean on the restart; standard error ended:\n{tatolab.recent_stderr()}"
    )
    tatolab.await_stderr_containing(
        ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS, occurrence=2
    )
    stderr_lines = list(tatolab.stderr_lines)
    engine_started_line_indices = [
        line_index
        for line_index, stderr_line in enumerate(stderr_lines)
        if ENGINE_STARTED_LOG_LINE in stderr_line
    ]
    lines_before_the_second_stream_started = stderr_lines[: engine_started_line_indices[1]]
    assert any(
        ENGINE_GRACEFUL_STOP_LOG_LINE in stderr_line
        for stderr_line in lines_before_the_second_stream_started
    ), (
        "the stream that survived the bad save did not shut down gracefully before "
        "the restart started the edited one"
    )
    tatolabd_pid_after_the_good_save = tatolab.registry_entry()["pid"]
    assert tatolabd_pid_after_the_good_save != tatolabd_pid_before_the_bad_save, (
        "a good save restarts the stream on a tatolabd of its own"
    )

    tatolab.interrupt()
    assert tatolab.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, tatolab.recent_stderr()


def test_a_bad_config_is_reported_without_a_launcher_traceback(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
):
    """`tatolabd`'s load refuses a config its built-in does not take, naming the
    node and the setting, before the engine starts. It is still the app's
    problem, not a launcher crash."""
    app_directory = make_tatolab_project(
        {
            "stream.py": (
                "from tatolab.stream import StreamBuilder, TestPatternSource, stream\n"
                "\n"
                "\n"
                "@stream\n"
                "def main(stream_builder: StreamBuilder) -> None:\n"
                '    stream_builder.add(TestPatternSource, config={"width": "not a number"})\n'
            )
        }
    )

    tatolab = start_tatolab("run", working_directory=app_directory)

    assert tatolab.await_exit(timeout=NODE_READY_TIMEOUT_SECONDS) == 1
    output = tatolab.stderr_text
    assert "Traceback (most recent call last)" not in output, (
        f"an engine-side failure must not arrive as a launcher traceback; output was:\n{output}"
    )
    refusal = tatolab.refusal()
    assert refusal is not None, f"tatolabd must end on its refusal; output was:\n{output}"
    assert "`tatolab.stream:TestPatternSource`" in refusal, refusal
    assert "width: invalid type" in refusal, refusal
    assert ENGINE_STARTED_LOG_LINE not in output


@pytest.mark.parametrize("verb", ["run", "dev"])
def test_a_stream_function_that_raises_publishes_no_node(
    verb: str,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
    private_runtime_directories: PrivateRuntimeDirectories,
):
    """A graph that failed to build must not leave a node advertising itself.

    `run` ends with the compile's failure; `dev` reports it and waits for the
    save that fixes it, with no stream running, until a Ctrl-C ends it.
    """
    app_directory = make_tatolab_project(
        {
            "stream.py": (
                "from tatolab.stream import StreamBuilder, stream\n"
                "\n"
                "\n"
                "@stream\n"
                "def main(stream_builder: StreamBuilder) -> None:\n"
                "    raise ValueError('bad wiring')\n"
            )
        }
    )
    streamlib_runtime_directory = private_runtime_directories.streamlib_runtime_directory

    tatolab = start_tatolab(verb, working_directory=app_directory)

    if verb == "run":
        assert tatolab.await_exit(timeout=NODE_READY_TIMEOUT_SECONDS) == 1, (
            f"a raising stream function must exit non-zero; standard error:\n"
            f"{tatolab.recent_stderr()}"
        )
    else:
        tatolab.await_stderr_containing(DEV_NO_STREAM_IS_RUNNING, timeout=NODE_READY_TIMEOUT_SECONDS)
        assert tatolab.hosting_tatolabd_process_ids() == set(), "dev started a tatolabd anyway"
        tatolab.interrupt()
        assert tatolab.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 128 + signal.SIGINT
    assert "ValueError: bad wiring" in tatolab.stderr_text, tatolab.recent_stderr()
    assert registry_entry_paths_in(streamlib_runtime_directory) == [], (
        "the control plane must be hosted only after the stream compiled and loaded"
    )

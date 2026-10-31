# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab run` / `dev` loading a project's stream into the machine's runtime, end to end.

The runtime compiles the project's stream in the project's own venv — here a
symlink to the suite venv, holding `tatolab-stream` and nothing of the runtime —
loads it and records it: the local API names it by its stream id, every
observation verb reaches it through the one socket, and it runs on once the
`tatolab` that loaded it has returned or been stopped, until `tatolab stop`.
Starting a stream initializes a GPU context, so the module needs a device.

The MVP minute is measured here too, with every processor in its own processor
interpreter: what `new` writes runs frame after frame, a graph of helpers goes
live inside the startup budget their interpreters cost, and the edit loop —
`dev` loading the stream again on each save — reports a bad save and loads the
next good one, on the same runtime. The scaffold opens no window, so live video
is the count of frames its effect wrote, read off `graph`, and the tests that
measure it run on the software device.
"""

from __future__ import annotations

import json
import os
import re
import socket
import stat
import subprocess
import sys
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from conftest import (
    TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE,
    TatolabdUnderTest,
    TatolabRunOrDevOnTheRuntime,
    a_tcp_port_free_on_loopback,
)
from runtime_process_under_test import ENGINE_GRACEFUL_STOP_LOG_LINE, ENGINE_STARTED_LOG_LINE
from runtime_unit_under_test import STREAM_ON_RUNTIME_SUITE_DIRECTORY
from stream_runs_on_tatolabd import TatolabRunOfAProject, stream_id_of_the_location
from tatolab.stream import TestPatternSource
from test_processor_interpreter_lend import assert_runs_in_a_process_of_its_own_beneath

pytestmark = pytest.mark.requires_gpu

# A load is the project's compile + the describe of its Python types + the
# engine's GPU context on the runtime's first start.
NODE_READY_TIMEOUT_SECONDS = 90.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0
LOCAL_API_SOCKET_FILE_MODE = 0o600
# Long enough that a per-frame failure or slowdown cannot hide inside it — and
# long enough to outlast a warm-up. An observation that ends before steady
# state proves less than its length suggests: a per-frame defect can need ~280
# delivered frames, nine seconds of them, before it appears at all.
SCAFFOLD_OBSERVATION_WINDOW_SECONDS = 12.0
# The save has to land on a stream that is already running, not one still booting.
SECONDS_OF_LIVE_VIDEO_BEFORE_THE_BAD_SAVE_LANDS = SCAFFOLD_OBSERVATION_WINDOW_SECONDS / 2
# Measured as the frames the effect wrote, effect in its own interpreter: 360
# frames in 12.0s — 30fps, the source's full rate, so the helper hop (escalate
# acquire + surface-share checkout per frame) costs the demo no frames at all.
# The floor keeps a third of that, which is still well above the ~4fps a
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
# The MVP sentence gives a minute for install, scaffold and run, and loading is
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
SCAFFOLDED_EFFECT_MODULE_PATH = "nodes/halftone_lens_effect.py"
SCAFFOLDED_EFFECT_NODE_NAME = "halftonelenseffect"
SCAFFOLDED_ENCODER_NODE_NAME = "h264encoder"
SCAFFOLDED_ENCODER_OUTPUT_PORT = "encoded_video"
# What the scaffold's encoder logs on a device with no H.264 encoder, the
# software device among them: a named refusal, while the stream stays loaded.
SCAFFOLDED_ENCODER_REFUSAL = "H264Encoder: failed to mint the encoder session"
# The encoder's own count, logged at teardown. `graph` counts a port's writes
# only once something reads it, and nothing subscribes to the track here.
SCAFFOLDED_ENCODER_FRAMES_ENCODED_AT_TEARDOWN = re.compile(
    r"H264Encoder: teardown .*frames_encoded=(\d+)"
)
SCAFFOLDED_EFFECT_OUTPUT_PORT = "video_to_downstream"

# How long a loaded stream's source has to write its first bag on `video`.
FIRST_BAG_WRITTEN_TIMEOUT_SECONDS = 30.0

#: The one stream function every project here defines, as its location names it.
STREAM_FUNCTION_OF_EVERY_PROJECT = "stream:main"


def stream_id_of_the_project(app_directory: Path) -> str:
    """The stream id the runtime keys and addresses the project's one stream by."""
    return stream_id_of_the_location(app_directory.resolve(), STREAM_FUNCTION_OF_EVERY_PROJECT)


def the_stream_stopped_log_line(stream_id: str) -> str:
    """The engine's line once the stream's unload has finished."""
    return f"[stop] The stream `{stream_id}` stopped"


#: What `dev` says as a save makes it queue another load of the project.
DEV_QUEUING_ANOTHER_LOAD_AFTER_A_SAVE = "tatolab dev: a saved change — queuing another load of the project"

#: How often a wait on what `dev` printed of the project's log looks again.
DEV_OUTPUT_POLL_INTERVAL_SECONDS = 0.1


def await_dev_printed(dev: TatolabRunOrDevOnTheRuntime, text: str, *, timeout: float) -> None:
    """Wait until `dev`'s standard output — the project's log it follows — holds `text`."""
    deadline = time.monotonic() + timeout
    while text not in dev.stdout_text:
        assert time.monotonic() < deadline, (
            f"`dev` printed no record holding {text!r} within {timeout}s; it printed:\n"
            f"{dev.stdout_text[-4000:]}"
        )
        time.sleep(DEV_OUTPUT_POLL_INTERVAL_SECONDS)


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


def make_scaffolded_sample_stream_project(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
) -> Path:
    """`tatolab new` into a project whose `.venv` is the suite venv."""
    app_directory = make_tatolab_project()
    scaffolded = run_tatolab("new", app_directory, working_directory=app_directory.parent)
    assert scaffolded.returncode == 0, scaffolded.stdout + scaffolded.stderr
    return app_directory


def load_the_projects_stream(
    tatolabd: TatolabdUnderTest, app_directory: Path, verb: str = "run"
) -> TatolabRunOrDevOnTheRuntime:
    """`tatolab <verb>` from `app_directory`, started on `tatolabd`."""
    return tatolabd.run_stream(TatolabRunOfAProject(working_directory=app_directory), verb=verb)


@pytest.mark.parametrize("verb", ["run", "dev"])
def test_a_loaded_stream_runs_on_past_the_verb_that_loaded_it_until_tatolab_stop(
    verb: str,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The MVP minute's observable half, for both verbs.

    `run` returns once the load is accepted and `dev` is ended by a Ctrl-C;
    neither ends the stream, which the runtime records and runs on until
    `tatolab stop` — so a divergence in load, listing or persistence between
    them is a defect in the load path itself.
    """
    app_directory = make_tatolab_project({"stream.py": STREAM_WITH_ONE_NATIVE_SOURCE})
    stream_id = stream_id_of_the_project(app_directory)
    tatolabd = start_tatolabd()

    stream_run = load_the_projects_stream(tatolabd, app_directory, verb)
    loaded = stream_run.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    local_api = tatolabd.local_api_client()
    running_graph = local_api.await_every_node_running(stream=stream_id)
    listed_streams = local_api.list_streams()

    assert loaded["stream_id"] == stream_id
    assert loaded["display_name"] == "main"
    assert loaded["stream_function"] == STREAM_FUNCTION_OF_EVERY_PROJECT
    assert Path(loaded["project_directory"]) == app_directory.resolve()
    listed_running = {
        "stream": stream_id,
        "display_name": "main",
        "location": {
            "project_directory": str(app_directory.resolve()),
            "stream_function": STREAM_FUNCTION_OF_EVERY_PROJECT,
        },
        "state": "running",
        "node_count": 1,
        "failed_because": None,
        "action": None,
        "step": None,
    }
    assert listed_streams == [listed_running]
    assert running_graph["stream"] == stream_id
    assert [node["type"] for node in running_graph["nodes"]].count(TestPatternSource.type) == 1, (
        running_graph["nodes"]
    )
    assert_only_its_owner_can_open(tatolabd.local_api_socket_path)

    if verb == "dev":
        stream_run.interrupt()
    assert stream_run.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        f"`tatolab {verb}` must exit cleanly; standard error:\n{stream_run.recent_stderr()}"
    )
    assert local_api.list_streams() == [listed_running], (
        f"the stream runs on once `tatolab {verb}` has ended"
    )
    assert the_stream_stopped_log_line(stream_id) not in tatolabd.stderr_text

    assert stream_run.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0
    tatolabd.await_stderr_containing(
        the_stream_stopped_log_line(stream_id), timeout=CLEAN_EXIT_TIMEOUT_SECONDS
    )
    assert local_api.list_streams() == [
        {**listed_running, "state": "stopped", "node_count": None}
    ], "`tatolab stop` unloads the stream and records it stopped"
    assert tatolabd.process.poll() is None, "the runtime outlives the stream it unloaded"


def socket_inodes_held_by(pid: int) -> "set[str]":
    """The inode of every socket `pid` holds a descriptor on, off Linux's `/proc`."""
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


def tcp_table_address_as_the_scan_names_it(tcp_table_address: str) -> str:
    """A `/proc/net/tcp{,6}` local address — hex host in the kernel's byte order,
    a colon, hex port — as `host:port`, or `[host]:port` for IPv6."""
    hex_host, hex_port = tcp_table_address.split(":")
    port = int(hex_port, 16)
    host_bytes = b"".join(
        bytes.fromhex(hex_host[word : word + 8])[::-1] for word in range(0, len(hex_host), 8)
    )
    if len(host_bytes) == 4:
        return f"{socket.inet_ntop(socket.AF_INET, host_bytes)}:{port}"
    return f"[{socket.inet_ntop(socket.AF_INET6, host_bytes)}]:{port}"


def listening_tcp_addresses_by_inode_in_the_network_namespace_of(pid: int) -> "dict[str, str]":
    """The local address of every TCP socket in LISTEN, IPv4 and IPv6, that
    `pid` can see, by its inode, off Linux's `/proc`."""
    listening_tcp_addresses_by_inode: "dict[str, str]" = {}
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
                listening_tcp_addresses_by_inode[columns[9]] = tcp_table_address_as_the_scan_names_it(
                    columns[1]
                )
    return listening_tcp_addresses_by_inode


def socket_names_lsof_reports_held_by(pid: int, *socket_selection: str) -> "set[str]":
    """The name `lsof` gives every socket `pid` holds that `socket_selection` selects."""
    # `-a` ANDs the selections, so only `pid`'s own sockets are listed; `-F n`
    # prints each one's name on a line of its own, prefixed `n`.
    completed = subprocess.run(
        ["lsof", "-nP", "-a", "-p", str(pid), *socket_selection, "-F", "n"],
        capture_output=True,
        text=True,
        check=False,
    )
    # lsof exits 1, printing nothing, when nothing matches the selection.
    assert completed.returncode == 0 or (completed.returncode == 1 and not completed.stdout), (
        f"`{' '.join(completed.args)}` exited {completed.returncode}:\n{completed.stderr}"
    )
    return {line[1:] for line in completed.stdout.splitlines() if line.startswith("n")}


def listening_tcp_sockets_held_by(pid: int) -> "set[str]":
    """The local address of every TCP socket in LISTEN, IPv4 and IPv6, `pid`
    holds — off Linux's `/proc`, off `lsof` on macOS — as `host:port`, or
    `[host]:port` for IPv6."""
    if sys.platform == "linux":
        listening_tcp_addresses_by_inode = (
            listening_tcp_addresses_by_inode_in_the_network_namespace_of(pid)
        )
        return {
            listening_tcp_addresses_by_inode[socket_inode]
            for socket_inode in socket_inodes_held_by(pid)
            if socket_inode in listening_tcp_addresses_by_inode
        }
    return socket_names_lsof_reports_held_by(pid, "-iTCP", "-sTCP:LISTEN")


def listening_tcp_socket_as_the_scan_names_it(tcp_listener: socket.socket) -> str:
    """`tcp_listener` as `listening_tcp_sockets_held_by` names it."""
    host, port = tcp_listener.getsockname()[:2]
    return f"[{host}]:{port}" if tcp_listener.family == socket.AF_INET6 else f"{host}:{port}"


def assert_the_tcp_listener_scan_sees_the_listeners_this_process_holds() -> None:
    """Bind loopback TCP listeners here and require the scan to find each one."""
    tcp_listeners = [socket.create_server(("127.0.0.1", 0))]
    try:
        tcp_listeners.append(socket.create_server(("::1", 0), family=socket.AF_INET6))
    except OSError:
        # A host without IPv6 loopback has no IPv6 listener to check the scan against.
        pass
    try:
        listening_tcp_sockets_this_process_holds = listening_tcp_sockets_held_by(os.getpid())
        for tcp_listener in tcp_listeners:
            tcp_listener_as_scanned = listening_tcp_socket_as_the_scan_names_it(tcp_listener)
            assert tcp_listener_as_scanned in listening_tcp_sockets_this_process_holds, (
                f"the TCP scan must see the listener this test holds at "
                f"{tcp_listener.getsockname()} ({tcp_listener_as_scanned}); it saw "
                f"{sorted(listening_tcp_sockets_this_process_holds)}"
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


def assert_the_scan_sees_the_unix_socket_listener_held_by(pid: int, unix_socket_path: str) -> None:
    """Require the scan of `pid`'s descriptors to find the Unix socket it listens on at `unix_socket_path`."""
    if sys.platform == "linux":
        listener_inode = unix_socket_listener_inode_at(pid, unix_socket_path)
        assert listener_inode is not None, (
            f"no Unix socket listens at {unix_socket_path} in /proc/{pid}/net/unix"
        )
        held_socket_inodes = socket_inodes_held_by(pid)
        assert listener_inode in held_socket_inodes, (
            f"pid {pid}'s descriptors must include its listener at {unix_socket_path} (inode "
            f"{listener_inode}); they hold sockets {sorted(held_socket_inodes)}"
        )
        return
    unix_socket_names = socket_names_lsof_reports_held_by(pid, "-U")
    # Some lsof builds follow the path with the socket's type: `<path> type=STREAM`.
    assert any(
        unix_socket_name == unix_socket_path or unix_socket_name.startswith(f"{unix_socket_path} ")
        for unix_socket_name in unix_socket_names
    ), (
        f"lsof must see pid {pid}'s listener at {unix_socket_path}; it saw {sorted(unix_socket_names)}"
    )


def test_the_runtime_listens_on_tcp_at_its_loopback_http_alone(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """Nothing on the network can reach the runtime's control API, and its HTTP
    listens nowhere but loopback: `tatolabd` holds one TCP socket in LISTEN, its
    loopback HTTP on `127.0.0.1`.

    The same scan — Linux's `/proc`, macOS's `lsof` — has to find the TCP
    listeners the test itself holds and the local API socket's listener, and
    the runtime has to answer over it, so what the scan reports of the runtime
    is the runtime's own and not a scan that saw nothing.
    """
    app_directory = make_tatolab_project({"stream.py": STREAM_WITH_ONE_NATIVE_SOURCE})
    loopback_http_port = a_tcp_port_free_on_loopback()
    tatolabd = start_tatolabd(
        extra_environment={TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE: str(loopback_http_port)}
    )

    stream_run = load_the_projects_stream(tatolabd, app_directory)
    stream_run.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    stream_id = stream_id_of_the_project(app_directory)
    graph = tatolabd.local_api_client().call_tool("graph", {"stream": stream_id})
    assert graph["stream"] == stream_id

    assert_the_tcp_listener_scan_sees_the_listeners_this_process_holds()
    assert_the_scan_sees_the_unix_socket_listener_held_by(
        tatolabd.pid, str(tatolabd.local_api_socket_path)
    )
    listening_tcp_sockets_it_holds = listening_tcp_sockets_held_by(tatolabd.pid)
    assert listening_tcp_sockets_it_holds == {f"127.0.0.1:{loopback_http_port}"}, (
        f"tatolabd holds TCP sockets in LISTEN ({sorted(listening_tcp_sockets_it_holds)}); "
        f"standard error ended:\n{tatolabd.recent_stderr()}"
    )

    assert stream_run.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0


def succeeded(completed: "subprocess.CompletedProcess[str]") -> str:
    """The verb's stdout, once it exited 0 — or a failure carrying what it said."""
    assert completed.returncode == 0, (
        f"`tatolab {' '.join(map(str, completed.args[1:]))}` exited {completed.returncode}:\n"
        f"stdout: {completed.stdout}\nstderr: {completed.stderr}"
    )
    return completed.stdout


def test_every_observation_verb_reaches_a_loaded_stream_through_the_local_api_socket(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    run_tatolab_observation_verb: "Callable[..., subprocess.CompletedProcess[str]]",
):
    """`tatolab streams`, `graph` and `logs`, driven the way a user drives them:
    a separate process, naming the stream. `graph` counts the bags the source
    writes once a connect has wired its output."""
    app_directory = make_tatolab_project(project_files_with_helper_placed_processors(1))
    tatolabd = start_tatolabd()
    stream_run = load_the_projects_stream(tatolabd, app_directory)
    stream_run.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)

    stream_id = stream_id_of_the_project(app_directory)
    listed = succeeded(run_tatolab_observation_verb("streams"))
    assert listed.splitlines()[0].split() == [
        "ID",
        "NAME",
        "STATE",
        "NODES",
        "PROJECT",
        "FUNCTION",
    ], listed
    assert re.search(
        rf"^{re.escape(stream_id)}\s+main\s+running\s+2\s+", listed, re.MULTILINE
    ), listed

    bags_the_source_wrote = 0
    deadline = time.monotonic() + FIRST_BAG_WRITTEN_TIMEOUT_SECONDS
    while bags_the_source_wrote == 0 and time.monotonic() < deadline:
        graph = json.loads(
            succeeded(run_tatolab_observation_verb("graph", "--stream", stream_id))
        )
        [source_node] = [
            graph_node
            for graph_node in graph["nodes"]
            if graph_node["name"].startswith("testpatternsource")
        ]
        bags_the_source_wrote = source_node["components"]["metrics"][
            "written_bags_by_output_port"
        ].get("video", 0)
        time.sleep(0.1)
    assert bags_the_source_wrote > 0, f"the source wrote nothing on `video`: {source_node}"

    succeeded(run_tatolab_observation_verb("logs", "--stream", stream_id))

    assert stream_run.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0


def iceoryx2_node_details_in(iceoryx2_domain_root: Path) -> "set[Path]":
    return set((iceoryx2_domain_root / "nodes").glob("*/*node.details"))


def test_the_runtime_and_its_processor_interpreter_share_the_runtime_directorys_iceoryx2_domain(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The stream carries a Python processor, so a frame reaching it proves
    `tatolabd` and the processor interpreter opened their nodes in one iceoryx2
    domain: the runtime directory's, where the surface socket sits too."""
    app_directory = make_tatolab_project(project_files_with_helper_placed_processors(1))
    tatolabd = start_tatolabd()
    runtime_directory = tatolabd.machine_directories.runtime_directory
    iceoryx2_node_details_before = iceoryx2_node_details_in(runtime_directory / "iox2")

    stream_run = load_the_projects_stream(tatolabd, app_directory)
    tatolabd.await_marker(LIVE_HELPER_MARKER_NAME, timeout=NODE_READY_TIMEOUT_SECONDS)
    new_node_details = (
        iceoryx2_node_details_in(runtime_directory / "iox2") - iceoryx2_node_details_before
    )

    assert {details.name for details in new_node_details} == {f"sl{os.getuid()}_node.details"}
    assert len(new_node_details) == 1, (
        f"the processor interpreter must open its node in the runtime directory's domain, "
        f"beside tatolabd's; found {sorted(map(str, new_node_details))}"
    )
    assert len(iceoryx2_node_details_before) == 1, sorted(map(str, iceoryx2_node_details_before))
    assert len(list(runtime_directory.glob("surface-share-*.sock"))) == 1

    assert stream_run.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0


def test_a_native_block_added_without_config_reaches_a_running_graph(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
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
    tatolabd = start_tatolabd()

    stream_run = load_the_projects_stream(tatolabd, app_directory)
    stream_run.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    running_graph = tatolabd.local_api_client().await_every_node_running(
        stream=stream_id_of_the_project(app_directory)
    )
    assert [node["type"] for node in running_graph["nodes"]].count(TestPatternSource.type) == 1

    assert stream_run.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0


def the_first_streams_output(tatolabd: TatolabdUnderTest, stream_id: str) -> str:
    """`tatolabd`'s standard error through the first stop of the stream `stream_id`."""
    the_first_streams_stop = tatolabd.await_stderr_containing(
        the_stream_stopped_log_line(stream_id), timeout=CLEAN_EXIT_TIMEOUT_SECONDS
    )
    return tatolabd.stderr_text.split(the_first_streams_stop, 1)[0] + the_first_streams_stop


def frames_the_scaffolded_effect_wrote(tatolabd: TatolabdUnderTest, stream_id: str) -> int:
    """The bags the running scaffold's effect has written on its output, read off `graph`."""
    [effect_node] = [
        graph_node
        for graph_node in tatolabd.local_api_client().graph(stream=stream_id)["nodes"]
        if graph_node["name"] == SCAFFOLDED_EFFECT_NODE_NAME
    ]
    return effect_node["components"]["metrics"]["written_bags_by_output_port"][
        SCAFFOLDED_EFFECT_OUTPUT_PORT
    ]


def test_the_scaffolded_app_reaches_a_running_graph(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """What `tatolab new` writes must actually run, frame after frame.

    Run exactly as scaffolded:
    `dev` has the runtime compile the scaffold's `@stream` and load the graph
    it builds, so the graph the local API renders carries the stream's name and
    the exposure it declared. A listed stream alone proves almost nothing here:
    it is listed whether or not `process()` ever succeeds, so the assertions
    that carry this test are the ones on the run's own records. `process()
    failed` catches an effect that raises every frame; the count of frames
    the effect wrote catches an effect that is correct but so slow the demo is
    a slideshow. The meter's line is the logic half of the first minute: a
    fan-out reader that never reports is a graph that carries the picture and
    drops the rest.
    """
    app_directory = make_scaffolded_sample_stream_project(make_tatolab_project, run_tatolab)
    tatolabd = start_tatolabd()

    dev = load_the_projects_stream(tatolabd, app_directory, "dev")
    dev.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    stream_id = stream_id_of_the_project(app_directory)
    live_graph = tatolabd.local_api_client().call_tool("graph", {"stream": stream_id})
    assert live_graph["stream"] == stream_id, (
        f"the runtime must render the stream it was loaded as; graph was {live_graph}"
    )
    assert live_graph["exposed"] == [
        {
            "node": SCAFFOLDED_ENCODER_NODE_NAME,
            "port": SCAFFOLDED_ENCODER_OUTPUT_PORT,
            "level": "public",
        }
    ], f"the runtime must render the exposure the stream declared; graph was {live_graph}"
    assert {graph_node["name"] for graph_node in live_graph["nodes"]} == {
        "testpatternsource",
        SCAFFOLDED_EFFECT_NODE_NAME,
        "brightnessmeter",
        SCAFFOLDED_ENCODER_NODE_NAME,
    }

    # Long enough for the source to have driven many frames through the effect.
    time.sleep(SCAFFOLD_OBSERVATION_WINDOW_SECONDS)
    frames_written = frames_the_scaffolded_effect_wrote(tatolabd, stream_id)
    dev.interrupt()
    assert dev.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, dev.recent_stderr()
    assert dev.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0

    stream_output = the_first_streams_output(tatolabd, stream_id)
    assert_the_effect_carried_live_video(
        tatolabd, stream_output, frames_written, "the app `tatolab new` writes"
    )
    frames_encoded = [
        int(count)
        for count in SCAFFOLDED_ENCODER_FRAMES_ENCODED_AT_TEARDOWN.findall(stream_output)
    ]
    assert any(frames_encoded) or SCAFFOLDED_ENCODER_REFUSAL in stream_output, (
        f"the scaffolded encoder neither encoded an access unit nor refused by name; standard "
        f"error ended:\n{tatolabd.recent_stderr()}"
    )
    meter_reports = len(SCAFFOLDED_METER_REPORT.findall(stream_output))
    assert meter_reports >= MINIMUM_METER_REPORTS, (
        f"the scaffolded meter logged a brightness {meter_reports} times in "
        f"{SCAFFOLD_OBSERVATION_WINDOW_SECONDS}s; standard error ended:\n{tatolabd.recent_stderr()}"
    )
    assert meter_reports <= MAXIMUM_METER_REPORTS, (
        f"the scaffolded meter logged a brightness {meter_reports} times in "
        f"{SCAFFOLD_OBSERVATION_WINDOW_SECONDS}s — that is per frame, not once a second"
    )


def test_a_scaffolded_app_with_a_cross_floor_finding_warns_and_starts_anyway(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The cross-floor check informs; it never walls off a start.

    The finding sits in a function nothing calls, so the app needs neither
    cupy nor CUDA to run: the check reads source, and this proves a finding in
    it reaches the user who ran `tatolab run` — in the project's log, under the
    load's action, which `tatolab logs --action` reads — without costing the
    start.
    """
    app_directory = make_scaffolded_sample_stream_project(make_tatolab_project, run_tatolab)
    effect_module = app_directory / SCAFFOLDED_EFFECT_MODULE_PATH
    effect_module.write_text(
        effect_module.read_text()
        + "\n\ndef never_called():\n"
        "    import cupy\n"
        "    import torch\n"
        '    return torch.device("cuda")\n'
    )
    tatolabd = start_tatolabd()

    tatolab_run = load_the_projects_stream(tatolabd, app_directory)
    loaded = tatolab_run.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    tatolabd.await_stderr_containing(ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS)
    assert tatolab_run.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0
    the_loads_records = run_tatolab(
        "logs",
        "--dir",
        app_directory,
        "--action",
        tatolab_run.queued_action(),
        working_directory=app_directory,
    )

    output = succeeded(the_loads_records)
    assert f"{SCAFFOLDED_EFFECT_MODULE_PATH}:" in output and "imports `cupy`" in output, (
        f"the warning block in the load's records must name the cupy import; they were:\n{output}"
    )
    assert "names the device 'cuda'" in output, (
        f"the warning block in the load's records must name the device literal; they were:\n"
        f"{output}"
    )
    assert "cross-floor check" in output, output
    assert loaded["stream_id"] == stream_id_of_the_project(app_directory), (
        f"the load the warning came with must still start the stream: {loaded}"
    )


def assert_the_effect_carried_live_video(
    tatolabd: TatolabdUnderTest, output: str, frames_written: int, what_ran: str
) -> None:
    """Require one stream to have carried live video, not a slideshow: the
    frames its effect wrote while it ran, and its records once it stopped.

    The effect's written count is the honest measure — an effect can be
    correct and still leave the demo at roughly 4 frames a second, which is
    what editing the write-combined mapping in place through a strided view
    produced.
    """
    process_failures_but_the_encoders_refusal = [
        line
        for line in output.splitlines()
        if "process() failed" in line and SCAFFOLDED_ENCODER_REFUSAL not in line
    ]
    assert not process_failures_but_the_encoders_refusal, (
        f"{what_ran}: a node failed a live frame; standard error ended:\n"
        f"{tatolabd.recent_stderr()}"
    )
    assert frames_written >= MINIMUM_FRAMES_FOR_LIVE_VIDEO, (
        f"{what_ran}'s effect wrote only {frames_written} frames in "
        f"{SCAFFOLD_OBSERVATION_WINDOW_SECONDS}s — that is a slideshow, not live video"
    )
    # Every reader in the scaffold drains its listener, so this guards the
    # run's records rather than the undrained-listener regression, which
    # `an_undrained_listener_is_delivered_every_send_and_drains_the_whole_count`
    # in `iceoryx2/output.rs` locks.
    # The text iceoryx2 logs for a notify that did not reach a listener.
    undeliverable_notifications = output.count("Unable to send notification")
    assert undeliverable_notifications == 0, (
        f"{what_ran}: {undeliverable_notifications} undeliverable link notifications in "
        f"{SCAFFOLD_OBSERVATION_WINDOW_SECONDS}s — a notify failed to reach its listener; "
        f"standard error ended:\n{tatolabd.recent_stderr()}"
    )


def test_every_helper_interpreter_goes_live_inside_the_startup_budget(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The N-processor-interpreter startup budget the MVP minute has to pay.

    Every Python processor is its own processor interpreter, so a graph's load
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
    tatolabd = start_tatolabd()
    launched_at = time.monotonic()
    fleet = load_the_projects_stream(tatolabd, fleet_app, "dev")
    # Waited for on a bound far above the budget rather than on the budget
    # itself: a wait that expires exactly at the ceiling can only ever report a
    # timeout, and what a blown budget should say is how long it actually took.
    tatolabd.await_marker(
        LIVE_HELPER_MARKER_NAME,
        occurrence=HELPER_PLACED_PROCESSOR_COUNT,
        timeout=NODE_READY_TIMEOUT_SECONDS,
    )
    seconds_for_every_helper = time.monotonic() - launched_at

    reporting_pids = set(tatolabd.marker_payloads(LIVE_HELPER_MARKER_NAME))
    assert len(reporting_pids) == HELPER_PLACED_PROCESSOR_COUNT, (
        f"{HELPER_PLACED_PROCESSOR_COUNT} processors reported from "
        f"{len(reporting_pids)} processes — every Python processor gets its own"
    )
    assert fleet.pid not in reporting_pids, "a processor reported from tatolab's own process"
    for reporting_pid in reporting_pids:
        assert_runs_in_a_process_of_its_own_beneath(reporting_pid, tatolabd.pid)
    assert seconds_for_every_helper < MAXIMUM_SECONDS_FOR_EVERY_HELPER_TO_GO_LIVE, (
        f"{HELPER_PLACED_PROCESSOR_COUNT} processor interpreters took "
        f"{seconds_for_every_helper:.2f}s to reach live traffic — the minute does "
        f"not absorb that"
    )
    fleet.interrupt()
    assert fleet.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, fleet.recent_stderr()
    assert fleet.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        f"a graph of {HELPER_PLACED_PROCESSOR_COUNT} helpers must still unload cleanly"
    )
    tatolabd.await_stderr_containing(
        the_stream_stopped_log_line(stream_id_of_the_project(fleet_app)),
        timeout=CLEAN_EXIT_TIMEOUT_SECONDS,
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
            "        ctx.outputs.write(",
            "        if not self.announced:\n"
            "            self.announced = True\n"
            '            log.info("MARKER:EDITED_EFFECT")\n'
            "        ctx.outputs.write(",
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


def node_ids_of(graph: "dict[str, Any]") -> "list[str]":
    """The ids of a live graph's nodes, which the runtime mints afresh for each load."""
    return sorted(node["id"] for node in graph["nodes"])


def test_the_edit_loop_survives_a_bad_save_and_runs_a_good_one(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The MVP edit loop: `dev` queuing another load of the project on every save.

    A save is a file write, and `dev` queues a load over its connection. A
    broken save is never refused: the runtime unloads the running stream and
    records it failed with the traceback, which the project's log carries and
    `dev` prints, and the next good save loads the edited code. Both halves run
    against the same `dev` and the same `tatolabd` in sequence because the first
    is only meaningful if the second follows: a loop that survives a bad save by
    ignoring the file entirely would pass the first alone.
    """
    app_directory = make_scaffolded_sample_stream_project(make_tatolab_project, run_tatolab)
    effect_module = app_directory / SCAFFOLDED_EFFECT_MODULE_PATH
    last_good_effect_source = effect_module.read_text()
    stream_id = stream_id_of_the_project(app_directory)
    tatolabd = start_tatolabd()
    local_api = tatolabd.local_api_client()

    dev = load_the_projects_stream(tatolabd, app_directory, "dev")
    dev.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    tatolabd.await_stderr_containing(ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS)
    node_ids_before_the_bad_save = node_ids_of(
        local_api.await_every_node_running(stream=stream_id)
    )

    time.sleep(SECONDS_OF_LIVE_VIDEO_BEFORE_THE_BAD_SAVE_LANDS)
    effect_module.write_text("def process(self ctx:\n    this does not parse\n")
    dev.await_stderr_containing(
        DEV_QUEUING_ANOTHER_LOAD_AFTER_A_SAVE, timeout=NODE_READY_TIMEOUT_SECONDS
    )
    bad_saves_failed_stream = dev.failed_stream(timeout=NODE_READY_TIMEOUT_SECONDS, occurrence=2)
    assert bad_saves_failed_stream is not None, dev.recent_stderr()
    assert bad_saves_failed_stream[0] == stream_id, bad_saves_failed_stream
    await_dev_printed(dev, f"the stream `{stream_id}` failed: ", timeout=NODE_READY_TIMEOUT_SECONDS)
    await_dev_printed(dev, "SyntaxError", timeout=NODE_READY_TIMEOUT_SECONDS)
    assert "halftone_lens_effect.py" in dev.stdout_text, dev.stdout_text[-4000:]
    assert dev.process.poll() is None, (
        f"a bad save must not take `dev` down; standard error ended:\n{dev.recent_stderr()}"
    )
    tatolabd.await_stderr_containing(
        the_stream_stopped_log_line(stream_id), timeout=NODE_READY_TIMEOUT_SECONDS
    )
    [listed] = local_api.list_streams()
    assert (listed["stream"], listed["state"]) == (stream_id, "failed"), listed
    assert "SyntaxError" in listed["failed_because"], listed

    effect_module.write_text(the_scaffolded_effect_edited(last_good_effect_source))
    dev.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS, occurrence=3)
    tatolabd.await_marker("EDITED_EFFECT", timeout=NODE_READY_TIMEOUT_SECONDS)
    tatolabd.await_stderr_containing(
        ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS, occurrence=2
    )
    stderr_lines = list(tatolabd.stderr_lines)
    engine_started_line_indices = [
        line_index
        for line_index, stderr_line in enumerate(stderr_lines)
        if ENGINE_STARTED_LOG_LINE in stderr_line
    ]
    assert any(
        ENGINE_GRACEFUL_STOP_LOG_LINE in stderr_line
        for stderr_line in stderr_lines[: engine_started_line_indices[1]]
    ), "the stream the bad save failed did not stop gracefully before the edited one started"
    node_ids_after_the_good_save = node_ids_of(
        local_api.await_every_node_running(stream=stream_id)
    )
    assert set(node_ids_after_the_good_save).isdisjoint(node_ids_before_the_bad_save), (
        "a good save loads the stream with a load of its own"
    )
    assert [listed["state"] for listed in local_api.list_streams()] == ["running"]
    assert tatolabd.process.poll() is None, "every load of the edit loop lands on the one runtime"

    dev.interrupt()
    assert dev.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, dev.recent_stderr()


def test_a_bad_config_is_reported_without_a_launcher_traceback(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The runtime's load fails a config its built-in does not take, naming
    the node and the setting, before the engine starts, and records the stream
    failed. It is still the app's problem, not a launcher crash."""
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
    tatolabd = start_tatolabd()

    stream_run = load_the_projects_stream(tatolabd, app_directory)

    failed_stream = stream_run.failed_stream(timeout=NODE_READY_TIMEOUT_SECONDS)
    assert failed_stream is not None, f"the load must record the stream failed: {stream_run.stdout_text}"
    failed_stream_id, failed_because = failed_stream
    assert "Traceback (most recent call last)" not in failed_because, (
        f"an engine-side failure must not arrive as a launcher traceback; it was:\n{failed_because}"
    )
    assert failed_stream_id == stream_id_of_the_project(app_directory)
    assert "`tatolab.stream:TestPatternSource`" in failed_because, failed_because
    assert "width: invalid type" in failed_because, failed_because
    assert ENGINE_STARTED_LOG_LINE not in tatolabd.stderr_text
    [listed] = tatolabd.local_api_client().list_streams()
    assert (listed["stream"], listed["state"]) == (failed_stream_id, "failed"), listed


@pytest.mark.parametrize("verb", ["run", "dev"])
def test_a_stream_function_that_raises_is_recorded_failed_with_its_traceback_and_loads_nothing(
    verb: str,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """A graph that failed to build loads nothing, and the runtime takes the
    stream in failed with the function's traceback.

    `run` returns once the load is queued, and the load records the stream
    failed; `dev` prints the failure from the project's log and waits for the
    save that fixes it, until a Ctrl-C ends it.
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
    stream_id = stream_id_of_the_project(app_directory)
    tatolabd = start_tatolabd()

    stream_run = load_the_projects_stream(tatolabd, app_directory, verb)

    failed_stream = stream_run.failed_stream(timeout=NODE_READY_TIMEOUT_SECONDS)
    assert failed_stream is not None, stream_run.recent_stderr()
    failed_stream_id, printed_reason = failed_stream
    assert failed_stream_id == stream_id
    if verb == "run":
        assert stream_run.await_exit(timeout=NODE_READY_TIMEOUT_SECONDS) == 0, (
            f"`run` exits 0 once the load is queued; standard error:\n{stream_run.recent_stderr()}"
        )
    else:
        await_dev_printed(stream_run, "ValueError: bad wiring", timeout=NODE_READY_TIMEOUT_SECONDS)
        stream_run.interrupt()
        assert stream_run.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, stream_run.recent_stderr()
        printed_reason = stream_run.stdout_text
    assert "ValueError: bad wiring" in printed_reason, printed_reason
    assert 'stream.py", line 6, in main' in printed_reason, printed_reason
    [listed] = tatolabd.local_api_client().list_streams()
    assert (listed["stream"], listed["state"]) == (stream_id, "failed"), listed
    assert "ValueError: bad wiring" in listed["failed_because"], listed
    assert "[start] Starting the stream" not in tatolabd.stderr_text, tatolabd.recent_stderr()


def test_a_stream_py_defining_no_stream_is_a_failed_project_until_a_run_defines_one(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """A `stream.py` that imports and defines no `@stream` is no refusal: the
    load lists the project as `<folder>-<hash>` failed with the missing `@stream`
    and a sample of one; a run once it defines one replaces it."""
    app_directory = make_tatolab_project({"stream.py": "PIPELINE = 1\n"})
    tatolabd = start_tatolabd()

    stream_run = load_the_projects_stream(tatolabd, app_directory)

    failed_stream = stream_run.failed_stream(timeout=NODE_READY_TIMEOUT_SECONDS)
    assert failed_stream is not None, stream_run.stdout_text + stream_run.stderr_text
    failed_project_id, failed_because = failed_stream
    assert "stream.py` defines no @stream function" in failed_because, failed_because
    assert "    @stream\n    def main(stream_builder: StreamBuilder) -> None:\n" in failed_because
    [listed] = tatolabd.local_api_client().list_streams()
    assert (listed["stream"], listed["state"]) == (failed_project_id, "failed"), listed
    assert listed["location"]["stream_function"] == "", listed

    (app_directory / "stream.py").write_text(
        "from tatolab.stream import StreamBuilder, TestPatternSource, stream\n"
        "\n"
        "\n"
        "@stream\n"
        "def main(stream_builder: StreamBuilder) -> None:\n"
        "    stream_builder.add(TestPatternSource)\n",
        encoding="utf-8",
    )
    defined_run = load_the_projects_stream(tatolabd, app_directory)
    defined_run.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)

    [listed] = tatolabd.local_api_client().list_streams()
    assert listed["stream"] == stream_id_of_the_project(app_directory), listed
    assert listed["state"] == "running", listed


STREAM_PY_WITH_TWO_STREAMS = (
    "from tatolab.stream import StreamBuilder, TestPatternSource, stream\n"
    "\n"
    "\n"
    "@stream\n"
    "def camera(stream_builder: StreamBuilder) -> None:\n"
    "    stream_builder.add(TestPatternSource)\n"
    "\n"
    "\n"
    "@stream\n"
    "def preview(stream_builder: StreamBuilder) -> None:\n"
    "    stream_builder.add(TestPatternSource)\n"
)


def test_a_projects_log_holds_every_stream_and_its_failed_syncs_uv_lines_read_by_project(
    tmp_path: Path,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """`logs` reads a project's one log by its directory: a sync uv refuses lands
    there with no `stream`, and two streams of one project write one
    `project.jsonl`, each record its stream's, read by stream after `rm` too."""
    tatolabd = start_tatolabd()
    client = tatolabd.local_api_client()

    unsyncable_directory = tmp_path / "unsyncable"
    unsyncable_directory.mkdir()
    (unsyncable_directory / "pyproject.toml").write_text("this is [not toml\n", encoding="utf-8")
    (unsyncable_directory / "stream.py").write_text(STREAM_WITH_ONE_NATIVE_SOURCE, encoding="utf-8")
    [unsynced] = client.call_tool(
        "run_stream", {"project_directory": str(unsyncable_directory.resolve())}
    )["streams"]
    assert unsynced["state"] == "failed", unsynced
    unsyncable_page = client.call_tool(
        "logs", {"project_directory": str(unsyncable_directory.resolve())}
    )
    uvs_lines = [
        record
        for record in unsyncable_page["records"]
        if "pyproject.toml" in record["message"] and "stream" not in record
    ]
    assert uvs_lines, unsyncable_page
    assert all(record["runtime_id"] for record in uvs_lines), uvs_lines

    app_directory = make_tatolab_project(
        {
            "pyproject.toml": '[project]\nname = "two-streams"\nversion = "0.1.0"\n',
            "stream.py": STREAM_PY_WITH_TWO_STREAMS,
        }
    )
    project_directory = str(app_directory.resolve())
    running_ids = set()
    for stream_function in ("stream:camera", "stream:preview"):
        [loaded] = client.call_tool(
            "run_stream",
            {"project_directory": project_directory, "stream_function": stream_function},
        )["streams"]
        assert loaded["state"] == "running", loaded
        running_ids.add(loaded["stream"])
    assert (app_directory / ".streamlib" / "logs" / "project.jsonl").is_file()

    every_record = client.call_tool("logs", {"project_directory": project_directory})["records"]
    assert {record.get("stream") for record in every_record} >= running_ids, every_record

    camera_id = stream_id_of_the_location(app_directory.resolve(), "stream:camera")
    camera_records_while_loaded = client.call_tool(
        "logs", {"project_directory": project_directory, "stream": camera_id}
    )["records"]
    assert camera_records_while_loaded, every_record
    assert all(record["stream"] == camera_id for record in camera_records_while_loaded)

    removal = client.call_tool("remove_project", {"project_directory": project_directory})
    assert client.stream_action_once_ended(removal["action"])["state"] == "done"
    camera_records_after_rm = client.call_tool(
        "logs",
        {"project_directory": project_directory, "stream": camera_id, "count": 4096},
    )["records"]
    assert camera_records_after_rm[: len(camera_records_while_loaded)] == (
        camera_records_while_loaded
    )


STREAM_PY_WITH_TWO_GOOD_STREAMS_AND_ONE_RAISING = (
    "from tatolab.stream import StreamBuilder, TestPatternSource, stream\n"
    "\n"
    "\n"
    "@stream\n"
    "def camera(stream_builder: StreamBuilder) -> None:\n"
    "    stream_builder.add(TestPatternSource)\n"
    "\n"
    "\n"
    "@stream\n"
    "def preview(stream_builder: StreamBuilder) -> None:\n"
    "    stream_builder.add(TestPatternSource)\n"
    "\n"
    "\n"
    "@stream\n"
    "def broken(stream_builder: StreamBuilder) -> None:\n"
    '    raise ValueError("bad wiring")\n'
)


def test_a_run_takes_in_every_stream_of_stream_py_and_fails_one_no_longer_defined(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """`run` loads every `@stream` in `stream.py`, each its own stream: two
    run, and the one that raises is failed with its traceback; a run once
    `stream.py` drops one records it failed, naming the function."""
    app_directory = make_tatolab_project(
        {"stream.py": STREAM_PY_WITH_TWO_GOOD_STREAMS_AND_ONE_RAISING}
    )
    tatolabd = start_tatolabd()
    stream_id_of = lambda function: stream_id_of_the_location(  # noqa: E731
        app_directory.resolve(), f"stream:{function}"
    )

    stream_run = load_the_projects_stream(tatolabd, app_directory)

    ended_load = stream_run.action_once_ended(timeout=NODE_READY_TIMEOUT_SECONDS)
    assert ended_load["state"] == "done", ended_load
    assert {stream["stream"]: stream["state"] for stream in ended_load["streams"]} == {
        stream_id_of("camera"): "running",
        stream_id_of("preview"): "running",
        stream_id_of("broken"): "failed",
    }, ended_load
    assert stream_run.stdout_text == (
        f"{ended_load['action']} queued: load of {app_directory.resolve()}\n"
        "  follow it: tatolab logs -f\n"
    ), stream_run.stdout_text
    failed_stream = stream_run.failed_stream()
    assert failed_stream is not None, ended_load
    assert failed_stream[0] == stream_id_of("broken"), failed_stream
    assert "ValueError: bad wiring" in failed_stream[1], failed_stream[1]
    states = {
        listed["stream"]: listed["state"]
        for listed in tatolabd.local_api_client().list_streams()
    }
    assert states == {
        stream_id_of("camera"): "running",
        stream_id_of("preview"): "running",
        stream_id_of("broken"): "failed",
    }, states

    (app_directory / "stream.py").write_text(
        STREAM_PY_WITH_TWO_GOOD_STREAMS_AND_ONE_RAISING.replace(
            "@stream\ndef preview", "def preview"
        ),
        encoding="utf-8",
    )
    rerun = load_the_projects_stream(tatolabd, app_directory)
    assert rerun.action_once_ended(timeout=NODE_READY_TIMEOUT_SECONDS)["state"] == "done"

    listed_by_id = {
        listed["stream"]: listed for listed in tatolabd.local_api_client().list_streams()
    }
    assert listed_by_id[stream_id_of("camera")]["state"] == "running", listed_by_id
    preview = listed_by_id[stream_id_of("preview")]
    assert preview["state"] == "failed", preview
    assert preview["failed_because"] == "`stream:preview` is no longer defined in stream.py"

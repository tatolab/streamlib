# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab run` / `dev` loading a project's stream into the machine's runtime, end to end.

The runtime compiles the project's stream in the project's own venv — here a
symlink to the suite venv, holding `tatolab-stream` and nothing of the runtime —
and loads it attached to the `tatolab` that asked: the local API names it by
its stream name, every observation verb reaches it through the one socket, and
a Ctrl-C to `tatolab` unloads it while `tatolabd` keeps serving. Starting a
stream initializes a GPU context, so the module needs a device.

The MVP minute is measured here too, with every processor in its own processor
interpreter: what `new` writes runs frame after frame, a graph of helpers goes
live inside the startup budget their interpreters cost, and the edit loop —
`dev` loading the stream again on each save — reports a bad save and loads the
next good one, on the same runtime.
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

from conftest import AttachedTatolabRun, PrivateMachineDirectories, TatolabdUnderTest
from runtime_process_under_test import ENGINE_GRACEFUL_STOP_LOG_LINE, ENGINE_STARTED_LOG_LINE
from runtime_unit_under_test import (
    STREAM_ON_RUNTIME_SUITE_DIRECTORY,
    SUITE_VENV_INTERPRETER,
    RuntimeUnitUnderTest,
)
from stream_runs_on_tatolabd import TatolabRunOfAProject
from tatolab.stream import TestPatternSource
from test_processor_interpreter_lend import assert_runs_in_a_process_of_its_own_beneath

pytestmark = pytest.mark.requires_gpu

# A load is the project's compile + the describe of its Python types + the
# engine's GPU context on the runtime's first start.
NODE_READY_TIMEOUT_SECONDS = 90.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0
LOCAL_API_SOCKET_FILE_MODE = 0o600
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
DISPLAY_WINDOW_STOPPED = "DisplayWindow: stopped"
DISPLAY_WINDOW_FRAME_COUNT = re.compile(r"DisplayWindow: stopped \((\d+) frames\)")
SCAFFOLDED_EFFECT_MODULE_PATH = "nodes/inverting_effect.py"

# A `tap` collects its sample server-side, so a verb can legitimately take a moment.
OBSERVATION_VERB_TIMEOUT_SECONDS = 60.0
# The id form races the pool: a frame tapped at 30fps can be recycled before
# its exchange lands, which is a `410` the caller answers by tapping again.
SURFACE_ID_EXCHANGE_ATTEMPTS = 10
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"

#: The one stream every project here compiles to.
STREAM_NAME = "main"

#: The engine's line once the attached stream's unload has finished.
THE_STREAM_STOPPED_LOG_LINE = f"[stop] The stream `{STREAM_NAME}` stopped"

# What `dev` says as a save makes it stop the stream and load it again.
DEV_LOADING_AGAIN_AFTER_A_SAVE = f"tatolab dev: a saved change — loading {STREAM_NAME} again"
# What `dev` says when a load is refused and nothing is loaded until the next save.
DEV_NO_STREAM_IS_LOADED = "tatolab dev: no stream is loaded — fix it and save again"
# What `dev` says once a refused save left the stream it was to replace running.
DEV_KEPT_THE_RUNNING_STREAM = (
    f"tatolab dev: {STREAM_NAME} keeps running the last save that loaded — fix it and save again"
)

# The `surface_id` field of one bag `tatolab tap` forwarded, given its hex.
# Run with the lend on `PYTHONPATH`: a tapped bag is the channel's
# transport-framed msgpack, and the engine's own decoder is what reads it.
SURFACE_ID_OF_A_TAPPED_BAG_SOURCE = """\
import json, sys
from tatolab.runtime._engine import decode_tapped_channel_bag_frame_to_python_object

bag = decode_tapped_channel_bag_frame_to_python_object(bytes.fromhex(sys.argv[1]))
surface_id = bag.get("surface_id") if isinstance(bag, dict) else None
print(json.dumps(surface_id if isinstance(surface_id, str) else None))
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


def attach_the_projects_stream(
    tatolabd: TatolabdUnderTest, app_directory: Path, verb: str = "run"
) -> AttachedTatolabRun:
    """`tatolab <verb>` from `app_directory`, started on `tatolabd`."""
    return tatolabd.run_stream_attached(TatolabRunOfAProject(working_directory=app_directory), verb=verb)


@pytest.mark.parametrize("verb", ["run", "dev"])
def test_an_attached_stream_loads_into_the_runtime_and_a_ctrl_c_unloads_it(
    verb: str,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The MVP minute's observable half, for both verbs.

    `run` and `dev` share one attached path — `dev` only adds the load again on
    a save — so a divergence in load, listing or unload between them is a
    defect in the path itself.
    """
    app_directory = make_tatolab_project({"stream.py": STREAM_WITH_ONE_NATIVE_SOURCE})
    tatolabd = start_tatolabd()

    attached = attach_the_projects_stream(tatolabd, app_directory, verb)
    loaded = attached.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    local_api = tatolabd.local_api_client()
    running_graph = local_api.await_every_node_running(stream=STREAM_NAME)
    listed_streams = local_api.list_streams()

    assert loaded["stream_name"] == STREAM_NAME
    assert int(loaded["node_count"]) == 1
    assert Path(loaded["project_directory"]) == app_directory.resolve()
    assert listed_streams == [
        {
            "name": STREAM_NAME,
            "state": "attached",
            "project_directory": str(app_directory.resolve()),
            "node_count": 1,
        }
    ]
    assert running_graph["stream"] == STREAM_NAME
    assert [node["type"] for node in running_graph["nodes"]].count(TestPatternSource.type) == 1, (
        running_graph["nodes"]
    )
    assert_only_its_owner_can_open(tatolabd.local_api_socket_path)

    attached.interrupt()
    assert attached.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        f"`tatolab {verb}` must exit cleanly on SIGINT; standard error:\n{attached.recent_stderr()}"
    )
    tatolabd.await_stderr_containing(THE_STREAM_STOPPED_LOG_LINE, timeout=CLEAN_EXIT_TIMEOUT_SECONDS)
    assert local_api.list_streams() == [], "a Ctrl-C to `tatolab` unloads its stream"
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


def listening_tcp_socket_inodes_in_the_network_namespace_of(pid: int) -> "set[str]":
    """The inode of every TCP socket in LISTEN, IPv4 and IPv6, that `pid` can see, off Linux's `/proc`."""
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
    """Every TCP socket in LISTEN, IPv4 and IPv6, `pid` holds: by inode off
    Linux's `/proc`, by local address off `lsof` on macOS."""
    if sys.platform == "linux":
        return socket_inodes_held_by(pid) & listening_tcp_socket_inodes_in_the_network_namespace_of(pid)
    return socket_names_lsof_reports_held_by(pid, "-iTCP", "-sTCP:LISTEN")


def listening_tcp_socket_as_the_scan_names_it(tcp_listener: socket.socket) -> str:
    """`tcp_listener` as `listening_tcp_sockets_held_by` names it."""
    if sys.platform == "linux":
        return str(os.fstat(tcp_listener.fileno()).st_ino)
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


def test_neither_the_runtime_nor_tatolab_listens_on_a_tcp_socket(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """Nothing on the network can reach the runtime's control API: neither
    `tatolabd` nor the `tatolab` attached to it holds a TCP socket in LISTEN, on
    any address, loopback included.

    The same scan — Linux's `/proc`, macOS's `lsof` — has to find the TCP
    listeners the test itself holds and the local API socket's listener, and
    the runtime has to answer over it, so an empty TCP answer is the runtime's
    own and not a scan that saw nothing.
    """
    app_directory = make_tatolab_project({"stream.py": STREAM_WITH_ONE_NATIVE_SOURCE})
    tatolabd = start_tatolabd()

    attached = attach_the_projects_stream(tatolabd, app_directory)
    attached.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    graph = tatolabd.local_api_client().call_tool("graph", {"stream": STREAM_NAME})
    assert graph["stream"] == STREAM_NAME

    assert_the_tcp_listener_scan_sees_the_listeners_this_process_holds()
    assert_the_scan_sees_the_unix_socket_listener_held_by(
        tatolabd.pid, str(tatolabd.local_api_socket_path)
    )
    for process_name, pid in (("tatolabd", tatolabd.pid), ("tatolab", attached.pid)):
        listening_tcp_sockets_it_holds = listening_tcp_sockets_held_by(pid)
        assert listening_tcp_sockets_it_holds == set(), (
            f"{process_name} holds TCP sockets in LISTEN "
            f"({sorted(listening_tcp_sockets_it_holds)}); standard error ended:\n"
            f"{tatolabd.recent_stderr()}"
        )

    attached.interrupt()
    assert attached.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0


def succeeded(completed: "subprocess.CompletedProcess[str]") -> str:
    """The verb's stdout, once it exited 0 — or a failure carrying what it said."""
    assert completed.returncode == 0, (
        f"`tatolab {' '.join(map(str, completed.args[1:]))}` exited {completed.returncode}:\n"
        f"stdout: {completed.stdout}\nstderr: {completed.stderr}"
    )
    return completed.stdout


def test_every_observation_verb_reaches_an_attached_stream_through_the_local_api_socket(
    tmp_path: Path,
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    run_tatolab_observation_verb: "Callable[..., subprocess.CompletedProcess[str]]",
):
    """`tatolab streams`, `graph`, `tap`, `logs` and both forms of `exchange`,
    driven the way a user drives them: a separate process, naming the stream.
    The source is wired to a reader, since a channel is tappable only once a
    connect has wired its output."""
    app_directory = make_tatolab_project(project_files_with_helper_placed_processors(1))
    tatolabd = start_tatolabd()
    attached = attach_the_projects_stream(tatolabd, app_directory)
    attached.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)

    listed = succeeded(run_tatolab_observation_verb("streams"))
    assert listed.splitlines()[0].split() == ["NAME", "STATE", "NODES", "PROJECT"], listed
    assert re.search(rf"^{STREAM_NAME}\s+attached\s+2\s+", listed, re.MULTILINE), listed

    graph = json.loads(succeeded(run_tatolab_observation_verb("graph", "--stream", STREAM_NAME)))
    runtime_name = graph["runtime_name"]
    source_name = next(
        graph_node["name"]
        for graph_node in graph["nodes"]
        if graph_node["name"].startswith("testpatternsource")
    )
    channel = f"{runtime_name}/{source_name}/video"

    tapped = json.loads(
        succeeded(
            run_tatolab_observation_verb(
                "tap", channel, "--count", "2", "--stream", STREAM_NAME,
                timeout=OBSERVATION_VERB_TIMEOUT_SECONDS,
            )
        )
    )  # fmt: skip
    assert tapped["received"] > 0, f"no bags reached the tap over the socket: {tapped}"

    succeeded(run_tatolab_observation_verb("logs", "--stream", STREAM_NAME))

    channel_form_directory = tmp_path / "channel-form"
    channel_form_written = succeeded(
        run_tatolab_observation_verb(
            "exchange", "--channel", channel, "--count", "1",
            "--out", str(channel_form_directory), "--stream", STREAM_NAME,
            timeout=OBSERVATION_VERB_TIMEOUT_SECONDS,
        )
    ).split()  # fmt: skip
    assert len(channel_form_written) == 1, channel_form_written
    assert Path(channel_form_written[0]).read_bytes().startswith(PNG_SIGNATURE)

    id_form_directory = tmp_path / "id-form"
    id_form_attempts: "list[str]" = []
    for _ in range(SURFACE_ID_EXCHANGE_ATTEMPTS):
        tapped_for_a_surface_id = json.loads(
            succeeded(
                run_tatolab_observation_verb(
                    "tap", channel, "--count", "1", "--stream", STREAM_NAME,
                    timeout=OBSERVATION_VERB_TIMEOUT_SECONDS,
                )
            )
        )  # fmt: skip
        assert tapped_for_a_surface_id["bags"], f"no bag reached the tap: {tapped_for_a_surface_id}"
        assert tapped_for_a_surface_id["bags"][0]["hex_truncated"] is False, (
            "the tapped bag is past the tap's preview cap, so its hex cannot be decoded: "
            f"{tapped_for_a_surface_id['bags'][0]['byte_len']} bytes"
        )
        published_surface_id = run_python_with_the_lend(
            runtime_unit,
            private_machine_directories.environment,
            SURFACE_ID_OF_A_TAPPED_BAG_SOURCE,
            tapped_for_a_surface_id["bags"][0]["hex_preview"],
        )
        assert published_surface_id is not None, f"{channel} published no surface id"
        exchanged = run_tatolab_observation_verb(
            "exchange", published_surface_id, "--out", str(id_form_directory),
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

    attached.interrupt()
    assert attached.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0


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

    attached = attach_the_projects_stream(tatolabd, app_directory)
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

    attached.interrupt()
    assert attached.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, attached.recent_stderr()


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

    attached = attach_the_projects_stream(tatolabd, app_directory)
    attached.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    running_graph = tatolabd.local_api_client().await_every_node_running(stream=STREAM_NAME)
    assert [node["type"] for node in running_graph["nodes"]].count(TestPatternSource.type) == 1

    attached.interrupt()
    assert attached.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0


def the_first_streams_output(tatolabd: TatolabdUnderTest) -> str:
    """`tatolabd`'s standard error through the first stream's window stopping."""
    the_first_streams_stop = tatolabd.await_stderr_containing(
        DISPLAY_WINDOW_STOPPED, timeout=CLEAN_EXIT_TIMEOUT_SECONDS
    )
    return tatolabd.stderr_text.split(the_first_streams_stop, 1)[0] + the_first_streams_stop


def test_the_scaffolded_app_reaches_a_running_graph(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """What `tatolab new` writes must actually run, frame after frame.

    Run exactly as scaffolded — window included, which is why this is rig-only:
    `dev` has the runtime compile the scaffold's `@stream` and load the graph
    it builds, so the graph the local API renders carries the stream's name and
    the exposure it declared. A listed stream alone proves almost nothing here:
    it is listed whether or not `process()` ever succeeds, so the assertions
    that carry this test are the ones on the run's own records. `process()
    failed` catches an effect that raises every frame; the delivered-frame
    count catches an effect that is correct but so slow the demo is a
    slideshow. The meter's line is the logic half of the first minute: a
    fan-out reader that never reports is a graph that shows the picture and
    drops the rest.
    """
    app_directory = make_scaffolded_test_pattern_project(make_tatolab_project, run_tatolab)
    tatolabd = start_tatolabd()

    dev = attach_the_projects_stream(tatolabd, app_directory, "dev")
    dev.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    live_graph = tatolabd.local_api_client().call_tool("graph", {"stream": STREAM_NAME})
    assert live_graph["stream"] == STREAM_NAME, (
        f"the runtime must render the stream it was loaded as; graph was {live_graph}"
    )
    assert live_graph["exposed"] == [
        {"node": "invertingeffect", "port": "video_to_downstream", "level": "private"}
    ], f"the runtime must render the exposure the stream declared; graph was {live_graph}"
    assert {
        "testpatternsource",
        "invertingeffect",
        "brightnessmeter",
        "displaywindow",
    } <= {graph_node["name"] for graph_node in live_graph["nodes"]}

    # Long enough for the source to have driven many frames through the effect.
    time.sleep(SCAFFOLD_OBSERVATION_WINDOW_SECONDS)
    dev.interrupt()
    assert dev.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, dev.recent_stderr()

    stream_output = the_first_streams_output(tatolabd)
    assert_the_window_showed_live_video(tatolabd, stream_output, "the app `tatolab new` writes")
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
    it reaches the terminal of the user who ran `tatolab run` — ahead of the
    note that the stream loaded — without costing the start. The runtime's log
    carries the same block.
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
    tatolabd = start_tatolabd()

    tatolab_run = attach_the_projects_stream(tatolabd, app_directory)
    tatolab_run.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    tatolabd.await_stderr_containing(ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS)
    tatolab_run.interrupt()
    assert tatolab_run.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, tatolab_run.recent_stderr()

    output = tatolab_run.stderr_text
    assert f"{SCAFFOLDED_EFFECT_MODULE_PATH}:" in output and "imports `cupy`" in output, (
        f"the warning block on `tatolab run`'s standard error must name the cupy import; it "
        f"ended:\n{tatolab_run.recent_stderr()}"
    )
    assert "names the device 'cuda'" in output, (
        f"the warning block on `tatolab run`'s standard error must name the device literal; it "
        f"ended:\n{tatolab_run.recent_stderr()}"
    )
    assert output.index("cross-floor check") < output.index(f"tatolab: {STREAM_NAME} loaded"), (
        f"the warning block must come ahead of the loaded note:\n{output}"
    )
    assert "imports `cupy`" in tatolabd.stderr_text, (
        f"the runtime's log must carry the compile's warning block too; it ended:\n"
        f"{tatolabd.recent_stderr()}"
    )


def assert_the_window_showed_live_video(
    tatolabd: TatolabdUnderTest, output: str, what_ran: str
) -> None:
    """Require one stopped stream's records to show live video, not a slideshow.

    The window reports what it actually put on screen, which is the honest
    measure — an effect can be correct and still leave the demo at roughly 4
    frames a second, which is what editing the write-combined mapping in place
    through a strided view produced.
    """
    assert "process() failed" not in output, (
        f"{what_ran}: the effect raised on a live frame; standard error ended:\n"
        f"{tatolabd.recent_stderr()}"
    )
    frames_shown = DISPLAY_WINDOW_FRAME_COUNT.search(output)
    assert frames_shown, (
        f"{what_ran}: the window never reported a frame count; standard error ended:\n"
        f"{tatolabd.recent_stderr()}"
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
    fleet = attach_the_projects_stream(tatolabd, fleet_app, "dev")
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
    assert fleet.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        f"a graph of {HELPER_PLACED_PROCESSOR_COUNT} helpers must still unload cleanly"
    )
    tatolabd.await_stderr_containing(THE_STREAM_STOPPED_LOG_LINE, timeout=CLEAN_EXIT_TIMEOUT_SECONDS)


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


def node_ids_of(graph: "dict[str, Any]") -> "list[str]":
    """The ids of a live graph's nodes, which the runtime mints afresh for each load."""
    return sorted(node["id"] for node in graph["nodes"])


def test_the_edit_loop_survives_a_bad_save_and_shows_a_good_one(
    make_tatolab_project: "Callable[..., Path]",
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The MVP edit loop: `dev` running the stream again on every save.

    A save is a file write, and `dev` runs the stream again over its
    connection. The runtime compiles the save before it touches the running
    stream, so a broken save is refused, named, and leaves the same stream —
    the same load, still showing live video — running; a good one replaces it
    with the edited code. Both halves run against the same `dev` and the same
    `tatolabd` in sequence because the first is only meaningful if the second
    follows: a loop that survives a bad save by ignoring the file entirely
    would pass the first alone.
    """
    app_directory = make_scaffolded_test_pattern_project(make_tatolab_project, run_tatolab)
    effect_module = app_directory / SCAFFOLDED_EFFECT_MODULE_PATH
    last_good_effect_source = effect_module.read_text()
    tatolabd = start_tatolabd()
    local_api = tatolabd.local_api_client()

    dev = attach_the_projects_stream(tatolabd, app_directory, "dev")
    dev.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    tatolabd.await_stderr_containing(ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS)
    node_ids_before_the_bad_save = node_ids_of(
        local_api.await_every_node_running(stream=STREAM_NAME)
    )

    time.sleep(SECONDS_OF_LIVE_VIDEO_BEFORE_THE_BAD_SAVE_LANDS)
    effect_module.write_text("def process(self ctx:\n    this does not parse\n")
    dev.await_stderr_containing(DEV_LOADING_AGAIN_AFTER_A_SAVE, timeout=NODE_READY_TIMEOUT_SECONDS)
    dev.await_stderr_containing(DEV_KEPT_THE_RUNNING_STREAM, timeout=NODE_READY_TIMEOUT_SECONDS)
    time.sleep(
        SCAFFOLD_OBSERVATION_WINDOW_SECONDS - SECONDS_OF_LIVE_VIDEO_BEFORE_THE_BAD_SAVE_LANDS
    )
    assert "SyntaxError" in dev.stderr_text, (
        f"`dev` must show why the save was refused; standard error ended:\n{dev.recent_stderr()}"
    )
    assert DEV_NO_STREAM_IS_LOADED not in dev.stderr_text, dev.recent_stderr()
    assert dev.process.poll() is None, (
        f"a bad save must not take `dev` down; standard error ended:\n{dev.recent_stderr()}"
    )
    assert node_ids_of(local_api.graph(stream=STREAM_NAME)) == node_ids_before_the_bad_save, (
        "a bad save must leave the same load of the stream running"
    )
    assert [listed["state"] for listed in local_api.list_streams()] == ["attached"]
    assert DISPLAY_WINDOW_FRAME_COUNT.search(tatolabd.stderr_text) is None, (
        "the window stopped although the bad save was to leave the stream running"
    )

    effect_module.write_text(the_scaffolded_effect_edited(last_good_effect_source))
    assert_the_window_showed_live_video(
        tatolabd,
        the_first_streams_output(tatolabd),
        "the stream running when the bad save landed, through the replace",
    )
    dev.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS, occurrence=2)
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
    ), (
        "the stream that survived the bad save did not stop gracefully before the edited one "
        "started"
    )
    node_ids_after_the_good_save = node_ids_of(
        local_api.await_every_node_running(stream=STREAM_NAME)
    )
    assert set(node_ids_after_the_good_save).isdisjoint(node_ids_before_the_bad_save), (
        "a good save replaces the stream with a load of its own"
    )
    assert tatolabd.process.poll() is None, "every load of the edit loop lands on the one runtime"

    dev.interrupt()
    assert dev.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, dev.recent_stderr()


def test_a_bad_config_is_reported_without_a_launcher_traceback(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The runtime's load refuses a config its built-in does not take, naming
    the node and the setting, before the engine starts. It is still the app's
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
    tatolabd = start_tatolabd()

    attached = attach_the_projects_stream(tatolabd, app_directory)

    assert attached.await_exit(timeout=NODE_READY_TIMEOUT_SECONDS) == 1
    output = attached.stderr_text
    assert "Traceback (most recent call last)" not in output, (
        f"an engine-side failure must not arrive as a launcher traceback; output was:\n{output}"
    )
    refusal = attached.refusal()
    assert refusal is not None, f"`tatolab run` must end on the runtime's refusal; output was:\n{output}"
    assert "`tatolab.stream:TestPatternSource`" in refusal, refusal
    assert "width: invalid type" in refusal, refusal
    assert ENGINE_STARTED_LOG_LINE not in tatolabd.stderr_text
    assert tatolabd.local_api_client().list_streams() == []


@pytest.mark.parametrize("verb", ["run", "dev"])
def test_a_stream_function_that_raises_loads_nothing(
    verb: str,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """A graph that failed to build must not leave a stream in the runtime.

    `run` ends with the compile's failure; `dev` reports it and waits for the
    save that fixes it, with nothing loaded, until a Ctrl-C ends it.
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
    tatolabd = start_tatolabd()

    attached = attach_the_projects_stream(tatolabd, app_directory, verb)

    if verb == "run":
        assert attached.await_exit(timeout=NODE_READY_TIMEOUT_SECONDS) == 1, (
            f"a raising stream function must exit non-zero; standard error:\n"
            f"{attached.recent_stderr()}"
        )
    else:
        attached.await_stderr_containing(DEV_NO_STREAM_IS_LOADED, timeout=NODE_READY_TIMEOUT_SECONDS)
        attached.interrupt()
        assert attached.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, attached.recent_stderr()
    assert "ValueError: bad wiring" in attached.stderr_text, attached.recent_stderr()
    assert tatolabd.local_api_client().list_streams() == [], (
        "a stream whose function raised must never be loaded"
    )
    assert "[start] Starting the stream" not in tatolabd.stderr_text, tatolabd.recent_stderr()

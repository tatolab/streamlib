# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`tatolab open --play` / `dev` loading a project's stream into the machine's runtime, end to end.

The runtime reads each graph document of the project, runs its Python nodes in
the project's own venv — here a symlink to the suite venv, holding
`tatolab-stream` and nothing of the runtime — loads each stream and records it: the local API names it by its stream id, every
observation verb reaches it through the one socket, and it runs on once the
`tatolab` that loaded it has returned or been stopped, until `tatolab stop`.
Starting a stream initializes a GPU context, so the module needs a device.
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

import pytest

from cli_launch_projects import (
    CLEAN_EXIT_TIMEOUT_SECONDS,
    LIVE_HELPER_MARKER_NAME,
    NODE_READY_TIMEOUT_SECONDS,
    STREAM_NAME_OF_EVERY_PROJECT,
    load_the_projects_stream,
    project_files_with_helper_placed_nodes,
    stream_id_of_the_project,
    the_stream_stopped_log_line,
)
from conftest import (
    TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE,
    TatolabdUnderTest,
    a_tcp_port_free_on_loopback,
)
from runtime_process_under_test import ENGINE_STARTED_LOG_LINE
from stream_runs_on_tatolabd import (
    GRAPH_DOCUMENTS_DIRECTORY_NAME,
    graph_document_path_of,
    stream_asset_held_at,
    stream_id_of_the_stream_in,
    write_a_graph_document,
    write_the_graph_documents_the_stream_py_compiles_to,
    write_the_project_file_unless_there,
)


pytestmark = pytest.mark.requires_gpu


LOCAL_API_SOCKET_FILE_MODE = 0o600


#: The test pattern module's node type, which the suite venv installs.
TEST_PATTERN_SOURCE_NODE_TYPE = "tatolab.testpattern:TestPatternSource"

STREAM_WITH_ONE_TEST_PATTERN_SOURCE = '''\
from tatolab.stream import StreamBuilder, graph_generator
from tatolab.testpattern import TestPatternSource


@graph_generator
def main(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TestPatternSource, input_values={"width": 320, "height": 180})
'''


# How long a loaded stream's source has to write its first bag on `video`.
FIRST_BAG_WRITTEN_TIMEOUT_SECONDS = 30.0


def assert_only_its_owner_can_open(local_api_socket_path: Path) -> None:
    status = os.stat(local_api_socket_path)
    assert stat.S_ISSOCK(status.st_mode), f"{local_api_socket_path} is not a socket"
    assert stat.S_IMODE(status.st_mode) == LOCAL_API_SOCKET_FILE_MODE, (
        f"the local API socket must be {LOCAL_API_SOCKET_FILE_MODE:o}; "
        f"got {stat.S_IMODE(status.st_mode):o}"
    )
    assert status.st_uid == os.getuid()


@pytest.mark.parametrize("verb", ["open", "dev"])
def test_a_loaded_stream_runs_on_past_the_verb_that_loaded_it_until_tatolab_stop(
    verb: str,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The MVP minute's observable half, for both verbs.

    `open --play` returns once the open and playback are accepted and `dev` is ended by a Ctrl-C;
    neither ends the stream, which the runtime records and runs on until
    `tatolab stop` — so a divergence in load, listing or persistence between
    them is a defect in the load path itself.
    """
    app_directory = make_tatolab_project({"stream.py": STREAM_WITH_ONE_TEST_PATTERN_SOURCE})
    stream_id = stream_id_of_the_project(app_directory)
    tatolabd = start_tatolabd()

    stream_run = load_the_projects_stream(tatolabd, app_directory, verb)
    loaded = stream_run.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    local_api = tatolabd.local_api_client()
    running_graph = local_api.await_every_node_running(stream=stream_id)
    listed_streams = local_api.list_streams()

    assert loaded["stream_id"] == stream_id
    assert loaded["display_name"] == "main"
    project_file = write_the_project_file_unless_there(app_directory)
    stream_asset = stream_asset_held_at(
        graph_document_path_of(app_directory, STREAM_NAME_OF_EVERY_PROJECT)
    )
    assert (loaded["project"], loaded["stream_asset"]) == (project_file["project"], stream_asset)
    assert Path(loaded["project_directory"]) == app_directory.resolve()
    listed_running = {
        "stream": stream_id,
        "display_name": "main",
        "location": {
            "project_directory": str(app_directory.resolve()),
            "project": project_file["project"],
            "stream_asset": stream_asset,
        },
        "state": "running",
        "condition": {"load": "loaded", "playback": "playing", "health": "healthy", "diagnostics": []},
        "node_count": 1,
        "failed_because": None,
        "capture_devices_whose_access_was_never_asked_for": [],
        "action": None,
        "step": None,
    }
    assert listed_streams == [listed_running]
    assert running_graph["stream"] == stream_id
    assert [node["type"] for node in running_graph["nodes"]].count(TEST_PATTERN_SOURCE_NODE_TYPE) == 1, (
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
    listed_stopped = {
        **listed_running,
        "state": "stopped",
        "condition": {**listed_running["condition"], "playback": "stopped"},
    }
    assert local_api.list_streams() == [listed_stopped], (
        "`tatolab stop` holds the stream stopped with its graph and records it stopped"
    )
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
    app_directory = make_tatolab_project({"stream.py": STREAM_WITH_ONE_TEST_PATTERN_SOURCE})
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
    app_directory = make_tatolab_project(project_files_with_helper_placed_nodes(1))
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
        "ASSET",
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


def test_the_runtime_and_its_node_interpreter_share_the_runtime_directorys_iceoryx2_domain(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The stream carries two Python nodes, the test pattern and the reporter,
    so a frame reaching the reporter proves `tatolabd` and both node
    interpreters opened their nodes in one iceoryx2 domain: the runtime
    directory's, where the surface socket sits too."""
    app_directory = make_tatolab_project(project_files_with_helper_placed_nodes(1))
    tatolabd = start_tatolabd()
    runtime_directory = tatolabd.machine_directories.runtime_directory
    iceoryx2_node_details_before = iceoryx2_node_details_in(runtime_directory / "iox2")

    stream_run = load_the_projects_stream(tatolabd, app_directory)
    tatolabd.await_marker(LIVE_HELPER_MARKER_NAME, timeout=NODE_READY_TIMEOUT_SECONDS)
    new_node_details = (
        iceoryx2_node_details_in(runtime_directory / "iox2") - iceoryx2_node_details_before
    )

    assert {details.name for details in new_node_details} == {f"sl{os.getuid()}_node.details"}
    assert len(new_node_details) == 2, (
        f"each node interpreter must open its node in the runtime directory's domain, "
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
    not. The struct is built at the graph's load, after the load accepted the graph,
    so only a running graph proves it.
    """
    app_directory = make_tatolab_project(
        {
            "stream.py": (
                "from tatolab.stream import StreamBuilder, graph_generator\n"
    "from tatolab.testpattern import TestPatternSource\n"
                "\n"
                "\n"
                "@graph_generator\n"
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
    assert [node["type"] for node in running_graph["nodes"]].count(TEST_PATTERN_SOURCE_NODE_TYPE) == 1

    assert stream_run.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0


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
                "from tatolab.stream import ChannelsToRecords, StreamBuilder, graph_generator\n"
                "\n"
                "\n"
                "@graph_generator\n"
                "def main(stream_builder: StreamBuilder) -> None:\n"
                "    stream_builder.add(\n"
                '        ChannelsToRecords, config={"channel_names": 7}\n'
                "    )\n"
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
    assert "`tatolab.stream:ChannelsToRecords`" in failed_because, failed_because
    assert "channel_names: invalid type" in failed_because, failed_because
    assert ENGINE_STARTED_LOG_LINE not in tatolabd.stderr_text
    [listed] = tatolabd.local_api_client().list_streams()
    assert (listed["stream"], listed["state"]) == (failed_stream_id, "failed"), listed


STREAM_PY_WITH_TWO_STREAMS = (
    "from tatolab.stream import StreamBuilder, graph_generator\n"
    "from tatolab.testpattern import TestPatternSource\n"
    "\n"
    "\n"
    "@graph_generator\n"
    "def camera(stream_builder: StreamBuilder) -> None:\n"
    "    stream_builder.add(TestPatternSource)\n"
    "\n"
    "\n"
    "@graph_generator\n"
    "def preview(stream_builder: StreamBuilder) -> None:\n"
    "    stream_builder.add(TestPatternSource)\n"
)


#: TOML the runtime reads to declare its first-party modules, whose `requires-python` uv refuses
#: as it syncs.
PYPROJECT_THE_RUNTIME_READS_AND_UV_REFUSES = (
    '[project]\nname = "unsyncable"\nversion = "0"\n'
    'requires-python = "this is not a version specifier"\n'
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
    (unsyncable_directory / "pyproject.toml").write_text(
        PYPROJECT_THE_RUNTIME_READS_AND_UV_REFUSES, encoding="utf-8"
    )
    (unsyncable_directory / "stream.py").write_text(STREAM_WITH_ONE_TEST_PATTERN_SOURCE, encoding="utf-8")
    write_the_graph_documents_the_stream_py_compiles_to(unsyncable_directory)
    unsynced_run = client.call_tool(
        "open_project", {"project_directory": str(unsyncable_directory.resolve())}
    )
    [unsynced] = client.stream_action_once_ended(unsynced_run["action"])["streams"]
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

    app_directory = make_tatolab_project({"stream.py": STREAM_PY_WITH_TWO_STREAMS})
    project_directory = str(app_directory.resolve())
    opened = client.call_tool("open_project", {"project_directory": project_directory})
    assert client.stream_action_once_ended(opened["action"])["state"] == "done"
    run = client.call_tool("start_playback", {"project_directory": project_directory})
    loaded_streams = client.stream_action_once_ended(run["action"])["streams"]
    assert [loaded["state"] for loaded in loaded_streams] == ["running", "running"], loaded_streams
    running_ids = {loaded["stream"] for loaded in loaded_streams}
    assert (app_directory / ".tatolab" / "logs" / "project.jsonl").is_file()
    assert not (app_directory / ".streamlib").exists()

    every_record = client.call_tool("logs", {"project_directory": project_directory})["records"]
    assert {record.get("stream") for record in every_record} >= running_ids, every_record

    camera_id = stream_id_of_the_stream_in(app_directory, "camera")
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


def test_a_run_takes_in_every_graph_document_and_forgets_a_stream_whose_document_is_gone(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """`open` loads every graph document of the project, each its own stream: two
    run, and one that does not load is failed saying why; a reload once a document
    is gone unloads and forgets its stream."""
    app_directory = make_tatolab_project({"stream.py": STREAM_PY_WITH_TWO_STREAMS})
    write_a_graph_document(app_directory, "broken", '{"nodes": "not a list"}')
    tatolabd = start_tatolabd()

    def stream_id_of(stream_name: str) -> str:
        return stream_id_of_the_stream_in(app_directory, stream_name)

    stream_run = load_the_projects_stream(tatolabd, app_directory)

    ended_playback = stream_run.action_once_ended(timeout=NODE_READY_TIMEOUT_SECONDS)
    assert ended_playback["state"] == "done", ended_playback
    assert {stream["stream"]: stream["state"] for stream in ended_playback["streams"]} == {
        stream_id_of("camera"): "running",
        stream_id_of("preview"): "running",
    }, ended_playback
    open_action = stream_run.stdout_text.split(" ", 1)[0]
    assert stream_run.stdout_text == (
        f"{open_action} queued: open of {app_directory.resolve()}\n"
        "  follow it: tatolab logs -f\n"
        f"{ended_playback['action']} queued: start_playback of {app_directory.resolve()}\n"
        "  follow it: tatolab logs -f\n"
    ), stream_run.stdout_text
    failed_stream = stream_run.failed_stream()
    assert failed_stream is not None, ended_playback
    assert failed_stream[0] == stream_id_of("broken"), failed_stream
    assert "`streams/broken.graph.json` does not load" in failed_stream[1], failed_stream[1]

    graph_document_path_of(app_directory, "preview").unlink()
    reload = load_the_projects_stream(tatolabd, app_directory, verb="reload")
    assert reload.action_once_ended(timeout=NODE_READY_TIMEOUT_SECONDS)["state"] == "done"

    states = {
        listed["stream"]: listed["state"]
        for listed in tatolabd.local_api_client().list_streams()
    }
    assert states == {
        stream_id_of("camera"): "running",
        stream_id_of("broken"): "failed",
    }, states
    assert (app_directory / GRAPH_DOCUMENTS_DIRECTORY_NAME / "camera.graph.json").is_file()

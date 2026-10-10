# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The runtime directory a running `tatolabd` opens, and the one socket every `tatolab` verb reaches.

The machine has one runtime and so one local API socket, at a fixed path in
the runtime directory: no registry, no per-runtime name, nothing to discover.
`tatolabd` opens the directory owner-only and the socket for its owner alone,
its iceoryx2 domain and surface socket beside it, and holds the machine lock
for its life. A test build keeps all of it under the test's machine root, so
every check here reads `<root>/run/` — the directory the client contract places
there — with no device.
"""

from __future__ import annotations

import json
import os
import socket
import stat
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path

from conftest import PrivateMachineDirectories, TatolabdUnderTest

RUNTIME_DIRECTORY_MODE = 0o700
LOCAL_API_SOCKET_FILE_MODE = 0o600

#: The directory the retired node registry kept, which no runtime writes now.
RETIRED_NODE_REGISTRY_DIRECTORY_NAME = "nodes"

#: How every verb refuses when nothing answers at the socket, before it names the socket.
NO_RUNTIME_REFUSAL_PREFIX = "no runtime is running on this machine: nothing answers at "


def iceoryx2_node_details_in(runtime_directory: Path) -> "set[Path]":
    return set((runtime_directory / "iox2" / "nodes").glob("*/*node.details"))


def surface_sockets_in(runtime_directory: Path) -> "set[Path]":
    return set(runtime_directory.glob("surface-share-*.sock"))


def test_tatolabd_serves_its_local_api_at_the_fixed_socket_in_an_owner_only_runtime_directory(
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    run_tatolab_observation_verb: "Callable[..., subprocess.CompletedProcess[str]]",
):
    tatolabd = start_tatolabd()
    machine_directories = tatolabd.machine_directories
    runtime_directory = machine_directories.runtime_directory
    local_api_socket_path = machine_directories.local_api_socket_path

    runtime_directory_status = os.lstat(runtime_directory)
    assert stat.S_ISDIR(runtime_directory_status.st_mode)
    assert stat.S_IMODE(runtime_directory_status.st_mode) == RUNTIME_DIRECTORY_MODE
    assert runtime_directory_status.st_uid == os.getuid()
    local_api_socket_status = os.lstat(local_api_socket_path)
    assert stat.S_ISSOCK(local_api_socket_status.st_mode), f"{local_api_socket_path} is not a socket"
    assert stat.S_IMODE(local_api_socket_status.st_mode) == LOCAL_API_SOCKET_FILE_MODE
    assert local_api_socket_status.st_uid == os.getuid()
    assert not (runtime_directory / RETIRED_NODE_REGISTRY_DIRECTORY_NAME).exists(), (
        sorted(path.name for path in runtime_directory.iterdir())
    )
    assert [details.name for details in iceoryx2_node_details_in(runtime_directory)] == [
        f"sl{os.getuid()}_node.details"
    ], f"tatolabd's iceoryx2 node must be in {runtime_directory / 'iox2'}"
    if sys.platform == "linux":
        assert len(surface_sockets_in(runtime_directory)) == 1, (
            f"tatolabd's surface socket must be in {runtime_directory}"
        )

    graph = run_tatolab_observation_verb("graph")
    assert graph.returncode == 0, graph.stdout + graph.stderr
    machine_graph = json.loads(graph.stdout)
    assert machine_graph["streams"] == [], machine_graph
    assert machine_graph["runtime_name"], machine_graph
    listed = run_tatolab_observation_verb("streams")
    assert listed.returncode == 0, listed.stdout + listed.stderr
    assert listed.stdout.strip() == "No streams in this runtime."

    tatolabd.interrupt()
    assert tatolabd.await_exit() == 0, tatolabd.recent_stderr()
    assert not local_api_socket_path.exists(), "a clean stop removes the local API socket"


def test_every_verb_with_no_runtime_refuses_naming_the_socket_and_how_to_start_one(
    private_machine_directories: PrivateMachineDirectories,
    run_tatolab_observation_verb: "Callable[..., subprocess.CompletedProcess[str]]",
):
    local_api_socket_path = private_machine_directories.machine_directories.local_api_socket_path

    for verb_arguments in (("graph",), ("streams",), ("stop", "main"), ("mcp",)):
        refused = run_tatolab_observation_verb(*verb_arguments)

        assert refused.returncode == 1, (verb_arguments, refused.stdout, refused.stderr)
        assert f"{NO_RUNTIME_REFUSAL_PREFIX}{local_api_socket_path}." in refused.stderr, (
            verb_arguments,
            refused.stderr,
        )
        assert "`tatolabd` in a terminal" in refused.stderr, (verb_arguments, refused.stderr)


def test_a_second_tatolabd_is_refused_naming_the_one_holding_the_machine(
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    first_tatolabd = start_tatolabd()

    second_tatolabd = start_tatolabd(wait_until_serving=False)
    assert second_tatolabd.await_exit() == 1, second_tatolabd.recent_stderr()

    refusal = second_tatolabd.refusal()
    assert refusal is not None, second_tatolabd.recent_stderr()
    assert "another runtime holds this machine" in refusal, refusal
    assert f"uid {os.getuid()}" in refusal, refusal
    assert f"pid {first_tatolabd.pid}" in refusal, refusal
    assert "tatolabd" in refusal, refusal
    first_tatolabd.local_api_client().health()


def test_a_live_listener_at_the_local_api_socket_refuses_tatolabd_naming_the_socket(
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    private_machine_directories: PrivateMachineDirectories,
):
    """A live process already listening at the fixed socket keeps it: the
    runtime nobody could reach over its local API refuses to run, and leaves
    the listener's socket alone."""
    machine_directories = private_machine_directories.machine_directories
    machine_directories.runtime_directory.mkdir(mode=RUNTIME_DIRECTORY_MODE)
    local_api_socket_path = machine_directories.local_api_socket_path
    squatting_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    squatting_listener.bind(str(local_api_socket_path))
    squatting_listener.listen(1)
    try:
        tatolabd = start_tatolabd(wait_until_serving=False)

        assert tatolabd.await_exit() == 1, tatolabd.recent_stderr()
        refusal = tatolabd.refusal()
        assert refusal is not None, tatolabd.recent_stderr()
        assert str(local_api_socket_path) in refusal, refusal
        assert "live process" in refusal, refusal
        assert local_api_socket_path.exists(), "a refused bind must leave the live socket alone"
    finally:
        squatting_listener.close()


def test_a_stale_local_api_socket_file_is_replaced(
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    private_machine_directories: PrivateMachineDirectories,
):
    machine_directories = private_machine_directories.machine_directories
    machine_directories.runtime_directory.mkdir(mode=RUNTIME_DIRECTORY_MODE)
    local_api_socket_path = machine_directories.local_api_socket_path
    crashed_runtimes_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    crashed_runtimes_listener.bind(str(local_api_socket_path))
    crashed_runtimes_listener.close()
    assert local_api_socket_path.exists(), "a closed listener leaves its file, as a crash does"

    tatolabd = start_tatolabd()

    local_api_socket_status = os.lstat(local_api_socket_path)
    assert stat.S_ISSOCK(local_api_socket_status.st_mode)
    assert stat.S_IMODE(local_api_socket_status.st_mode) == LOCAL_API_SOCKET_FILE_MODE
    assert tatolabd.local_api_client().graph()["streams"] == []
    tatolabd.interrupt()
    assert tatolabd.await_exit() == 0, tatolabd.recent_stderr()
    assert not local_api_socket_path.exists()

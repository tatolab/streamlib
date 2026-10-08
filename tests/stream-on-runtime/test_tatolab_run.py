# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab run` starting a stream on `tatolabd`, end to end.

`tatolab` compiles the project's stream in the project's own venv — here a
symlink to the suite venv, holding `tatolab-stream` and nothing of the runtime —
and starts `tatolabd` beside itself. The stream it hosts is a node: it publishes
a registry entry naming `tatolabd`'s pid and an owner-only local API socket, and
a Ctrl-C to `tatolab` takes both away. Starting the engine initializes a GPU
context, so the module needs a device.
"""

from __future__ import annotations

import os
import re
import socket
import stat
from collections.abc import Callable
from pathlib import Path

import pytest

from conftest import PrivateRuntimeDirectories
from runtime_process_under_test import RuntimeProcessUnderTest, registry_entry_paths_in

pytestmark = pytest.mark.requires_gpu

CLEAN_EXIT_TIMEOUT_SECONDS = 60.0
LOCAL_API_SOCKET_FILE_MODE = 0o600
NODE_REGISTRY_SCHEMA_VERSION = 3

STREAM_WITH_ONE_NATIVE_SOURCE = '''\
from tatolab.stream import StreamBuilder, TestPatternSource, stream


@stream
def main(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TestPatternSource, config={"width": 320, "height": 180})
'''


def assert_only_its_owner_can_open(local_api_socket_path: Path) -> None:
    status = os.stat(local_api_socket_path)
    assert stat.S_ISSOCK(status.st_mode), f"{local_api_socket_path} is not a socket"
    assert stat.S_IMODE(status.st_mode) == LOCAL_API_SOCKET_FILE_MODE, (
        f"the local API socket must be {LOCAL_API_SOCKET_FILE_MODE:o}; "
        f"got {stat.S_IMODE(status.st_mode):o}"
    )
    assert status.st_uid == os.getuid()


def test_a_launched_app_registers_as_a_node_and_tears_down(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolab: "Callable[..., RuntimeProcessUnderTest]",
    private_runtime_directories: PrivateRuntimeDirectories,
):
    app_directory = make_tatolab_project({"stream.py": STREAM_WITH_ONE_NATIVE_SOURCE})
    streamlib_runtime_directory = private_runtime_directories.streamlib_runtime_directory

    tatolab = start_tatolab("run", working_directory=app_directory)
    registry_entry_path = tatolab.registry_entry_path()
    entry = tatolab.registry_entry()

    assert entry["pid"] != tatolab.pid, "the entry names tatolabd, not the tatolab that started it"
    assert entry["pid"] in tatolab.hosting_tatolabd_process_ids()
    assert entry["schema_version"] == NODE_REGISTRY_SCHEMA_VERSION
    local_api_socket_path = Path(entry["local_api_socket_path"])
    assert local_api_socket_path == (
        streamlib_runtime_directory / f"local-api-{entry['runtime_id']}.sock"
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

    tatolab.interrupt()
    assert tatolab.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        f"`tatolab run` must exit cleanly on SIGINT; standard error:\n{tatolab.recent_stderr()}"
    )
    assert registry_entry_path not in registry_entry_paths_in(streamlib_runtime_directory), (
        "clean teardown must remove the node-registry entry"
    )
    assert not local_api_socket_path.exists(), "clean teardown must remove the local API socket"

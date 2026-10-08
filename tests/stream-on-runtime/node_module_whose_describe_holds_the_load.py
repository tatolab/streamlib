# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A node module whose import, in the interpreter describing it, parks the load until released.

The module connects back over a Unix socket and waits for one byte, bounded so
a test that never releases it fails the describe rather than hanging. While it
is parked, `tatolabd` is mid-load: alive, its graph not yet loaded.
"""

from __future__ import annotations

import contextlib
import shutil
import socket
import tempfile
from pathlib import Path

#: How long the parked import waits for its release, and the test for the park.
HELD_DESCRIBE_DEADLINE_SECONDS = 20.0

#: The module the held one's node classes are copied from.
DESCRIBED_NODE_SOURCE = Path(__file__).with_name("runtime_load_nodes.py")


class NodeModuleWhoseDescribeHoldsTheLoad:
    """`runtime_load_nodes.py` as `name` in a project directory of its own, its describe parked."""

    def __init__(self, project_directory: Path, held_module_name: str) -> None:
        self.name = held_module_name
        self.project_directory = project_directory
        # Under `/tmp` and short: a Unix socket path is capped at 108 bytes.
        self._rendezvous_directory = Path(tempfile.mkdtemp(prefix="sl-held-load-", dir="/tmp"))
        rendezvous_socket_path = self._rendezvous_directory / "held-load.sock"
        self._listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._listener.bind(str(rendezvous_socket_path))
        self._listener.listen(1)
        self._listener.settimeout(HELD_DESCRIBE_DEADLINE_SECONDS)
        self._describing_interpreter_connection: "socket.socket | None" = None
        (project_directory / f"{held_module_name}.py").write_text(
            "import socket\n"
            "_held_load = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n"
            f"_held_load.settimeout({HELD_DESCRIBE_DEADLINE_SECONDS})\n"
            f"_held_load.connect({str(rendezvous_socket_path)!r})\n"
            "_held_load.recv(1)\n"
            "_held_load.close()\n" + DESCRIBED_NODE_SOURCE.read_text()
        )

    def wait_until_the_load_reaches_the_import(self) -> bool:
        """Whether the describing interpreter reached the import within the deadline."""
        try:
            self._describing_interpreter_connection, _ = self._listener.accept()
        except TimeoutError:
            return False
        return True

    def release_the_load(self) -> None:
        """Let the parked import finish."""
        if self._describing_interpreter_connection is not None:
            # A describing interpreter already killed has nothing left to release.
            with contextlib.suppress(BrokenPipeError, ConnectionResetError):
                self._describing_interpreter_connection.sendall(b"r")
            self._describing_interpreter_connection.close()
            self._describing_interpreter_connection = None

    def close(self) -> None:
        """Release the load and remove the rendezvous socket."""
        self.release_the_load()
        self._listener.close()
        shutil.rmtree(self._rendezvous_directory, ignore_errors=True)

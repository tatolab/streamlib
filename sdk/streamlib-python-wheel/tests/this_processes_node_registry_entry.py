# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The node registry entry the running app process published for itself."""

import os

from streamlib._control_plane_client import LocalApiSocket
from streamlib._node_registry import live_nodes


def this_processes_local_api_socket() -> LocalApiSocket:
    """This run's own local API socket, found by pid.

    By pid rather than by "the only live node": another test's app may be up at
    the same time, and this must never read that one's graph.
    """
    for node in live_nodes():
        if node.pid == os.getpid():
            return LocalApiSocket(node.local_api_socket_path)
    raise RuntimeError("this run published no node registry entry")

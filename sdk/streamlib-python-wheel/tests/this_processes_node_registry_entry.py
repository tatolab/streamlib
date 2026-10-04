# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The node registry entry the running app process published for itself."""

import os

from streamlib._node_registry import live_nodes


def this_processes_control_url() -> str:
    """This run's own control plane, found by pid.

    By pid rather than by "the only live node": another test's app may be up at
    the same time, and this must never read that one's graph.
    """
    for node in live_nodes():
        if node.pid == os.getpid():
            return node.control_url
    raise RuntimeError("this run published no node registry entry")

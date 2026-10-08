# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Print the runtime_id of the live registered node a fixture launched.

Usage: PYTHONPATH=<lend> python runtime_id_of_launched_node.py <launched pid>

The node is a `tatolabd`; the launched pid is that process or one of its
ancestors — `tatolab run`, or a wrapper (`timeout`) around it. Exits 1 unless
exactly one live registry entry matches, so a caller polls until the node has
registered and its local API socket answers.
"""

import subprocess
import sys

from tatolab.runtime._node_registry import live_nodes

# `timeout` -> `tatolab run` -> `tatolabd` is the deepest chain a fixture launches.
DEEPEST_LAUNCH_CHAIN = 3


def parent_pid_of(pid: int) -> int:
    ps = subprocess.run(
        ["ps", "-o", "ppid=", "-p", str(pid)], capture_output=True, text=True
    )
    return int(ps.stdout.strip() or 0)


def launch_chain_of(node_pid: int) -> "list[int]":
    """The node's pid and its ancestors, nearest first, as far as a launch reaches."""
    chain = [node_pid]
    while len(chain) < DEEPEST_LAUNCH_CHAIN + 1 and chain[-1] > 1:
        chain.append(parent_pid_of(chain[-1]))
    return chain


def main() -> int:
    launched_pid = int(sys.argv[1])
    matching_runtime_ids = [
        node.runtime_id for node in live_nodes() if launched_pid in launch_chain_of(node.pid)
    ]
    if len(matching_runtime_ids) != 1:
        return 1
    print(matching_runtime_ids[0])
    return 0


if __name__ == "__main__":
    sys.exit(main())

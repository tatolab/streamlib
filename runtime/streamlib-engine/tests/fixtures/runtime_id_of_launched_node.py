# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Print the runtime_id of the live registered node a fixture launched.

Usage: python runtime_id_of_launched_node.py <tatolab executable> <launched pid>

The node is read off `tatolab nodes`. It is a `tatolabd`; the launched pid is
that process or one of its ancestors — `tatolab run`, or a wrapper (`timeout`)
around it. Exits 1 unless exactly one live row matches, so a caller polls until
the node has registered and its local API socket answers.
"""

import re
import subprocess
import sys

# `timeout` -> `tatolab run` -> `tatolabd` is the deepest chain a fixture launches.
DEEPEST_LAUNCH_CHAIN = 3

# `tatolab nodes` pads its columns apart with two or more spaces; the last one,
# HINT, may carry single spaces of its own.
NODE_TABLE_COLUMN_SEPARATOR = re.compile(r" {2,}")
NODE_TABLE_COLUMN_COUNT = 6
NODE_TABLE_FIRST_COLUMN_NAME = "RUNTIME_NAME"


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


def live_nodes_in_the_table(node_table: str) -> "list[tuple[str, int]]":
    """`(runtime_id, pid)` of every row whose ALIVE? column says yes."""
    lines = node_table.splitlines()
    if not lines or not lines[0].startswith(NODE_TABLE_FIRST_COLUMN_NAME):
        # The one-line message `nodes` prints over an empty registry.
        return []
    live_nodes = []
    for row in lines[1:]:
        columns = NODE_TABLE_COLUMN_SEPARATOR.split(row.strip(), NODE_TABLE_COLUMN_COUNT - 1)
        if len(columns) < NODE_TABLE_COLUMN_COUNT - 1:
            continue
        runtime_id, pid, alive = columns[1], columns[3], columns[4]
        if alive == "yes" and pid.isdigit():
            live_nodes.append((runtime_id, int(pid)))
    return live_nodes


def main() -> int:
    tatolab_executable, launched_pid = sys.argv[1], int(sys.argv[2])
    listed = subprocess.run(
        [tatolab_executable, "nodes"], capture_output=True, text=True
    )
    if listed.returncode != 0:
        sys.stderr.write(listed.stderr)
        return 1
    matching_runtime_ids = [
        runtime_id
        for runtime_id, node_pid in live_nodes_in_the_table(listed.stdout)
        if launched_pid in launch_chain_of(node_pid)
    ]
    if len(matching_runtime_ids) != 1:
        return 1
    print(matching_runtime_ids[0])
    return 0


if __name__ == "__main__":
    sys.exit(main())

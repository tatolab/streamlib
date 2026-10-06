"""Print the runtime_id of the live registered node a fixture launched.

Usage: python runtime_id_of_launched_node.py <launched pid>

The launched pid is the node itself, or a wrapper (`timeout`) whose direct child
it is. Exits 1 unless exactly one live registry entry matches, so a caller polls
until the node has registered and its local API socket answers.
"""

import subprocess
import sys

from streamlib._node_registry import live_nodes


def parent_pid_of(pid: int) -> int:
    ps = subprocess.run(
        ["ps", "-o", "ppid=", "-p", str(pid)], capture_output=True, text=True
    )
    return int(ps.stdout.strip() or 0)


def main() -> int:
    launched_pid = int(sys.argv[1])
    matching_runtime_ids = [
        node.runtime_id
        for node in live_nodes()
        if launched_pid in (node.pid, parent_pid_of(node.pid))
    ]
    if len(matching_runtime_ids) != 1:
        return 1
    print(matching_runtime_ids[0])
    return 0


if __name__ == "__main__":
    sys.exit(main())

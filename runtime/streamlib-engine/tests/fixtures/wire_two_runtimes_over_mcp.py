#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""Ask one node to wire two *other* runtimes together, through MCP `connect`.

Both ends are named on the mesh, so the node this drives is neither of them:
it sends a link request to the runtime that owns the input, which is the only
way a third-party wiring is made. Reports the tool's own answer — which carries
`link_request_id` rather than `link_id`, because the link is the other
runtime's to make — as one JSON line on stdout.

Used by `verify_cross_runtime_link_requests.sh`; a separate file rather than a
heredoc so the JSON-RPC shape is readable, and standalone (no `streamlib`
import) so it drives whatever node it is pointed at.
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.error
import urllib.request

#: How long one MCP call may take. `connect` returns without waiting on the
#: mesh, so this only has to outlast the HTTP round trip.
HOW_LONG_ONE_CALL_MAY_TAKE_SECONDS = 30


def call_one_mcp_tool(url: str, tool_name: str, arguments: dict) -> dict:
    """Call `tool_name` on the node at `url`, returning its parsed result."""
    request = urllib.request.Request(
        f"{url.rstrip('/')}/mcp",
        data=json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": tool_name, "arguments": arguments},
            }
        ).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(
        request, timeout=HOW_LONG_ONE_CALL_MAY_TAKE_SECONDS
    ) as response:
        answered = json.load(response)

    if "error" in answered:
        raise SystemExit(f"the node refused the call: {answered['error']}")
    result = answered.get("result", {})
    text = result.get("content", [{}])[0].get("text", "")
    if result.get("isError"):
        raise SystemExit(f"{tool_name} was refused: {text}")
    return json.loads(text)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True, help="the wiring node's control plane")
    parser.add_argument("--from-runtime", required=True)
    parser.add_argument("--from-display-name", required=True)
    parser.add_argument("--from-port", required=True)
    parser.add_argument("--to-runtime", required=True)
    parser.add_argument("--to-display-name", required=True)
    parser.add_argument("--to-port", required=True)
    asked = parser.parse_args()

    try:
        answered = call_one_mcp_tool(
            asked.url,
            "connect",
            {
                "from_runtime_name": asked.from_runtime,
                "from_processor_display_name": asked.from_display_name,
                "from_port": asked.from_port,
                "to_runtime_name": asked.to_runtime,
                "to_processor_display_name": asked.to_display_name,
                "to_port": asked.to_port,
            },
        )
    except urllib.error.URLError as unreachable:
        print(f"the wiring node at {asked.url} could not be reached: {unreachable}", file=sys.stderr)
        return 3

    print(json.dumps(answered))
    return 0


if __name__ == "__main__":
    sys.exit(main())

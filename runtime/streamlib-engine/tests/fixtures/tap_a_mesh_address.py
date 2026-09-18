#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""Tap a port on another runtime, by its mesh address, through MCP.

The channel a remote link's bags land on is hashed from the address, so the
address is the only thing a caller can name it by — which is what this asks
`tap` for. Reports the sample as one JSON line on stdout.

Used by `verify_cross_runtime_link.sh`; kept a separate file rather than a
heredoc so the JSON-RPC shape is readable and reusable.
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.error
import urllib.request

#: How long one MCP call may take. A tap collects over its own bounded window
#: inside the node, so this only has to outlast that.
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
    ) as answered:
        answer = json.loads(answered.read())
    if "error" in answer:
        raise SystemExit(f"the node refused `{tool_name}`: {answer['error']}")
    content = answer.get("result", {}).get("content", [])
    if not content:
        raise SystemExit(f"the node answered `{tool_name}` with no content")
    return json.loads(content[0]["text"])


def main() -> None:
    parsed = argparse.ArgumentParser(description=__doc__)
    parsed.add_argument("--url", required=True, help="the node's control plane")
    parsed.add_argument(
        "--address",
        required=True,
        help="the port's mesh address, <runtime name>/<display name>/<port>",
    )
    parsed.add_argument("--count", type=int, default=8, help="bags to collect")
    arguments = parsed.parse_args()

    try:
        tapped = call_one_mcp_tool(
            arguments.url,
            "tap",
            {"channel": arguments.address, "count": arguments.count},
        )
    except (urllib.error.URLError, OSError) as unreachable:
        raise SystemExit(f"the node at {arguments.url} is unreachable: {unreachable}")

    # The bags themselves are not this fixture's subject — that a remote link
    # carries anything at all is. Reported by count and by the stamp each
    # arrived under, which is the producer's and crossed unchanged.
    print(
        json.dumps(
            {
                "channel": tapped.get("channel"),
                "requested": tapped.get("requested"),
                "bags": tapped.get("received", 0),
                "dropped_bags": tapped.get("dropped_bags", 0),
            }
        )
    )
    sys.stdout.flush()


if __name__ == "__main__":
    main()

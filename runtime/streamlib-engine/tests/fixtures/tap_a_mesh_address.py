#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""Tap a channel — a port's mesh address, or a local one — through MCP.

The channel a remote link's bags land on is hashed from the address, so the
address is the only thing a caller can name it by — which is what this asks
`tap` for. A local channel name works the same way, which is how the sending
end of a mesh link is read. Reports the sample as one JSON line on stdout.

With `--report-surface-id` it also reports the top-level `surface_id` of the
first bag that names one: the frame-carrying arm needs each end's id to
exchange it on that end's own node, and the two must differ.

Used by `verify_cross_runtime_link.sh` and `verify_cross_runtime_frame.sh`;
kept a separate file rather than a heredoc so the JSON-RPC shape is readable
and reusable.
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
    said = content[0].get("text", "")
    try:
        return json.loads(said)
    except json.JSONDecodeError:
        # A refused tool answers in prose, not JSON. Passed on as it was said,
        # because that sentence is the whole diagnosis.
        raise SystemExit(f"the node refused `{tool_name}`: {said}")


#: What `tap` hands back ahead of each bag: the engine's frame header, whose
#: width is a pinned wire contract (`streamlib-ipc-types`, `FRAME_HEADER_SIZE`).
#: The bag itself begins after it.
FRAME_HEADER_BYTES = 76


def the_first_surface_id_named_by(bags: list) -> str | None:
    """The top-level `surface_id` of the first bag carrying one, or `None`.

    A truncated bag is read past rather than guessed at: `tap` caps how much
    of each bag it hands back, and a partial map is not one this can decode.
    """
    try:
        import msgpack
    except ImportError:
        raise SystemExit(
            "reading a bag's surface_id needs msgpack; install it with "
            "`pip install msgpack`"
        )
    for bag in bags:
        if bag.get("hex_truncated"):
            continue
        framed = bytes.fromhex(bag["hex_preview"])
        if len(framed) <= FRAME_HEADER_BYTES:
            continue
        try:
            decoded = msgpack.unpackb(framed[FRAME_HEADER_BYTES:], raw=False)
        except (ValueError, msgpack.exceptions.UnpackException):
            continue
        if isinstance(decoded, dict) and isinstance(decoded.get("surface_id"), str):
            return decoded["surface_id"]
    return None


def main() -> None:
    parsed = argparse.ArgumentParser(description=__doc__)
    parsed.add_argument("--url", required=True, help="the node's control plane")
    parsed.add_argument(
        "--address",
        required=True,
        help="the port's mesh address, <runtime name>/<display name>/<port>",
    )
    parsed.add_argument("--count", type=int, default=8, help="bags to collect")
    parsed.add_argument(
        "--report-surface-id",
        action="store_true",
        help="also report the top-level surface_id of the first bag naming one",
    )
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
    reported = {
        "channel": tapped.get("channel"),
        "requested": tapped.get("requested"),
        "bags": tapped.get("received", 0),
        "dropped_bags": tapped.get("dropped_bags", 0),
    }
    if arguments.report_surface_id:
        reported["surface_id"] = the_first_surface_id_named_by(tapped.get("bags", []))
    print(json.dumps(reported))
    sys.stdout.flush()


if __name__ == "__main__":
    main()

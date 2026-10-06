#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Verify what one audio node published, off its own output port.
#
# Sister to the loopback fixture, and the one that answers the question a PR
# usually has: the loopback proves the rig when the engine will not build, this
# proves a processor when it does. No second device, no downstream consumer
# doing the checking — the tap reads what the source put on the wire.
#
# Usage:
#   ./verify_audio_channel.sh <node-name> [--node RUNTIME_NAME_OR_ID]
#                             [--count N] [--port NAME]
#                             [--expect-frame-not-restamped]
#
# `<node-name>` is the node's `name` as `streamlib graph` lists it. `--node`
# picks the running runtime the way the CLI's own `--node` does; without it the
# sole live runtime is the one read. `--port`
# names which output to tap. Without it the node must declare exactly one,
# because guessing at a node that declares several would tap whichever the
# graph happened to list first.
#
# `--expect-frame-not-restamped` requires the transport frame's timestamp to
# match the block's own, which a capture built-in publishes and a producer that
# stamps at publication does not — so it is asked for rather than assumed.
#
# Assumes a node is already running and hosting its control plane. Exit status
# is the verdict; stdout is the report JSON, progress is on stderr.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PYTHON="${PYTHON:-python3}"

NODE_NAME="${1:?usage: verify_audio_channel.sh <node-name> [--node RUNTIME_NAME_OR_ID] [--count N]}"
shift
RUNTIME_NAME_OR_ID=""
BAG_COUNT=8
OUTPUT_PORT=""
EXPECT_FRAME_NOT_RESTAMPED=""
while [ $# -gt 0 ]; do
    case "$1" in
        --node) RUNTIME_NAME_OR_ID="$2"; shift 2 ;;
        --count) BAG_COUNT="$2"; shift 2 ;;
        --port) OUTPUT_PORT="$2"; shift 2 ;;
        --expect-frame-not-restamped) EXPECT_FRAME_NOT_RESTAMPED="--expect-frame-not-restamped"; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

# Spelled in full rather than `mktemp -t`, which BSD reads as a prefix to
# suffix under $TMPDIR — so the directory is where the skill looks on both.
TEMPORARY_DIRECTORY="${TMPDIR:-/tmp}"
OUTPUT_DIR="$(mktemp -d "${TEMPORARY_DIRECTORY%/}/streamlib-audio-channel-XXXXXX")"

# The channel is the port's address, `<runtime_name>/<node>/<port>`, with this
# runtime's own top-level `runtime_name`.
# Read into a variable rather than fed to `$(...)` as a heredoc: macOS's bash
# 3.2 parses a heredoc inside a command substitution for quotes, and the
# apostrophes below end the script there.
read -r -d '' CHANNEL_RESOLVING_PROGRAM <<'PY'
import json, sys

# The engine's own resolver and client, so this cannot drift from the node and
# socket `streamlib graph --node` actually drives.
from streamlib._control_plane_client import (
    ControlPlaneError,
    call_tool,
    resolve_control_plane_endpoint,
)

runtime_name_or_id, wanted, requested_port = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    endpoint = resolve_control_plane_endpoint(None, runtime_name_or_id or None)
    graph = json.loads(call_tool(endpoint, "graph", {}))
except ControlPlaneError as control_plane_error:
    sys.exit(str(control_plane_error))
for node in graph["nodes"]:
    if node["name"] != wanted:
        continue
    declared = [output["name"] for output in node["ports"]["outputs"]]
    if not declared:
        sys.exit(f"{wanted} declares no output port")
    if requested_port:
        if requested_port not in declared:
            sys.exit(
                f"{wanted} declares no output named {requested_port!r}; "
                f"it declares {', '.join(declared)}"
            )
        port = requested_port
    elif len(declared) > 1:
        # Refused rather than guessed: taking the first would tap whichever the
        # graph happened to list first, and be right by luck.
        sys.exit(
            f"{wanted} declares {len(declared)} outputs ({', '.join(declared)}); "
            f"name one with --port"
        )
    else:
        port = declared[0]
    print(f"{graph['runtime_name']}/{node['name']}/{port}")
    break
else:
    sys.exit(f"no node named {wanted} in the running graph")
PY
CHANNEL="$("$PYTHON" -c "$CHANNEL_RESOLVING_PROGRAM" \
    "$RUNTIME_NAME_OR_ID" "$NODE_NAME" "$OUTPUT_PORT")" || exit 1

RUNTIME_SELECTION=()
if [ -n "$RUNTIME_NAME_OR_ID" ]; then
    RUNTIME_SELECTION=(--node "$RUNTIME_NAME_OR_ID")
fi

echo "tapping $CHANNEL for $BAG_COUNT bags" >&2
if ! "$PYTHON" -m streamlib.cli tap "$CHANNEL" --count "$BAG_COUNT" \
    ${RUNTIME_SELECTION[@]+"${RUNTIME_SELECTION[@]}"} \
    > "$OUTPUT_DIR/tapped.json" 2>"$OUTPUT_DIR/tap.err"; then
    cat "$OUTPUT_DIR/tap.err" >&2
    exit 1
fi

# shellcheck disable=SC2086  # deliberately unquoted: empty means "not asked for"
"$PYTHON" "$HERE/tap_audio_channel.py" "$OUTPUT_DIR/tapped.json" \
    --waveform "$OUTPUT_DIR/published.wav" $EXPECT_FRAME_NOT_RESTAMPED \
    | tee "$OUTPUT_DIR/report.json"
VERDICT=${PIPESTATUS[0]}

echo "artifacts: $OUTPUT_DIR" >&2
exit "$VERDICT"

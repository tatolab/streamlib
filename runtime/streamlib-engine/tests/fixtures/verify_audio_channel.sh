#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Verify what one audio processor published, off its own output port.
#
# Sister to the loopback fixture, and the one that answers the question a PR
# usually has: the loopback proves the rig when the engine will not build, this
# proves a processor when it does. No second device, no downstream consumer
# doing the checking — the tap reads what the source put on the wire.
#
# Usage:
#   ./verify_audio_channel.sh <processor-display-name> [--url URL] [--count N]
#                             [--expect-frame-not-restamped]
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

PROCESSOR="${1:?usage: verify_audio_channel.sh <processor-display-name> [--url URL] [--count N]}"
shift
CONTROL_URL="http://127.0.0.1:9000"
BAG_COUNT=8
EXPECT_FRAME_NOT_RESTAMPED=""
while [ $# -gt 0 ]; do
    case "$1" in
        --url) CONTROL_URL="$2"; shift 2 ;;
        --count) BAG_COUNT="$2"; shift 2 ;;
        --expect-frame-not-restamped) EXPECT_FRAME_NOT_RESTAMPED="--expect-frame-not-restamped"; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

OUTPUT_DIR="$(mktemp -d -t streamlib-audio-channel-XXXXXX)"

# The channel name is the source's processor id lowercased, then its port —
# copying the id out of `graph` verbatim gets "no tappable channel named".
CHANNEL="$("$PYTHON" - "$CONTROL_URL" "$PROCESSOR" <<'PY'
import json, sys

# The engine's own client rather than a hand-written URL, so this cannot drift
# from the endpoint `streamlib graph` actually drives.
from streamlib._control_plane_client import call_tool

control_url, wanted = sys.argv[1], sys.argv[2]
graph = json.loads(call_tool(control_url, "graph", {}))
for node in graph["nodes"]:
    if node["display_name"] != wanted:
        continue
    outputs = node["ports"]["outputs"]
    if not outputs:
        sys.exit(f"{wanted} declares no output port")
    print(f"{node['id'].lower()}/{outputs[0]['name']}")
    break
else:
    sys.exit(f"no processor named {wanted} in the running graph")
PY
)" || exit 1

echo "tapping $CHANNEL for $BAG_COUNT bags" >&2
if ! "$PYTHON" -m streamlib.cli tap "$CHANNEL" --count "$BAG_COUNT" \
    --url "$CONTROL_URL" > "$OUTPUT_DIR/tapped.json" 2>"$OUTPUT_DIR/tap.err"; then
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

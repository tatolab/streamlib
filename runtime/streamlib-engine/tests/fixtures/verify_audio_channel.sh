#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Verify what one audio node published, off its own output port.
#
# Sister to the loopback fixture, and the one that answers the question a PR
# usually has: the loopback proves the rig when the engine will not build, this
# proves a processor when it does. No second device, no downstream consumer
# doing the checking — the port is read the way any user reads it, as a track
# at the machine's local relay, with `tatolab-read-track`.
#
# Usage:
#   ./verify_audio_channel.sh <node-name> [--stream STREAM]
#                             [--count N] [--port NAME]
#                             [--expect-the-track-stamped-with-the-block]
#
# `<node-name>` is the node's `name` as `tatolab graph` lists it. `--stream`
# names the loaded stream it belongs to, handed to `tatolab`'s own `--stream`;
# without it the runtime must hold exactly one stream, which is read. `--port`
# names which output to read. Without it the node must declare exactly one,
# because guessing at a node that declares several would read whichever the
# graph happened to list first. A port the stream does not expose is exposed
# private for the read and made internal again after it.
#
# `--expect-the-track-stamped-with-the-block` requires each object's stamp — the
# port's own write stamp — to match its block's own, which a capture built-in
# publishes and a producer that stamps at publication does not — so it is asked
# for rather than assumed.
#
# Assumes the runtime is already running with the stream loaded. Exit status
# is the verdict; stdout is the report JSON, progress is on stderr. The graph is
# read through the runtime unit's `tatolab`, the machine's name and its exposed
# ports through its loopback HTTP (see fixture_runtime_unit.sh).
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=fixture_runtime_unit.sh
. "$HERE/fixture_runtime_unit.sh"

require_the_runtime_unit
require_the_fixture_venv

NODE_NAME="${1:?usage: verify_audio_channel.sh <node-name> [--stream STREAM] [--count N]}"
shift
STREAM_NAME=""
OBJECT_COUNT=8
OUTPUT_PORT=""
EXPECT_THE_TRACK_STAMPED_WITH_THE_BLOCK=""
while [ $# -gt 0 ]; do
    case "$1" in
        --stream) STREAM_NAME="$2"; shift 2 ;;
        --count) OBJECT_COUNT="$2"; shift 2 ;;
        --port) OUTPUT_PORT="$2"; shift 2 ;;
        --expect-the-track-stamped-with-the-block)
            EXPECT_THE_TRACK_STAMPED_WITH_THE_BLOCK="--expect-the-track-stamped-with-the-block"
            shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
require_tatolab_read_track

# Spelled in full rather than `mktemp -t`, which BSD reads as a prefix to
# suffix under $TMPDIR — so the directory is where the skill looks on both.
TEMPORARY_DIRECTORY="${TMPDIR:-/tmp}"
OUTPUT_DIR="$(mktemp -d "${TEMPORARY_DIRECTORY%/}/streamlib-audio-channel-XXXXXX")"

if [ -z "$STREAM_NAME" ] && ! STREAM_NAME="$(name_of_the_sole_stream_the_runtime_holds)"; then
    echo "no --stream, and the runtime at this user's local API socket does not hold exactly" \
        "one stream — name it with --stream:" >&2
    tatolab_observation_verb streams >&2
    exit 1
fi

# The port's track is `<node>/<port>` of the broadcast `<machine>/<stream>.hang`,
# read off the stream's own graph.
# Read into a variable rather than fed to `$(...)` as a heredoc: macOS's bash
# 3.2 parses a heredoc inside a command substitution for quotes, and the
# apostrophes below end the script there.
read -r -d '' CHANNEL_RESOLVING_PROGRAM <<'PY'
import json, sys

wanted, requested_port = sys.argv[1], sys.argv[2]
graph_text = sys.stdin.read()
if not graph_text.strip():
    # `tatolab graph` failed, and said why on stderr.
    sys.exit(1)
graph = json.loads(graph_text)
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
        # Refused rather than guessed: taking the first would read whichever the
        # graph happened to list first, and be right by luck.
        sys.exit(
            f"{wanted} declares {len(declared)} outputs ({', '.join(declared)}); "
            f"name one with --port"
        )
    else:
        port = declared[0]
    print(graph["stream"], node["name"], port)
    break
else:
    sys.exit(f"no node named {wanted} in the running graph")
PY
read -r STREAM_NAME NODE_NAME OUTPUT_PORT < <(tatolab_observation_verb graph --stream "$STREAM_NAME" \
    | "$FIXTURE_PYTHON" -c "$CHANNEL_RESOLVING_PROGRAM" "$NODE_NAME" "$OUTPUT_PORT") || exit 1
[ -n "$OUTPUT_PORT" ] || exit 1

MACHINE_NAME="$(the_machine_name_the_loopback_listing_names)" || exit 1
if ! the_loopback_listing_exposes "$STREAM_NAME" "$NODE_NAME" "$OUTPUT_PORT"; then
    echo "exposing $STREAM_NAME/$NODE_NAME/$OUTPUT_PORT private for the read" >&2
    tatolab_observation_verb expose "$STREAM_NAME" "$NODE_NAME" "$OUTPUT_PORT" >&2 || exit 1
    trap 'tatolab_observation_verb expose --remove "$STREAM_NAME" "$NODE_NAME" "$OUTPUT_PORT" >/dev/null 2>&1' EXIT
fi

BROADCAST="$MACHINE_NAME/$STREAM_NAME.hang"
TRACK="$NODE_NAME/$OUTPUT_PORT"
echo "reading $OBJECT_COUNT objects of $TRACK in $BROADCAST" >&2
if ! read_the_track_at_the_local_relay "$BROADCAST" "$TRACK" "$OBJECT_COUNT" \
    "$OUTPUT_DIR/track" 2>"$OUTPUT_DIR/read_track.err"; then
    cat "$OUTPUT_DIR/read_track.err" >&2
    exit 1
fi

# shellcheck disable=SC2086  # deliberately unquoted: empty means "not asked for"
"$FIXTURE_PYTHON" "$HERE/audio_track_dump_check.py" "$OUTPUT_DIR/track" \
    --waveform "$OUTPUT_DIR/published.wav" $EXPECT_THE_TRACK_STAMPED_WITH_THE_BLOCK \
    | tee "$OUTPUT_DIR/report.json"
VERDICT=${PIPESTATUS[0]}

echo "artifacts: $OUTPUT_DIR" >&2
exit "$VERDICT"

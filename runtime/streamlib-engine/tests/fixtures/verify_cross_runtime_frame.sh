#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# The frame-carrying rig arm: a video frame crosses the mesh into a surface of
# the reading runtime's own.
#
# A bag's top-level `surface_id` names a frame in the sending machine's pools,
# so nothing about it means anything on another machine. What crosses is the
# picture: the sender copies the frame's pixels out, and the reader mints a
# local surface, writes them in, and hands the bag on naming that one. Both
# ends need a GPU, so this is rig-only by construction.
#
# What it reads:
#   1. The link is `wired` — as the audio arm already proves, but for a port
#      whose every bag names a surface.
#   2. The two ends name DIFFERENT surface ids, and each resolves only on its
#      own node. An id that crossed verbatim would fail here.
#   3. Exchanging each id on its own node gives byte-identical pixels. RGBA
#      crosses as RGBA, so the bar is byte-exact and not a PSNR floor.
#   4. Neither runtime's log carries a GPU or processor failure, and both exit
#      cleanly on SIGTERM. The reader's log does carry one deliberate `500`:
#      the probe at (2) asks it to resolve the sender's id, and it must not.
#
# Usage:
#   ./verify_cross_runtime_frame.sh [output_dir]
#
# Exit status is the verdict: 0 pass, 1 fail, 3 cannot run. Progress and both
# runtimes' logs go to stderr and to output_dir; the report JSON is the only
# thing on stdout.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$HERE/../../../.." && pwd)"
PYTHON="${PYTHON:-python3}"
OUTPUT_DIR="${1:-$(mktemp -d -t streamlib-cross-runtime-frame-XXXXXX)}"
mkdir -p "$OUTPUT_DIR"

MESH_NAME="xfrig-$$"
SOURCE_RUNTIME_NAME="xfrig-source-$$"
READER_RUNTIME_NAME="xfrig-reader-$$"

# Both ports in one go, holding both sockets until both are known: asked one at
# a time, the kernel hands the same ephemeral port straight back out.
two_free_loopback_ports() {
  "$PYTHON" -c '
import socket
held = [socket.socket() for _ in range(2)]
for one in held:
    one.bind(("127.0.0.1", 0))
print(" ".join(str(one.getsockname()[1]) for one in held))
for one in held:
    one.close()
'
}
read -r A_FREE_PORT ANOTHER_FREE_PORT <<<"$(two_free_loopback_ports)"
SOURCE_CONTROL_PORT="${SOURCE_CONTROL_PORT:-$A_FREE_PORT}"
READER_CONTROL_PORT="${READER_CONTROL_PORT:-$ANOTHER_FREE_PORT}"

THE_DISPLAY_NAME="TestPatternSource"
THE_PORT="video"
THE_ADDRESS="$SOURCE_RUNTIME_NAME/$THE_DISPLAY_NAME/$THE_PORT"

HOW_LONG_THE_LINK_HAS_TO_WIRE=45
HOW_LONG_A_RUNTIME_HAS_TO_STOP=30

say() { printf '%s\n' "$*" >&2; }

stop_both_runtimes() {
  for pid in "${SOURCE_PID:-}" "${READER_PID:-}"; do
    [ -n "$pid" ] && kill -TERM "$pid" 2>/dev/null
  done
  for pid in "${SOURCE_PID:-}" "${READER_PID:-}"; do
    [ -z "$pid" ] && continue
    waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt "$HOW_LONG_A_RUNTIME_HAS_TO_STOP" ]; do
      sleep 1
      waited=$((waited + 1))
    done
    if kill -0 "$pid" 2>/dev/null; then
      say "A runtime did not stop within ${HOW_LONG_A_RUNTIME_HAS_TO_STOP}s; killing it."
      kill -KILL "$pid" 2>/dev/null
    fi
    wait "$pid" 2>/dev/null
  done
}
trap stop_both_runtimes EXIT

if ! "$PYTHON" -c 'import msgpack' 2>/dev/null; then
  say "CANNOT RUN: reading a bag's surface_id needs msgpack ('pip install msgpack')."
  exit 3
fi

say "Building the rig..."
if ! cargo build --locked -p streamlib-engine --example cross_runtime_link_rig \
  >"$OUTPUT_DIR/build.log" 2>&1; then
  say "CANNOT RUN: the rig did not build; see $OUTPUT_DIR/build.log"
  exit 3
fi
RIG="$WORKSPACE_ROOT/target/debug/examples/cross_runtime_link_rig"

say "Starting the video source runtime ($SOURCE_RUNTIME_NAME)..."
STREAMLIB_RUNTIME_NAME="$SOURCE_RUNTIME_NAME" STREAMLIB_MESH_NAME="$MESH_NAME" \
  "$RIG" --video-source --control-plane-port "$SOURCE_CONTROL_PORT" \
  >"$OUTPUT_DIR/source.log" 2>&1 &
SOURCE_PID=$!

say "Starting the video reader runtime ($READER_RUNTIME_NAME), linked from $THE_ADDRESS..."
STREAMLIB_RUNTIME_NAME="$READER_RUNTIME_NAME" STREAMLIB_MESH_NAME="$MESH_NAME" \
  "$RIG" --video-reader "$SOURCE_RUNTIME_NAME" --control-plane-port "$READER_CONTROL_PORT" \
  >"$OUTPUT_DIR/reader.log" 2>&1 &
READER_PID=$!

# A runtime that could not start — no GPU, no Vulkan Video — is a rig that
# cannot run this, never a failing engine.
sleep 5
for pid in "$SOURCE_PID" "$READER_PID"; do
  if ! kill -0 "$pid" 2>/dev/null; then
    say "CANNOT RUN: a runtime exited before the link could wire."
    say "  source: $OUTPUT_DIR/source.log"
    say "  reader: $OUTPUT_DIR/reader.log"
    exit 3
  fi
done

read_the_graph_of() {
  curl --silent --show-error --max-time 10 \
    "http://127.0.0.1:$1/api/graph" 2>>"$OUTPUT_DIR/curl.log"
}

say "Waiting for the link to wire..."
LINK_STATE="unread"
for _ in $(seq 1 "$HOW_LONG_THE_LINK_HAS_TO_WIRE"); do
  GRAPH="$(read_the_graph_of "$READER_CONTROL_PORT")"
  if [ -n "$GRAPH" ]; then
    printf '%s' "$GRAPH" >"$OUTPUT_DIR/reader-graph.json"
    LINK_STATE="$(printf '%s' "$GRAPH" | "$PYTHON" -c '
import json, sys
graph = json.load(sys.stdin)
for link in graph.get("links", []):
    if link.get("source", {}).get("runtime_name"):
        print(link.get("state", "no state"))
        break
else:
    print("no remote link")
' 2>/dev/null)"
    [ "$LINK_STATE" = "wired" ] && break
  fi
  sleep 1
done

if [ "$LINK_STATE" != "wired" ]; then
  say "FAIL: the link read '$LINK_STATE' rather than 'wired'."
  say "  reader graph: $OUTPUT_DIR/reader-graph.json"
  say "  reader log:   $OUTPUT_DIR/reader.log"
  exit 1
fi
say "The link is wired."

# The sending side is read from its own log rather than tapped: `tap` resolves
# a channel through the running graph, and a port only the mesh reads has no
# local link to resolve through, so it refuses the port by name. The pattern
# publishes one surface for its whole run and says which as it mints it, so
# this id is stable and its frame is never recycled under the exchange below.
SOURCE_SURFACE_ID="$(
  "$PYTHON" -c '
import re, sys
found = None
for line in open(sys.argv[1], errors="replace"):
    if "pattern surface ready" in line:
        named = re.search(r"surface_id=\"([^\"]+)\"", line)
        if named:
            found = named.group(1)
print(found or "")
' "$OUTPUT_DIR/source.log"
)"
if [ -z "$SOURCE_SURFACE_ID" ]; then
  say "FAIL: the source named no surface; see $OUTPUT_DIR/source.log"
  exit 1
fi

# A pooled frame id is `<slot>#<generation>`, and a bare `#` would make the
# generation a URL fragment the server never sees — so the id is encoded down
# to RFC 3986's unreserved set before it goes on the wire.
exchange_on() {
  local encoded_id
  encoded_id="$("$PYTHON" -c '
import sys, urllib.parse
print(urllib.parse.quote(sys.argv[1], safe=""))
' "$2")"
  curl --silent --show-error --fail --max-time 30 \
    "http://127.0.0.1:$1/api/surfaces/$encoded_id/image" \
    --output "$3" 2>>"$OUTPUT_DIR/curl.log"
}

say "Exchanging the source's own surface '$SOURCE_SURFACE_ID'..."
if ! exchange_on "$SOURCE_CONTROL_PORT" "$SOURCE_SURFACE_ID" "$OUTPUT_DIR/source-frame.png"; then
  say "FAIL: the source could not exchange its own surface '$SOURCE_SURFACE_ID'."
  exit 1
fi

# Tap and exchange in one breath, retried: the reader mints a fresh surface for
# every arriving frame, so its pool rotates through a slot in a few frame
# times and an id read a moment ago is answered `410 Gone`. One bag per tap
# rather than a sample, so the tap returns on the first frame rather than
# holding its window open. Each attempt is a whole frame of the same static
# pattern, so whichever lands is the one to score.
HOW_MANY_FRAMES_MAY_BE_RECYCLED_UNDER_THE_EXCHANGE=15
READER_SURFACE_ID=""
for _ in $(seq 1 "$HOW_MANY_FRAMES_MAY_BE_RECYCLED_UNDER_THE_EXCHANGE"); do
  READER_TAP="$("$PYTHON" "$HERE/tap_a_mesh_address.py" \
    --url "http://127.0.0.1:$READER_CONTROL_PORT" --address "$THE_ADDRESS" \
    --count 1 --report-surface-id 2>>"$OUTPUT_DIR/tap.log")"
  printf '%s\n' "$READER_TAP" >>"$OUTPUT_DIR/reader-taps.jsonl"
  TAPPED_ID="$(printf '%s' "$READER_TAP" | "$PYTHON" -c '
import json, sys
print(json.load(sys.stdin).get("surface_id") or "")
' 2>/dev/null)"
  [ -z "$TAPPED_ID" ] && continue
  if exchange_on "$READER_CONTROL_PORT" "$TAPPED_ID" "$OUTPUT_DIR/reader-frame.png"; then
    READER_SURFACE_ID="$TAPPED_ID"
    break
  fi
done

if [ -z "$READER_SURFACE_ID" ]; then
  say "FAIL: the reader minted no surface this could exchange before it was recycled."
  say "  taps: $OUTPUT_DIR/reader-taps.jsonl"
  say "  log:  $OUTPUT_DIR/reader.log"
  exit 1
fi
if [ "$SOURCE_SURFACE_ID" = "$READER_SURFACE_ID" ]; then
  say "FAIL: both ends name the surface '$SOURCE_SURFACE_ID'. A surface id must"
  say "      never cross — the reading runtime mints one of its own."
  exit 1
fi
say "The two ends name different surfaces: '$SOURCE_SURFACE_ID' and '$READER_SURFACE_ID'."

# Each id resolves only where it was minted: an exchange of the other node's id
# must be refused, which is what "no surface id crosses" means in practice.
if exchange_on "$READER_CONTROL_PORT" "$SOURCE_SURFACE_ID" "$OUTPUT_DIR/should-not-exist.png"; then
  say "FAIL: the reader resolved the sender's surface id '$SOURCE_SURFACE_ID'."
  exit 1
fi
say "Each id resolves on its own node and nowhere else."

# Byte-exact, not a PSNR floor: RGBA crosses as RGBA and the exchange encodes
# both ends' pixels with the same lossless writer, so any difference at all is
# a copy that went wrong.
if ! cmp --silent "$OUTPUT_DIR/source-frame.png" "$OUTPUT_DIR/reader-frame.png"; then
  say "FAIL: the frame that arrived is not the frame that was sent."
  say "  sent:    $OUTPUT_DIR/source-frame.png"
  say "  arrived: $OUTPUT_DIR/reader-frame.png"
  say "  score them with: cargo xtask psnr score --decoded <dir> --reference <dir>"
  exit 1
fi

# Both runtimes stopped before the logs are read, so teardown's own lines are
# inside the gate rather than after it.
say "Stopping both runtimes..."
stop_both_runtimes
SOURCE_PID=""
READER_PID=""

THE_STANDARD_LOG_GATES='OUT_OF_DEVICE_MEMORY|DEVICE_LOST|Validation Error|process\(\) failed'
for which in source reader; do
  if grep -qE "$THE_STANDARD_LOG_GATES" "$OUTPUT_DIR/$which.log"; then
    say "FAIL: the $which runtime's log carries a failure the gates refuse:"
    grep -nE "$THE_STANDARD_LOG_GATES" "$OUTPUT_DIR/$which.log" | head -5 >&2
    exit 1
  fi
done
say "Both logs pass the standard gates."

say "PASS: the frame crossed the mesh byte for byte into a surface of the reader's own."
"$PYTHON" -c '
import json, sys
print(json.dumps({
    "verdict": "pass",
    "address": sys.argv[1],
    "sending_runtime_surface_id": sys.argv[2],
    "reading_runtime_surface_id": sys.argv[3],
    "pixels": "byte-identical",
}))
' "$THE_ADDRESS" "$SOURCE_SURFACE_ID" "$READER_SURFACE_ID"

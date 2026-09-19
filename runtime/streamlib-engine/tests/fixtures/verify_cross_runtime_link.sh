#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# The cross-runtime-link rig arm: two started runtimes, one link.
#
# CI's proof of a remote link stands each runtime's mesh half up without a
# `Runner`, because `Runner::start()` needs a GPU. This is the other half of
# the bar — two whole runtimes on real hardware, one pulling the other's output
# port through the ordinary `connect` — so it is rig-only by construction.
#
# What it reads, all through the reader's own control plane:
#   1. `graph` carries a link whose `source` is the three-part mesh address,
#      not a processor id.
#   2. That link's `state` is `wired` — the source runtime turned up, said it
#      offers the port, and the ingress opened.
#   3. Tapping the port BY ITS MESH ADDRESS returns bags: the channel the
#      ingress writes is hashed from that address, so the address is the only
#      thing a caller could name it by.
#
# Usage:
#   ./verify_cross_runtime_link.sh [output_dir]
#
# Exit status is the verdict. Progress and both runtimes' logs go to stderr and
# to output_dir; the report JSON is the only thing on stdout, so a caller can
# pipe it.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$HERE/../../../.." && pwd)"
PYTHON="${PYTHON:-python3}"
OUTPUT_DIR="${1:-$(mktemp -d -t streamlib-cross-runtime-link-XXXXXX)}"
mkdir -p "$OUTPUT_DIR"

# A mesh of this run's own, so a rig running two of these at once — or a real
# runtime on the same machine — is never part of the proof.
MESH_NAME="xrig-$$"
SOURCE_RUNTIME_NAME="xrig-source-$$"
READER_RUNTIME_NAME="xrig-reader-$$"
# A free loopback port each, rather than two fixed ones: two runs of this at
# once — or anything else already on 9410/9411 — would otherwise have the second
# runtime fail to bind before it ever reached the link it is here to check.
# Both in one go, holding both sockets until both ports are known: asked one at
# a time, the first socket is closed before the second is bound and the kernel
# hands the same ephemeral port straight back out.
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

# The port's address on the mesh, spelled the way `connect` and `tap` take it.
THE_ADDRESS="$SOURCE_RUNTIME_NAME/MicrophoneSource/audio"

# How long the link has to resolve: the source's token has to reach the reader,
# the reader has to ask what it offers, and the egress has to come up. Generous
# against a loaded rig; the arm normally converges in a couple of seconds.
HOW_LONG_THE_LINK_HAS_TO_WIRE=45

say() { printf '%s\n' "$*" >&2; }

# How long a runtime has to finish leaving before it is killed. A bounded wait
# on purpose: `wait` with no deadline turns a runtime that never exits into a
# fixture that hangs its caller rather than one that reports a failure.
HOW_LONG_A_RUNTIME_HAS_TO_STOP=30

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

say "Building the rig..."
if ! cargo build --locked -p streamlib-engine --example cross_runtime_link_rig \
  >"$OUTPUT_DIR/build.log" 2>&1; then
  say "CANNOT RUN: the rig did not build; see $OUTPUT_DIR/build.log"
  exit 3
fi
RIG="$WORKSPACE_ROOT/target/debug/examples/cross_runtime_link_rig"

say "Starting the source runtime ($SOURCE_RUNTIME_NAME)..."
STREAMLIB_RUNTIME_NAME="$SOURCE_RUNTIME_NAME" STREAMLIB_MESH_NAME="$MESH_NAME" \
  "$RIG" --source --control-plane-port "$SOURCE_CONTROL_PORT" \
  >"$OUTPUT_DIR/source.log" 2>&1 &
SOURCE_PID=$!

say "Starting the reader runtime ($READER_RUNTIME_NAME), linked from $THE_ADDRESS..."
STREAMLIB_RUNTIME_NAME="$READER_RUNTIME_NAME" STREAMLIB_MESH_NAME="$MESH_NAME" \
  "$RIG" --reader "$SOURCE_RUNTIME_NAME" --control-plane-port "$READER_CONTROL_PORT" \
  >"$OUTPUT_DIR/reader.log" 2>&1 &
READER_PID=$!

# A runtime that could not start — no GPU, no display server — is a rig that
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

read_the_readers_graph() {
  curl --silent --show-error --max-time 10 \
    "http://127.0.0.1:$READER_CONTROL_PORT/api/graph" 2>>"$OUTPUT_DIR/curl.log"
}

say "Waiting for the link to wire..."
LINK_STATE="unread"
for _ in $(seq 1 "$HOW_LONG_THE_LINK_HAS_TO_WIRE"); do
  GRAPH="$(read_the_readers_graph)"
  if [ -n "$GRAPH" ]; then
    printf '%s' "$GRAPH" >"$OUTPUT_DIR/reader-graph.json"
    LINK_STATE="$(printf '%s' "$GRAPH" | "$PYTHON" -c '
import json, sys
graph = json.load(sys.stdin)
for link in graph.get("links", []):
    source = link.get("source", {})
    if source.get("runtime_name"):
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

SOURCE_RENDERED="$("$PYTHON" -c '
import json, sys
graph = json.load(open(sys.argv[1]))
for link in graph.get("links", []):
    source = link.get("source", {})
    if source.get("runtime_name"):
        print("{}/{}/{}".format(source["runtime_name"],
                                source["processor_display_name"],
                                source["port_name"]))
        break
' "$OUTPUT_DIR/reader-graph.json")"
if [ "$SOURCE_RENDERED" != "$THE_ADDRESS" ]; then
  say "FAIL: graph rendered the source as '$SOURCE_RENDERED', not '$THE_ADDRESS'."
  exit 1
fi
say "graph renders the source as its mesh address."

say "Tapping $THE_ADDRESS on the reader..."
TAP_REPORT="$("$PYTHON" "$HERE/tap_a_mesh_address.py" \
  --url "http://127.0.0.1:$READER_CONTROL_PORT" \
  --address "$THE_ADDRESS" \
  --count 8 2>>"$OUTPUT_DIR/tap.log")"
if [ -z "$TAP_REPORT" ]; then
  say "FAIL: the tap reported nothing; see $OUTPUT_DIR/tap.log"
  exit 1
fi
printf '%s' "$TAP_REPORT" >"$OUTPUT_DIR/tap-report.json"

BAGS="$(printf '%s' "$TAP_REPORT" | "$PYTHON" -c 'import json,sys; print(json.load(sys.stdin)["bags"])')"
if [ "$BAGS" -lt 1 ]; then
  say "FAIL: the link is wired and carried $BAGS bags."
  exit 1
fi

say "PASS: $BAGS bags crossed the mesh into the reader."
"$PYTHON" -c '
import json, sys
report = json.load(open(sys.argv[1]))
report["verdict"] = "pass"
report["address"] = sys.argv[2]
print(json.dumps(report))
' "$OUTPUT_DIR/tap-report.json" "$THE_ADDRESS"

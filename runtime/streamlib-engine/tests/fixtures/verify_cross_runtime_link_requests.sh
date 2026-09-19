#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# The link-request rig arm: three started runtimes, and a link none of the two
# that hold its ends asked for.
#
# CI's proof of a link request runs three runtimes that are constructed and
# never started, because `Runner::start()` needs a GPU. This is the other half
# of the bar — three whole runtimes on real hardware, wired by a fourth party
# over the control plane an agent actually drives — so it is rig-only by
# construction.
#
# What it reads, all through the destination's own control plane:
#   1. MCP `connect` on a runtime that is NEITHER end answers a
#      `link_request_id` rather than a `link_id`: the link is the runtime that
#      owns the input's to make, not the caller's.
#   2. The destination's `graph` carries that link, with `created_by_runtime_name`
#      naming the wiring runtime — neither of its own ends.
#   3. Its `state` is `wired`, and tapping the port BY ITS MESH ADDRESS returns
#      bags. A wired link that carries nothing would pass (2) and fail here.
#
# Usage:
#   ./verify_cross_runtime_link_requests.sh [output_dir]
#
# Exit status is the verdict. Progress and all three runtimes' logs go to
# stderr and to output_dir; the report JSON is the only thing on stdout.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$HERE/../../../.." && pwd)"
PYTHON="${PYTHON:-python3}"
OUTPUT_DIR="${1:-$(mktemp -d -t streamlib-link-requests-XXXXXX)}"
mkdir -p "$OUTPUT_DIR"

MESH_NAME="xreq-$$"
SOURCE_RUNTIME_NAME="xreq-source-$$"
DESTINATION_RUNTIME_NAME="xreq-dest-$$"
WIRING_RUNTIME_NAME="xreq-agent-$$"

# Three free loopback ports, all bound at once and released together: asked one
# at a time, the first socket is closed before the next is bound and the kernel
# hands the same ephemeral port straight back out — and `streamlib run`
# increments its control port on collision, so a runtime would land somewhere
# this script never learned.
three_free_loopback_ports() {
  "$PYTHON" -c '
import socket
held = [socket.socket() for _ in range(3)]
for one in held:
    one.bind(("127.0.0.1", 0))
print(" ".join(str(one.getsockname()[1]) for one in held))
for one in held:
    one.close()
'
}
read -r A_FREE_PORT ANOTHER_FREE_PORT A_THIRD_FREE_PORT <<<"$(three_free_loopback_ports)"
SOURCE_CONTROL_PORT="${SOURCE_CONTROL_PORT:-$A_FREE_PORT}"
DESTINATION_CONTROL_PORT="${DESTINATION_CONTROL_PORT:-$ANOTHER_FREE_PORT}"
WIRING_CONTROL_PORT="${WIRING_CONTROL_PORT:-$A_THIRD_FREE_PORT}"

# The port's address on the mesh, spelled the way `connect` and `tap` take it.
THE_SOURCE_ADDRESS="$SOURCE_RUNTIME_NAME/MicrophoneSource/audio"
THE_DESTINATIONS_DISPLAY_NAME="OpusEncoder"
THE_PORT="audio"

# How long the link has to resolve: the request has to reach the destination,
# the destination has to apply it, its reader token has to reach the source,
# and the source's egress has to come up.
HOW_LONG_THE_LINK_HAS_TO_WIRE=60

say() { printf '%s\n' "$*" >&2; }

HOW_LONG_A_RUNTIME_HAS_TO_STOP=30

stop_every_runtime() {
  for pid in "${SOURCE_PID:-}" "${DESTINATION_PID:-}" "${WIRING_PID:-}"; do
    [ -n "$pid" ] && kill -TERM "$pid" 2>/dev/null
  done
  for pid in "${SOURCE_PID:-}" "${DESTINATION_PID:-}" "${WIRING_PID:-}"; do
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
trap stop_every_runtime EXIT

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

say "Starting the destination runtime ($DESTINATION_RUNTIME_NAME), wiring nothing..."
STREAMLIB_RUNTIME_NAME="$DESTINATION_RUNTIME_NAME" STREAMLIB_MESH_NAME="$MESH_NAME" \
  "$RIG" --destination --control-plane-port "$DESTINATION_CONTROL_PORT" \
  >"$OUTPUT_DIR/destination.log" 2>&1 &
DESTINATION_PID=$!

say "Starting the wiring runtime ($WIRING_RUNTIME_NAME), holding no processor..."
STREAMLIB_RUNTIME_NAME="$WIRING_RUNTIME_NAME" STREAMLIB_MESH_NAME="$MESH_NAME" \
  "$RIG" --wiring-agent --control-plane-port "$WIRING_CONTROL_PORT" \
  >"$OUTPUT_DIR/wiring.log" 2>&1 &
WIRING_PID=$!

# A runtime that could not start — no GPU, no display server — is a rig that
# cannot run this, never a failing engine.
sleep 5
for pid in "$SOURCE_PID" "$DESTINATION_PID" "$WIRING_PID"; do
  if ! kill -0 "$pid" 2>/dev/null; then
    say "CANNOT RUN: a runtime exited before the link could be asked for."
    say "  source:      $OUTPUT_DIR/source.log"
    say "  destination: $OUTPUT_DIR/destination.log"
    say "  wiring:      $OUTPUT_DIR/wiring.log"
    exit 3
  fi
done

# The wiring runtime has to see both ends before it can address either: a
# request to a runtime it cannot see waits rather than failing, which would
# make this arm time out with nothing to say.
say "Waiting for the wiring runtime to see both ends..."
SAW_BOTH=no
for _ in $(seq 1 30); do
  PEERS="$(curl --silent --show-error --max-time 10 \
    "http://127.0.0.1:$WIRING_CONTROL_PORT/api/graph" 2>>"$OUTPUT_DIR/curl.log" |
    "$PYTHON" -c '
import json, sys
try:
    graph = json.load(sys.stdin)
except ValueError:
    sys.exit(0)
print(" ".join(sorted(peer["runtime_name"] for peer in graph["mesh"]["peers"])))
' 2>/dev/null)"
  case "$PEERS" in
    *"$SOURCE_RUNTIME_NAME"*)
      case "$PEERS" in *"$DESTINATION_RUNTIME_NAME"*) SAW_BOTH=yes ;; esac
      ;;
  esac
  [ "$SAW_BOTH" = yes ] && break
  sleep 1
done
if [ "$SAW_BOTH" != yes ]; then
  say "CANNOT RUN: the wiring runtime saw '$PEERS' rather than both ends."
  exit 3
fi
say "The wiring runtime sees both ends."

say "Asking $WIRING_RUNTIME_NAME to wire $THE_SOURCE_ADDRESS into $DESTINATION_RUNTIME_NAME..."
WIRING_REPORT="$("$PYTHON" "$HERE/wire_two_runtimes_over_mcp.py" \
  --url "http://127.0.0.1:$WIRING_CONTROL_PORT" \
  --from-runtime "$SOURCE_RUNTIME_NAME" \
  --from-display-name "MicrophoneSource" \
  --from-port "$THE_PORT" \
  --to-runtime "$DESTINATION_RUNTIME_NAME" \
  --to-display-name "$THE_DESTINATIONS_DISPLAY_NAME" \
  --to-port "$THE_PORT" 2>>"$OUTPUT_DIR/wiring-call.log")"
if [ -z "$WIRING_REPORT" ]; then
  say "FAIL: the wiring call reported nothing; see $OUTPUT_DIR/wiring-call.log"
  exit 1
fi
printf '%s' "$WIRING_REPORT" >"$OUTPUT_DIR/wiring-report.json"

# A runtime that is neither end answers a request id, never a link id: the link
# is the runtime that owns the input's to make.
if ! printf '%s' "$WIRING_REPORT" | "$PYTHON" -c '
import json, sys
answered = json.load(sys.stdin)
assert "link_request_id" in answered, answered
assert "link_id" not in answered, answered
assert answered["input_runtime_name"] == sys.argv[1], answered
' "$DESTINATION_RUNTIME_NAME" 2>>"$OUTPUT_DIR/wiring-call.log"; then
  say "FAIL: the wiring call answered $WIRING_REPORT, which is not a link request."
  exit 1
fi
say "The wiring runtime answered a link request, not a link."

read_the_destinations_graph() {
  curl --silent --show-error --max-time 10 \
    "http://127.0.0.1:$DESTINATION_CONTROL_PORT/api/graph" 2>>"$OUTPUT_DIR/curl.log"
}

say "Waiting for the link to wire on $DESTINATION_RUNTIME_NAME..."
LINK_STATE="unread"
for _ in $(seq 1 "$HOW_LONG_THE_LINK_HAS_TO_WIRE"); do
  GRAPH="$(read_the_destinations_graph)"
  if [ -n "$GRAPH" ]; then
    printf '%s' "$GRAPH" >"$OUTPUT_DIR/destination-graph.json"
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
  say "  destination graph: $OUTPUT_DIR/destination-graph.json"
  say "  destination log:   $OUTPUT_DIR/destination.log"
  exit 1
fi
say "The link is wired on the runtime that owns the input."

CREATED_BY="$("$PYTHON" -c '
import json, sys
graph = json.load(open(sys.argv[1]))
for link in graph.get("links", []):
    if link.get("source", {}).get("runtime_name"):
        print(link.get("created_by_runtime_name", "unnamed"))
        break
' "$OUTPUT_DIR/destination-graph.json")"
if [ "$CREATED_BY" != "$WIRING_RUNTIME_NAME" ]; then
  say "FAIL: the link names '$CREATED_BY' as its creator, not '$WIRING_RUNTIME_NAME'."
  exit 1
fi
say "The link names the runtime that asked for it — neither of its own ends."

say "Tapping $THE_SOURCE_ADDRESS on the destination..."
TAP_REPORT="$("$PYTHON" "$HERE/tap_a_mesh_address.py" \
  --url "http://127.0.0.1:$DESTINATION_CONTROL_PORT" \
  --address "$THE_SOURCE_ADDRESS" \
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

say "PASS: $BAGS bags crossed a link a third runtime asked for."
"$PYTHON" -c '
import json, sys
report = json.load(open(sys.argv[1]))
report["verdict"] = "pass"
report["address"] = sys.argv[2]
report["created_by_runtime_name"] = sys.argv[3]
report["link_request"] = json.load(open(sys.argv[4]))
print(json.dumps(report))
' "$OUTPUT_DIR/tap-report.json" "$THE_SOURCE_ADDRESS" "$WIRING_RUNTIME_NAME" \
  "$OUTPUT_DIR/wiring-report.json"

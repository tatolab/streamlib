#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# The Python-and-helper half of the cross-runtime-link rig arm.
#
# `verify_cross_runtime_link.sh` proves the link between two started runtimes
# from Rust, into a native destination. This proves the other authoring surface
# and the other placement: the reader is a Python app that spells the remote
# port with `rt.remote_processor_output(...)`, and its destination is a
# helper-placed Python processor, so the link's name has to survive the
# parent's wiring envelope into a child interpreter to be read at all.
#
# What it reads:
#   1. The reader's `graph` carries a link whose `source` is the three-part
#      mesh address and whose `state` is `wired`.
#   2. The helper-placed probe enumerated that link under its *address* at
#      `setup()` — not under the hashed channel its ingress writes — and read
#      bags off it naming the same address, from a pid that is not the app's.
#   3. Tapping the port by its mesh address on the reader returns bags, and
#      those bags arrive as one continuous stream under their producer's own
#      stamps rather than ones minted at the receiving end.
#   4. The *source's* `graph.mesh.egress_ports` names the port and the reader
#      runtime reading it, and empties once the reader has gone.
#
# Usage:
#   ./verify_cross_runtime_link_from_python.sh [output_dir]
#
# Exit status is the verdict: 0 pass, 1 fail, 77 cannot run. Progress and both
# runtimes' logs go to stderr and to output_dir; the report JSON is the only
# thing on stdout.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../../../.." && pwd)"
OUTPUT_DIR="${1:-$(mktemp -d -t streamlib-cross-runtime-python-XXXXXX)}"
mkdir -p "$OUTPUT_DIR"

say() { printf '%s\n' "$*" >&2; }

STREAMLIB_CLI="$(command -v streamlib || true)"
if [ -z "$STREAMLIB_CLI" ]; then
    STREAMLIB_CLI="$REPO_ROOT/sdk/streamlib-python-wheel/.venv/bin/streamlib"
fi
if [ ! -x "$STREAMLIB_CLI" ]; then
    say "SKIP: no streamlib CLI on PATH or at $STREAMLIB_CLI"
    exit 77
fi

# The interpreter beside the CLI, because that is the one whose environment the
# CLI ships in; a bare `python3` can be an unrelated one first on PATH.
PYTHON="$(dirname "$STREAMLIB_CLI")/python3"
if [ ! -x "$PYTHON" ]; then
    PYTHON="$(command -v python3)"
fi
# The nodes run whatever `_engine.abi3.so` that interpreter imports, so an
# extension predating the remote reference would be measured and reported as a
# PASS for code that is not in the tree. Refused by name instead.
if ! CANNOT_IMPORT="$("$PYTHON" -c '
import streamlib

streamlib.RemoteProcessorOutputPortReference
streamlib.Runtime.remote_processor_output
' 2>&1)"; then
    say "SKIP: $PYTHON has no remote port reference. Rebuild the wheel with"
    say "      \`maturin develop\` — this measures the extension, not the tree."
    say "$CANNOT_IMPORT"
    exit 77
fi

# A mesh of this run's own, so a rig running two of these at once — or a real
# runtime on the same machine — is never part of the proof.
MESH_NAME="xpy-$$"
SOURCE_RUNTIME_NAME="xpy-source-$$"
READER_RUNTIME_NAME="xpy-reader-$$"
THE_ADDRESS="$SOURCE_RUNTIME_NAME/KnownAudioSignalSource/audio"

# Both ports in one go, holding both sockets until both are known: asked one at
# a time, the first socket is closed before the second is bound and the kernel
# hands the same ephemeral port straight back out.
read -r SOURCE_CONTROL_PORT READER_CONTROL_PORT <<<"$("$PYTHON" -c '
import socket
held = [socket.socket() for _ in range(2)]
for one in held:
    one.bind(("127.0.0.1", 0))
print(" ".join(str(one.getsockname()[1]) for one in held))
for one in held:
    one.close()
')"
SOURCE_URL="http://127.0.0.1:$SOURCE_CONTROL_PORT"
READER_URL="http://127.0.0.1:$READER_CONTROL_PORT"

# How long the link has to resolve: the source's token has to reach the reader,
# the reader has to ask what it offers, the egress has to come up and the
# helper has to answer. Generous against a loaded rig.
HOW_LONG_THE_LINK_HAS_TO_WIRE=60

# How long a runtime has to finish leaving before it is killed. Bounded on
# purpose: a `wait` with no deadline turns a runtime that never exits into a
# fixture that hangs its caller rather than one that reports a failure.
HOW_LONG_A_RUNTIME_HAS_TO_STOP=30

stop_one_runtime() {
    local pid="$1"
    [ -z "$pid" ] && return 0
    kill -TERM "$pid" 2>/dev/null
    local waited=0
    while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt "$HOW_LONG_A_RUNTIME_HAS_TO_STOP" ]; do
        sleep 1
        waited=$((waited + 1))
    done
    if kill -0 "$pid" 2>/dev/null; then
        say "A runtime did not stop within ${HOW_LONG_A_RUNTIME_HAS_TO_STOP}s; killing it."
        kill -KILL "$pid" 2>/dev/null
    fi
    wait "$pid" 2>/dev/null
}

stop_both_runtimes() {
    stop_one_runtime "${READER_PID:-}"
    stop_one_runtime "${SOURCE_PID:-}"
}
trap stop_both_runtimes EXIT

NODE="$HERE/cross_runtime_link_python_node.py"
NODE_LOG_LEVEL="${RUST_LOG:-warn,streamlib=info}"

say "Starting the source runtime ($SOURCE_RUNTIME_NAME) on $SOURCE_URL..."
# Repeating, because this arm taps: the signal is 3.78 s and a tap's window
# opens whenever its caller gets round to asking, which is well past that.
RUST_LOG="$NODE_LOG_LEVEL" \
    STREAMLIB_KNOWN_SIGNAL_REPEATS=1 \
    STREAMLIB_RUNTIME_NAME="$SOURCE_RUNTIME_NAME" STREAMLIB_MESH_NAME="$MESH_NAME" \
    "$PYTHON" "$NODE" --source --control-plane-port "$SOURCE_CONTROL_PORT" \
    >"$OUTPUT_DIR/source.log" 2>&1 &
SOURCE_PID=$!

say "Starting the reader runtime ($READER_RUNTIME_NAME), linked from $THE_ADDRESS..."
RUST_LOG="$NODE_LOG_LEVEL" \
    STREAMLIB_RUNTIME_NAME="$READER_RUNTIME_NAME" STREAMLIB_MESH_NAME="$MESH_NAME" \
    "$PYTHON" "$NODE" --reader "$SOURCE_RUNTIME_NAME" \
    --control-plane-port "$READER_CONTROL_PORT" \
    >"$OUTPUT_DIR/reader.log" 2>&1 &
READER_PID=$!

# A runtime that could not start — no GPU, no display server — is a rig that
# cannot run this, never a failing engine.
sleep 8
for pid in "$SOURCE_PID" "$READER_PID"; do
    if ! kill -0 "$pid" 2>/dev/null; then
        say "CANNOT RUN: a runtime exited before the link could wire."
        say "  source: $OUTPUT_DIR/source.log"
        say "  reader: $OUTPUT_DIR/reader.log"
        exit 77
    fi
done

read_the_graph_at() {
    curl --silent --show-error --max-time 10 "$1/api/graph" 2>>"$OUTPUT_DIR/curl.log"
}

# ── 1. The link wires, rendered as its mesh address ──────────────────
say "Waiting for the link to wire..."
LINK_STATE="unread"
for _ in $(seq 1 "$HOW_LONG_THE_LINK_HAS_TO_WIRE"); do
    GRAPH="$(read_the_graph_at "$READER_URL")"
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
say "The link is wired, and graph renders its source as $THE_ADDRESS."

# ── 2. The helper-placed probe names the link by its address ─────────
say "Waiting for the helper-placed probe to read bags off it..."
PROBE_PID=""
for _ in $(seq 1 "$HOW_LONG_THE_LINK_HAS_TO_WIRE"); do
    if grep -q "MARKER:RECEIVED $THE_ADDRESS " "$OUTPUT_DIR/reader.log" 2>/dev/null; then
        break
    fi
    sleep 1
done

if ! grep -q "MARKER:INBOUND_LINKS $THE_ADDRESS" "$OUTPUT_DIR/reader.log"; then
    say "FAIL: the probe did not enumerate the link under its mesh address at setup()."
    say "  what it enumerated:"
    grep "MARKER:INBOUND_LINKS" "$OUTPUT_DIR/reader.log" >&2 || say "  (nothing)"
    exit 1
fi
RECEIVED_LINE="$(grep -m1 "MARKER:RECEIVED $THE_ADDRESS " "$OUTPUT_DIR/reader.log" || true)"
if [ -z "$RECEIVED_LINE" ]; then
    say "FAIL: the probe read no bags naming $THE_ADDRESS."
    grep "MARKER:RECEIVED" "$OUTPUT_DIR/reader.log" >&2 || say "  (it reported no reads at all)"
    exit 1
fi
PROBE_PID="$(printf '%s' "$RECEIVED_LINE" | grep -o 'helper_pid=[0-9]*' | head -1 | cut -d= -f2)"
if [ -z "$PROBE_PID" ] || [ "$PROBE_PID" = "$READER_PID" ]; then
    say "FAIL: the probe reported pid '$PROBE_PID', which is the app's own —"
    say "      a Python processor runs in its own helper process, always."
    exit 1
fi
say "The probe named the link $THE_ADDRESS from helper pid $PROBE_PID."

# ── 3. The remote link taps by its mesh address ──────────────────────
say "Tapping $THE_ADDRESS on the reader..."
TAP_REPORT="$("$PYTHON" "$HERE/tap_a_mesh_address.py" \
    --url "$READER_URL" --address "$THE_ADDRESS" --count 8 \
    2>>"$OUTPUT_DIR/tap.log")"
if [ -z "$TAP_REPORT" ]; then
    say "FAIL: the tap reported nothing; see $OUTPUT_DIR/tap.log"
    exit 1
fi
printf '%s' "$TAP_REPORT" >"$OUTPUT_DIR/tap-report.json"
TAPPED_BAGS="$(printf '%s' "$TAP_REPORT" | "$PYTHON" -c 'import json,sys; print(json.load(sys.stdin)["bags"])')"
if [ "$TAPPED_BAGS" -lt 1 ]; then
    say "FAIL: the link is wired and the tap collected $TAPPED_BAGS bags."
    exit 1
fi
say "The tap collected $TAPPED_BAGS bags off the remote link."

# The stamps the bags arrived under are the *producer's*, carried across the
# hop rather than minted at the receiving end — which is what a tap of the
# whole bag can see and a count cannot. Cadence and timestamp continuity are
# the hop's to keep: an ingress that re-stamped, or a hop that lost bags
# without saying so, shows up here as a discontinuous stream.
#
# Not `--expect-frame-not-restamped`, which the plan's validation shape named:
# that compares a bag's frame header against its own block stamp, which is a
# property of the *producer*. `KnownAudioSignalSource` publishes through the
# implicit write while running a 100 ms lead, so its frame header sits ~100 ms
# behind its block stamp before any hop — the flag fails against this fixture
# at the source. The existing arm that uses it runs it against
# `MicrophoneSource`, a capture built-in that publishes through the timestamped
# write. The hop's own byte-equal-stamp contract is CI-proven in
# `cross_runtime_links_two_processes`.
say "Checking the stamps crossed unchanged..."
if ! "$STREAMLIB_CLI" tap --url "$READER_URL" --count 20 "$THE_ADDRESS" \
    >"$OUTPUT_DIR/tapped-bags.json" 2>>"$OUTPUT_DIR/tap.log"; then
    say "FAIL: the CLI tap of $THE_ADDRESS failed; see $OUTPUT_DIR/tap.log"
    exit 1
fi
if ! "$PYTHON" "$HERE/tap_audio_channel.py" "$OUTPUT_DIR/tapped-bags.json" \
    >"$OUTPUT_DIR/stamp-report.json" 2>&1; then
    say "FAIL: the bags that crossed do not carry their producer's stamps:"
    cat "$OUTPUT_DIR/stamp-report.json" >&2
    exit 1
fi
say "The stamps crossed unchanged."

# ── 4. The source's graph names the port and its reader ──────────────
read_the_sources_egress_ports() {
    read_the_graph_at "$SOURCE_URL" | "$PYTHON" -c '
import json, sys
mesh = json.load(sys.stdin).get("mesh", {})
print(json.dumps(mesh.get("egress_ports", "absent")))
' 2>/dev/null
}

EGRESS_PORTS="$(read_the_sources_egress_ports)"
printf '%s' "$EGRESS_PORTS" >"$OUTPUT_DIR/source-egress-ports.json"
if ! printf '%s' "$EGRESS_PORTS" | grep -q "$READER_RUNTIME_NAME"; then
    say "FAIL: the source's mesh.egress_ports does not name $READER_RUNTIME_NAME."
    say "  it read: $EGRESS_PORTS"
    exit 1
fi
say "The source's graph names its port and $READER_RUNTIME_NAME reading it."

# The last reader leaving takes the egress with it, so the entry must go too.
say "Stopping the reader, and watching the source's egress ports empty..."
stop_one_runtime "$READER_PID"
READER_PID=""
EMPTIED="no"
for _ in $(seq 1 "$HOW_LONG_THE_LINK_HAS_TO_WIRE"); do
    if [ "$(read_the_sources_egress_ports)" = "[]" ]; then
        EMPTIED="yes"
        break
    fi
    sleep 1
done
if [ "$EMPTIED" != "yes" ]; then
    say "FAIL: the source still renders an egress port after its only reader left:"
    say "  $(read_the_sources_egress_ports)"
    exit 1
fi

say "PASS."
"$PYTHON" -c '
import json, sys
print(json.dumps({
    "verdict": "pass",
    "address": sys.argv[1],
    "link_state": "wired",
    "probe_helper_pid": int(sys.argv[2]),
    "tapped_bags": int(sys.argv[3]),
    "stamps": json.load(open(sys.argv[5]))["verdict"],
    "source_egress_ports_while_read": json.loads(sys.argv[4]),
    "source_egress_ports_after_the_reader_left": [],
}))
' "$THE_ADDRESS" "$PROBE_PID" "$TAPPED_BAGS" "$EGRESS_PORTS" "$OUTPUT_DIR/stamp-report.json"

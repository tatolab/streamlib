#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# `tatolab new`'s starter stream — one test pattern source — created and run on
# this Mac's `tatolabd`, on the Vulkan driver the runtime unit's lend carries:
#
#   scripts/check_scaffolded_stream_runs_on_the_driver_the_macos_lend_carries.sh \
#     <runtime-unit-root> <tatolab-stream-wheel-directory>
#
# The project's venv holds what a user's `uv sync` gives it — `tatolab-stream`,
# from the wheel directory because no index carries this version yet, and the
# `tatolab-testpattern` wheel the runtime unit carries — and no runtime. Passes when
# `tatolabd` opened the lend's Vulkan loader and the source wrote at least sixty
# frames on `video` while a viewer held the port open, as Tato Studio's viewer
# does, by the stream's `graph`. Needs a Mac with no Vulkan driver of its own,
# or the claim is not proven.

set -eu

usage="usage: $(basename "$0") <runtime-unit-root> <tatolab-stream-wheel-directory>"
runtime_unit_root="${1:?$usage}"
tatolab_stream_wheel_directory="${2:?$usage}"
tatolabd_executable="$runtime_unit_root/bin/tatolabd"
tatolab_executable="$runtime_unit_root/bin/tatolab"

# The runtime syncs the project's venv on the load, and the scaffold requires
# the `tatolab-stream` this runtime was built beside, which no index carries
# before the release publishes it. uv reads this from the environment the sync
# inherits from `tatolabd`, and resolves it from the project directory, so the
# path is absolute.
UV_FIND_LINKS="$(cd "$tatolab_stream_wheel_directory" && pwd)"
export UV_FIND_LINKS

"$tatolabd_executable" > /tmp/tatolabd.log 2>&1 &
tatolabd_pid=$!
trap 'kill -KILL "$tatolabd_pid" 2>/dev/null || true' EXIT
local_api_deadline=$((SECONDS + 60))
until "$tatolab_executable" streams > /dev/null 2>&1; do
  if ! kill -0 "$tatolabd_pid" 2>/dev/null; then
    cat /tmp/tatolabd.log
    echo "::error::tatolabd exited before its local API socket answered"
    exit 1
  fi
  if [ "$SECONDS" -ge "$local_api_deadline" ]; then
    cat /tmp/tatolabd.log
    echo "::error::tatolabd's local API socket did not answer within 60s"
    exit 1
  fi
  sleep 1
done

# The venv is made first, from this run's build, so the create's load finds
# an interpreter that runs and its sync has only the lock left to write.
mkdir -p /tmp/scaffold-check
python3 -m venv /tmp/scaffold-check/.venv
# `--no-index` keeps `tatolab-stream` to this run's build, so the one
# dependency of its no wheel directory holds comes from PyPI first.
/tmp/scaffold-check/.venv/bin/pip install 'msgpack>=1.0'
/tmp/scaffold-check/.venv/bin/pip install --no-index \
  --find-links "$tatolab_stream_wheel_directory" \
  tatolab-stream
# The unit's own wheel; `--no-deps` keeps its `tatolab-stream` to this run's build.
/tmp/scaffold-check/.venv/bin/pip install --no-deps \
  "$runtime_unit_root"/lib/tatolab/imports/tatolab_testpattern-*.whl
if /tmp/scaffold-check/.venv/bin/python -c '
import importlib.util, sys
sys.exit(0 if importlib.util.find_spec("tatolab.runtime") is not None else 1)
'; then
  echo "::error::the scaffold's venv can import tatolab.runtime — a stream's venv holds no runtime"
  exit 1
fi

# SIGINT a process, failing the step unless it exits 0 within 30s.
interrupt_and_expect_a_clean_exit() {
  process_name="$1"
  process_pid="$2"
  process_log="$3"
  kill -INT "$process_pid" 2>/dev/null || true
  stop_deadline=$((SECONDS + 30))
  while kill -0 "$process_pid" 2>/dev/null && [ "$SECONDS" -lt "$stop_deadline" ]; do
    sleep 1
  done
  if kill -0 "$process_pid" 2>/dev/null; then
    pkill -KILL -P "$process_pid" || true
    kill -KILL "$process_pid" || true
    cat "$process_log"
    echo "::error::$process_name did not stop within 30s of SIGINT"
    exit 1
  fi
  process_exit_status=0
  wait "$process_pid" || process_exit_status=$?
  if [ "$process_exit_status" -ne 0 ]; then
    cat "$process_log"
    echo "::error::$process_name exited $process_exit_status after SIGINT, not 0"
    exit 1
  fi
}

if ! "$tatolab_executable" new /tmp/scaffold-check > /tmp/scaffold-new.log 2>&1; then
  cat /tmp/scaffold-new.log /tmp/tatolabd.log
  echo "::error::tatolab new did not queue the create"
  exit 1
fi
# `graph` counts a port's writes only once something reads it, and the starter
# links nothing, so a viewer holds the source's `video` open over `tatolab mcp`
# for 20s before the graph is read, then lets go.
python3 - "$tatolab_executable" > /tmp/scaffold-graph.json 2> /tmp/scaffold-viewer.log <<'EOF_HOLD_A_VIEWER' || true
import json
import subprocess
import sys
import time

STARTER_SOURCE_NODE_NAME = "testpatternsource"
STARTER_SOURCE_OUTPUT_PORT = "video"
SECONDS_THE_VIEWER_HOLDS_THE_PORT = 20
SECONDS_THE_LOAD_HAS_TO_RUN_THE_SOURCE = 60

tatolab_mcp = subprocess.Popen(
    [sys.argv[1], "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True
)
next_request_id = 0


def answer_of(method, params):
    global next_request_id
    next_request_id += 1
    tatolab_mcp.stdin.write(
        json.dumps({"jsonrpc": "2.0", "id": next_request_id, "method": method, "params": params})
        + "\n"
    )
    tatolab_mcp.stdin.flush()
    while True:
        message = json.loads(tatolab_mcp.stdout.readline())
        if message.get("id") == next_request_id:
            return message


def tool_answer(tool_name, arguments):
    message = answer_of(
        "tools/call",
        {"name": "call_operation", "arguments": {"operation": tool_name, "params": arguments}},
    )
    result = message.get("result") or {}
    if "error" in message or result.get("isError"):
        raise SystemExit(f"{tool_name} failed: {message}")
    return json.loads(result["content"][0]["text"])


answer_of(
    "initialize",
    {
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "scaffolded-stream-check", "version": "1"},
    },
)
tatolab_mcp.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
tatolab_mcp.stdin.flush()

load_deadline = time.monotonic() + SECONDS_THE_LOAD_HAS_TO_RUN_THE_SOURCE
while True:
    streams = tool_answer("graph", {}).get("streams", [])
    sources = [
        (stream_graph["stream"], node["id"])
        for stream_graph in (streams.values() if isinstance(streams, dict) else streams)
        for node in stream_graph.get("nodes", [])
        if node.get("name") == STARTER_SOURCE_NODE_NAME
        and node.get("components", {}).get("state") == "Running"
    ]
    if sources:
        break
    if time.monotonic() >= load_deadline:
        raise SystemExit(f"no running {STARTER_SOURCE_NODE_NAME} within {SECONDS_THE_LOAD_HAS_TO_RUN_THE_SOURCE}s")
    time.sleep(1)
[(stream_id, source_node_id)] = sources
tool_answer(
    "open_viewer",
    {"stream": stream_id, "node_id": source_node_id, "port": STARTER_SOURCE_OUTPUT_PORT},
)
time.sleep(SECONDS_THE_VIEWER_HOLDS_THE_PORT)
print(json.dumps(tool_answer("graph", {})))
tatolab_mcp.stdin.close()
tatolab_mcp.wait(timeout=30)
EOF_HOLD_A_VIEWER
interrupt_and_expect_a_clean_exit tatolabd "$tatolabd_pid" /tmp/tatolabd.log
trap - EXIT
echo "::group::tatolab new"
cat /tmp/scaffold-new.log
echo "::endgroup::"
echo "::group::tatolabd"
cat /tmp/tatolabd.log
echo "::endgroup::"

if ! grep -q 'Vulkan loader library opened.*_vulkan_driver/libvulkan.1.dylib' /tmp/tatolabd.log; then
  echo "::error::tatolabd did not open the Vulkan loader the lend carries"
  exit 1
fi
echo "::group::viewer"
cat /tmp/scaffold-viewer.log
echo "::endgroup::"
echo "::group::graph"
cat /tmp/scaffold-graph.json
echo "::endgroup::"
# The stream loaded, and its source wrote frames to the viewer. Far enough to
# prove the starter runs end to end, not a frame-rate bar: the runner's GPU is
# a paravirtual device.
frames_written_by() {
  node_name="$1"
  port_name="$2"
  python3 - "$node_name" "$port_name" <<'EOF_FRAMES_WRITTEN'
import json
import sys

node_name, port_name = sys.argv[1], sys.argv[2]
try:
    graph = json.load(open("/tmp/scaffold-graph.json"))
except ValueError:
    raise SystemExit(0)
streams = graph.get("streams", [])
for stream_graph in streams.values() if isinstance(streams, dict) else streams:
    for node in stream_graph.get("nodes", []):
        if node.get("name") == node_name:
            metrics = node.get("components", {}).get("metrics", {})
            print(metrics.get("written_bags_by_output_port", {}).get(port_name, ""))
EOF_FRAMES_WRITTEN
}
for node_and_port in testpatternsource/video; do
  node_name="${node_and_port%%/*}"
  port_name="${node_and_port#*/}"
  frames_written="$(frames_written_by "$node_name" "$port_name")"
  if [ -z "$frames_written" ] || [ "$frames_written" -lt 60 ]; then
    echo "::error::$node_name.$port_name wrote ${frames_written:-no} frames in 20s, by the stream's graph"
    exit 1
  fi
  echo "$node_name.$port_name wrote $frames_written frames"
done

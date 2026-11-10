#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# `tatolab new`'s sample stream — the test pattern through the Python effect,
# its brightness metered — created and run on this Mac's `tatolabd`, on the
# Vulkan driver the runtime unit's lend carries:
#
#   scripts/check_scaffolded_stream_runs_on_the_driver_the_macos_lend_carries.sh \
#     <runtime-unit-root> <tatolab-stream-wheel-directory>
#
# The project's venv holds what a user's `uv sync` gives it — `tatolab-stream`,
# from the wheel directory because no index carries this version yet, the
# `tatolab-testpattern` module from this checkout, and the numpy the scaffold
# declares — and no runtime. Passes when `tatolabd` opened the lend's Vulkan
# loader and the source and the effect each wrote frames by the stream's
# `graph`. Needs a Mac with no Vulkan driver of its own, or the claim is not
# proven.

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
/tmp/scaffold-check/.venv/bin/pip install 'numpy>=2.1'
# Pure Python; `--no-deps` keeps its `tatolab-stream` to this run's build.
/tmp/scaffold-check/.venv/bin/pip install --no-deps \
  "$(cd "$(dirname "$0")/.." && pwd)/packages/tatolab-testpattern"
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
sleep 20
"$tatolab_executable" graph > /tmp/scaffold-graph.json || true
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
echo "::group::graph"
cat /tmp/scaffold-graph.json
echo "::endgroup::"
# The stream loaded, and its source and effect wrote frames into the graph.
# Far enough to prove the graph runs end to end, not a frame-rate bar: the
# runner's GPU is a paravirtual device. `graph` counts a port's writes once
# something reads it, and the meter reads the effect's output.
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
for node_and_port in testpatternsource/video halftonelenseffect/video_to_downstream; do
  node_name="${node_and_port%%/*}"
  port_name="${node_and_port#*/}"
  frames_written="$(frames_written_by "$node_name" "$port_name")"
  if [ -z "$frames_written" ] || [ "$frames_written" -lt 60 ]; then
    echo "::error::$node_name.$port_name wrote ${frames_written:-no} frames in 20s, by the stream's graph"
    exit 1
  fi
  echo "$node_name.$port_name wrote $frames_written frames"
done

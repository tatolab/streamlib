#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# `tatolab new`'s sample stream — the test pattern through the Python effect
# into H.264 — created and run on this Mac's `tatolabd`, on the Vulkan driver
# the runtime unit's lend carries:
#
#   scripts/check_scaffolded_stream_runs_on_the_driver_the_macos_lend_carries.sh \
#     <runtime-unit-root> <tatolab-stream-wheel-directory>
#
# The project's venv holds what a user's `uv sync` gives it — `tatolab-stream`,
# from the wheel directory because no index carries this version yet, and the
# numpy the scaffold declares — and no runtime. Passes when `tatolabd` opened
# the lend's Vulkan loader, the source wrote frames by the stream's `graph`, and
# the encoder encoded or refused by name. Needs a Mac with no Vulkan driver of
# its own, or the claim is not proven.

set -eu

runtime_unit_root="${1:?usage: $(basename "$0") <runtime-unit-root> <tatolab-stream-wheel-directory>}"
tatolab_stream_wheel_directory="${2:?usage: $(basename "$0") <runtime-unit-root> <tatolab-stream-wheel-directory>}"
tatolabd_executable="$runtime_unit_root/bin/tatolabd"
tatolab_executable="$runtime_unit_root/bin/tatolab"

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

# The venv is made first, from this run's build, so the create's
# load finds an interpreter that runs and syncs nothing.
mkdir -p /tmp/scaffold-check
python3 -m venv /tmp/scaffold-check/.venv
# `--no-index` keeps `tatolab-stream` to this run's build, so its one
# dependency comes from PyPI first.
/tmp/scaffold-check/.venv/bin/pip install 'msgpack>=1.0'
/tmp/scaffold-check/.venv/bin/pip install --no-index --find-links "$tatolab_stream_wheel_directory" tatolab-stream
/tmp/scaffold-check/.venv/bin/pip install 'numpy>=2.1'
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
# The stream loaded, and its source wrote frames into the graph. Far
# enough to prove the graph runs end to end, not a frame-rate bar: the
# runner's GPU is a paravirtual device.
frames_written="$(python3 -c '
import json
try:
    graph = json.load(open("/tmp/scaffold-graph.json"))
except ValueError:
    raise SystemExit(0)
streams = graph.get("streams", [])
for stream_graph in streams.values() if isinstance(streams, dict) else streams:
    for node in stream_graph.get("nodes", []):
        if node.get("name") == "testpatternsource":
            metrics = node.get("components", {}).get("metrics", {})
            print(metrics.get("written_bags_by_output_port", {}).get("video", ""))
')"
if [ -z "$frames_written" ] || [ "$frames_written" -lt 60 ]; then
  echo "::error::TestPatternSource.video wrote ${frames_written:-no} frames in 20s, by the stream's graph"
  exit 1
fi
echo "TestPatternSource.video wrote $frames_written frames"
# The encoder either encodes, or — on a runner with no encoder — refuses
# naming itself while the stream stays loaded. Its own count, logged at
# teardown: `graph` counts a port's writes only once something reads
# it, and nothing subscribes to the track here.
frames_encoded="$(grep -o 'H264Encoder: teardown .*frames_encoded=[0-9]*' /tmp/tatolabd.log \
  | grep -o 'frames_encoded=[0-9]*' | cut -d= -f2 | sort -n | tail -1)"
if [ -n "$frames_encoded" ] && [ "$frames_encoded" -gt 0 ]; then
  echo "H264Encoder encoded $frames_encoded frames"
elif grep -q 'H264Encoder: failed to mint the encoder session' /tmp/tatolabd.log; then
  echo "H264Encoder refused by name on a runner with no encoder; the stream stayed loaded"
else
  echo "::error::H264Encoder neither encoded a frame nor refused by name"
  exit 1
fi
# Frames the encoder lost to backpressure are a normal event on a realtime
# link, so they are reported from `graph`'s drop count, never asserted.
frames_dropped_into_the_encoder="$(python3 -c '
import json
try:
    graph = json.load(open("/tmp/scaffold-graph.json"))
except ValueError:
    raise SystemExit(0)
streams = graph.get("streams", [])
for stream_graph in streams.values() if isinstance(streams, dict) else streams:
    for node in stream_graph.get("nodes", []):
        if node.get("name") == "h264encoder":
            metrics = node.get("components", {}).get("metrics", {})
            print(sum(metrics.get("dropped_bags_by_link", {}).values()))
')"
echo "H264Encoder.video dropped ${frames_dropped_into_the_encoder:-an unknown number of} frames, by the stream's graph"

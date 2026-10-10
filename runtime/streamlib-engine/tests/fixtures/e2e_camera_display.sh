#!/bin/bash
# E2E test: the camera-display stream (`camera_display_stream.py`, beside this
# script) on a vivid virtual camera.
#
# Starts the runtime unit's `tatolabd`, loads the stream into it with `tatolab
# run --name`, proves it live through the control plane, captures its window,
# then stops the stream with SIGTERM to `tatolab run` and the runtime with
# SIGINT.
#
# Assertions ride the plan's durable contracts — the `graph` tool's JSON, the
# JSONL log schema, and a captured PNG — never engine tracing prose, which is
# renamed without notice and leaves a grep on it passing vacuously.
#
# Validates:
#   - The stream loads and answers `graph --stream`
#   - Both native built-ins are in the graph, linked camera → window
#   - The window renders (PNG captured and non-trivial)
#   - No Vulkan allocation / device-loss / process() failure in the logs
#   - SIGTERM to `tatolab run` unloads the stream, and SIGINT to `tatolabd`
#     tears the engine down, each cleanly
#
# Prerequisites:
#   - vivid kernel module available: sudo modprobe vivid
#   - the runtime unit: `cargo xtask build-runtime` (see fixture_runtime_unit.sh)
#   - no runtime holding this machine: the fixture starts its own
#   - uv, to make the fixture venv on first use
#   - xdotool + xwd + python3-PIL for the window capture
#
# Exit codes: 0 = pass, 1 = fail, 77 = skip

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=fixture_runtime_unit.sh
. "$SCRIPT_DIR/fixture_runtime_unit.sh"
STREAM_ENTRY_FILE_NAME="camera_display_stream.py"
STREAM_NAME="camera_display_fixture"
OUTPUT_DIR="${1:-/tmp/streamlib-e2e}"
# Bounds the stream's compile, description and load, until it answers.
LOAD_SECS="${LOAD_SECS:-60}"
WINDOW_TITLE="StreamLib Camera Display"

rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"
PNG_DIR="$OUTPUT_DIR/png_samples"
mkdir -p "$PNG_DIR"
# The runtime's output: the engine's log mirror, the stream's records among it.
LOG_FILE="$OUTPUT_DIR/pipeline.log"
STREAM_RUN_LOG_FILE="$OUTPUT_DIR/stream_run.log"
GRAPH_FILE="$OUTPUT_DIR/graph.json"
STREAM_RUN_PID=""

cleanup() {
    if [ -n "$STREAM_RUN_PID" ] && kill -0 "$STREAM_RUN_PID" 2>/dev/null; then
        kill -TERM "$STREAM_RUN_PID" 2>/dev/null || true
        sleep 2
        kill -KILL "$STREAM_RUN_PID" 2>/dev/null || true
    fi
    stop_the_fixture_runtime
}
trap cleanup EXIT

# ── Prerequisites ────────────────────────────────────────────────────
echo "[e2e] Checking prerequisites..."

require_the_runtime_unit
require_the_fixture_venv
echo "[e2e] Runtime unit: $RUNTIME_UNIT_DIRECTORY"

if ! command -v xdotool &>/dev/null; then
    echo "[e2e] SKIP: xdotool not installed (needed to find the window)"
    exit 77
fi

# ImageMagick's `import` grabs and encodes in one step. capture_window.py is the
# fallback, and it needs PIL in whichever `python3` wins on PATH — which is not
# the fixture venv, so it is the less portable of the two.
if command -v import &>/dev/null; then
    CAPTURE_WITH="import"
elif command -v xwd &>/dev/null && python3 -c "import PIL" 2>/dev/null; then
    CAPTURE_WITH="capture_window.py"
else
    echo "[e2e] SKIP: no window capture available (install imagemagick, or xwd + python3-pil)"
    exit 77
fi
echo "[e2e] Window capture: $CAPTURE_WITH"

if [ -z "${DISPLAY:-}" ]; then
    echo "[e2e] SKIP: no \$DISPLAY — this fixture opens a real window"
    exit 77
fi

# ── Load vivid virtual camera ───────────────────────────────────────
# vivid is an in-kernel V4L2 test driver — no DKMS or out-of-tree modules.
if ! lsmod | grep -q vivid; then
    echo "[e2e] Loading vivid kernel module..."
    if ! sudo modprobe vivid 2>/dev/null; then
        echo "[e2e] SKIP: vivid module not available (check kernel config)"
        exit 77
    fi
fi

VIRTUAL_DEVICE=""
for dev in $(v4l2-ctl --list-devices 2>/dev/null | awk '/vivid/{getline; print $1}'); do
    if v4l2-ctl -d "$dev" --info 2>/dev/null | grep -q "Video Capture"; then
        VIRTUAL_DEVICE="$dev"
        break
    fi
done

if [ -z "$VIRTUAL_DEVICE" ]; then
    echo "[e2e] SKIP: no vivid capture device found"
    exit 77
fi
echo "[e2e] Using vivid capture device: $VIRTUAL_DEVICE"

# ── Start the runtime and load the stream ────────────────────────────
# No build step here: the stream is Python and the runtime unit is already
# built, so nothing sits between an edit of the stream file and this run. The
# camera and the log filter are the runtime's environment, which the stream's
# compile and its processor interpreters inherit. `--dir` anchors the load at
# this directory, so the stream compiles in the fixture venv and the
# cross-floor check reads these fixtures rather than whatever directory the
# script was started from.
echo "[e2e] Starting the runtime..."
if ! STREAMLIB_CAMERA_DEVICE="$VIRTUAL_DEVICE" \
    RUST_LOG="${RUST_LOG:-warn,streamlib=info}" \
        start_the_fixture_runtime "$LOG_FILE" "$OUTPUT_DIR/runtime_state"; then
    echo "[e2e] FAIL: the fixture could not start its runtime"
    exit 1
fi
echo "[e2e] Loading $SCRIPT_DIR/$STREAM_ENTRY_FILE_NAME as $STREAM_NAME with \`tatolab run\`..."
"$TATOLAB_EXECUTABLE" run --dir "$SCRIPT_DIR" --name "$STREAM_NAME" "$STREAM_ENTRY_FILE_NAME" \
    >"$STREAM_RUN_LOG_FILE" 2>&1 &
STREAM_RUN_PID=$!

# ── Wait for the stream to answer ────────────────────────────────────
# The stream answers `graph --stream` only once its graph has loaded, so that
# is its own liveness signal — not a fixed sleep — and the name it was loaded
# under is what every later verb addresses.
if ! wait_until_the_stream_answers "$STREAM_NAME" "$STREAM_RUN_PID" "$LOAD_SECS"; then
    echo "[e2e] FAIL: the stream did not answer within ${LOAD_SECS}s"
    tail -30 "$STREAM_RUN_LOG_FILE"
    tail -30 "$LOG_FILE"
    exit 1
fi
echo "[e2e] Stream answering: $STREAM_NAME"

# ── Graph assertions ─────────────────────────────────────────────────
tatolab_observation_verb graph --stream "$STREAM_NAME" >"$GRAPH_FILE" 2>/dev/null || true

GRAPH_VERDICT="$(python3 - "$GRAPH_FILE" <<'PYEOF'
import json
import sys

try:
    with open(sys.argv[1]) as handle:
        graph = json.load(handle)
except Exception as failure:
    print(f"unreadable: {failure}")
    raise SystemExit(0)

nodes = graph.get("nodes", [])
links = graph.get("links", [])


def node_name_of(type_fragment):
    for node in nodes:
        if type_fragment in node.get("type", ""):
            return node.get("name")
    return None


camera_name = node_name_of("CameraSource")
window_name = node_name_of("DisplayWindow")

missing = [
    name
    for name, found in (("CameraSource", camera_name), ("DisplayWindow", window_name))
    if found is None
]
if missing:
    print(f"missing {', '.join(missing)} in {sorted(n.get('type', '') for n in nodes)}")
    raise SystemExit(0)

# The direction and both port names, not merely "some link exists" — a reversed
# link, a link to an unrelated node, or one on the wrong port is exactly
# the wiring bug this fixture is here to catch.
wired = [
    link
    for link in links
    if link.get("source", {}).get("node") == camera_name
    and link.get("source", {}).get("port") == "video"
    and link.get("target", {}).get("node") == window_name
    and link.get("target", {}).get("port") == "video"
]
if not wired:
    present = [
        f"{link.get('source', {}).get('node')}"
        f":{link.get('source', {}).get('port')}"
        f" -> {link.get('target', {}).get('node')}"
        f":{link.get('target', {}).get('port')}"
        for link in links
    ]
    print(f"no CameraSource:video -> DisplayWindow:video link; found {present}")
else:
    print("ok")
PYEOF
)"

# ── Capture the window ───────────────────────────────────────────────
# Let the swapchain settle and frames actually present before the grab.
sleep 5
WINDOW_ID="$(xdotool search --name "$WINDOW_TITLE" 2>/dev/null | head -1)"
PNG_PATH="$PNG_DIR/window.png"
if [ -n "$WINDOW_ID" ]; then
    echo "[e2e] Capturing window $WINDOW_ID with $CAPTURE_WITH..."
    if [ "$CAPTURE_WITH" = "import" ]; then
        import -window "$WINDOW_ID" "$PNG_PATH" || true
    else
        python3 "$SCRIPT_DIR/capture_window.py" "$WINDOW_ID" "$PNG_PATH" || true
    fi
else
    echo "[e2e] No window matched '$WINDOW_TITLE'"
fi

# ── Stop the stream, then the runtime ────────────────────────────────
# SIGTERM makes `tatolab run` stop its stream, which the runtime unloads before
# the run exits; SIGINT then makes `tatolabd`'s signal ladder tear the engine
# down. A clean exit of each IS the gate.
echo "[e2e] Stopping the stream (SIGTERM to tatolab run)..."
kill -TERM "$STREAM_RUN_PID" 2>/dev/null || true
SHUTDOWN_STATUS="timeout"
for _ in $(seq 1 15); do
    if ! kill -0 "$STREAM_RUN_PID" 2>/dev/null; then
        SHUTDOWN_STATUS="clean"
        break
    fi
    sleep 1
done
# Reap only a process that is actually dying. A run that ignores SIGTERM would
# otherwise block `wait` forever — and the EXIT trap cannot fire while we are
# blocked in it, so the fixture would hang instead of reporting the failure it
# just detected. Hanging CI is strictly worse than a FAIL.
if [ "$SHUTDOWN_STATUS" = "timeout" ]; then
    echo "[e2e] tatolab run ignored SIGTERM for 15s — escalating to SIGKILL."
    kill -KILL "$STREAM_RUN_PID" 2>/dev/null || true
fi
wait "$STREAM_RUN_PID" 2>/dev/null || true
STREAM_RUN_PID=""
echo "[e2e] Stopping the runtime (SIGINT to tatolabd)..."
stop_the_fixture_runtime 15
if ! RUNTIME_STOP_REASON="$(the_fixture_runtime_stopped_cleanly 2>&1)"; then
    SHUTDOWN_STATUS="$SHUTDOWN_STATUS; $RUNTIME_STOP_REASON"
fi

# ── Analyze results ──────────────────────────────────────────────────
count_in_log() { grep -c "$1" "$LOG_FILE" 2>/dev/null || true; }

VK_OOM="$(count_in_log 'OUT_OF_DEVICE_MEMORY')"
VK_DEVICE_LOST="$(count_in_log 'DEVICE_LOST')"
PROCESS_FAILED="$(count_in_log 'process() failed')"
VALIDATION_ERRORS="$(count_in_log 'Validation Error')"
if [ -s "$PNG_PATH" ]; then
    PNG_BYTES="$(stat -c%s "$PNG_PATH")"
else
    PNG_BYTES=0
fi

echo ""
echo "══════════════════════════════════════════════════════════════"
echo "  E2E camera-display (Python stream) Results"
echo "══════════════════════════════════════════════════════════════"
echo "  Virtual device:        $VIRTUAL_DEVICE (vivid)"
echo "  Stream:                $STREAM_NAME"
echo "  Graph:                 $GRAPH_VERDICT"
echo "  Window PNG:            $PNG_BYTES bytes ($PNG_PATH)"
echo "  Stop (run, runtime):   $SHUTDOWN_STATUS"
echo "  OUT_OF_DEVICE_MEMORY:  $VK_OOM"
echo "  DEVICE_LOST:           $VK_DEVICE_LOST"
echo "  process() failed:      $PROCESS_FAILED"
echo "  Validation Error:      $VALIDATION_ERRORS"
echo "  Output dir:            $OUTPUT_DIR"
echo "══════════════════════════════════════════════════════════════"

PASS=true

if [ "$GRAPH_VERDICT" != "ok" ]; then
    echo "[e2e] FAIL: graph — $GRAPH_VERDICT"
    PASS=false
fi
# A window that never rendered writes nothing; a black frame still writes a
# plausible PNG, so this gate is a floor. The visual read is the real gate —
# see the /verify-live audit checklist.
if [ "$PNG_BYTES" -lt 1024 ]; then
    echo "[e2e] FAIL: no usable window capture"
    PASS=false
fi
if [ "$SHUTDOWN_STATUS" != "clean" ]; then
    echo "[e2e] FAIL: the stop was not clean — $SHUTDOWN_STATUS"
    PASS=false
fi
if [ "$VK_OOM" -gt 0 ]; then
    echo "[e2e] FAIL: $VK_OOM OUT_OF_DEVICE_MEMORY"
    PASS=false
fi
if [ "$VK_DEVICE_LOST" -gt 0 ]; then
    echo "[e2e] FAIL: $VK_DEVICE_LOST DEVICE_LOST"
    PASS=false
fi
if [ "$PROCESS_FAILED" -gt 0 ]; then
    echo "[e2e] FAIL: $PROCESS_FAILED process() failures"
    PASS=false
fi

if [ "$PASS" = true ]; then
    echo "[e2e] RESULT: PASS"
    echo "[e2e] Read $PNG_PATH and describe it — a black frame with clean logs IS a regression."
    exit 0
else
    echo "[e2e] RESULT: FAIL"
    echo "[e2e] Last 30 lines of the runtime's log:"
    tail -30 "$LOG_FILE"
    exit 1
fi

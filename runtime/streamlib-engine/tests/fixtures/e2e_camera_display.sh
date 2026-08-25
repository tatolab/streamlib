#!/bin/bash
# E2E test: the `examples/camera-display` Python app on a vivid virtual camera.
#
# Boots the app with `streamlib run`, proves it live through the control plane,
# captures its window, and stops it with SIGTERM.
#
# Assertions ride the plan's durable contracts — the `graph` tool's JSON, the
# JSONL log schema, and a captured PNG — never engine tracing prose. The prose
# this fixture used to grep ("Ring textures created", "First frame captured")
# was renamed out from under it and the gate went vacuous; contracts do not
# move that way.
#
# Validates:
#   - The app's node registers a control plane and answers `graph`
#   - Both native built-ins are in the graph, linked camera → window
#   - The window renders (PNG captured and non-trivial)
#   - No Vulkan allocation / device-loss / process() failure in the logs
#   - SIGTERM tears the pipeline down cleanly
#
# Prerequisites:
#   - vivid kernel module available: sudo modprobe vivid
#   - `streamlib` on PATH (or a built wheel venv in the checkout)
#   - xdotool + xwd + python3-PIL for the window capture
#
# Exit codes: 0 = pass, 1 = fail, 77 = skip

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
APP_DIR="$REPO_ROOT/examples/camera-display"
OUTPUT_DIR="${1:-/tmp/streamlib-e2e}"
# Long enough for the swapchain to settle and several frames to present.
RUN_SECS="${RUN_SECS:-20}"
WINDOW_TITLE="StreamLib Camera Display"

rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"
PNG_DIR="$OUTPUT_DIR/png_samples"
mkdir -p "$PNG_DIR"
LOG_FILE="$OUTPUT_DIR/pipeline.log"
GRAPH_FILE="$OUTPUT_DIR/graph.json"
NODE_PID=""

cleanup() {
    if [ -n "$NODE_PID" ] && kill -0 "$NODE_PID" 2>/dev/null; then
        kill -TERM "$NODE_PID" 2>/dev/null || true
        sleep 2
        kill -KILL "$NODE_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# ── Prerequisites ────────────────────────────────────────────────────
echo "[e2e] Checking prerequisites..."

# The CLI ships inside the wheel, so `streamlib` on PATH means an installed
# wheel. Fall back to the checkout's own wheel venv, which is what a session
# working in-tree has.
STREAMLIB="${STREAMLIB_BIN:-streamlib}"
if ! command -v "$STREAMLIB" >/dev/null 2>&1; then
    STREAMLIB="$REPO_ROOT/sdk/streamlib-python-wheel/.venv/bin/streamlib"
    if [ ! -x "$STREAMLIB" ]; then
        echo "[e2e] SKIP: no streamlib CLI on PATH and no wheel venv in the checkout"
        echo "[e2e]       build one with: maturin develop --manifest-path sdk/streamlib-python-wheel/Cargo.toml"
        exit 77
    fi
fi
echo "[e2e] streamlib CLI: $STREAMLIB"

if ! command -v xdotool &>/dev/null; then
    echo "[e2e] SKIP: xdotool not installed (needed to find the window)"
    exit 77
fi

# ImageMagick's `import` grabs and encodes in one step. capture_window.py is the
# fallback, and it needs PIL in whichever `python3` wins on PATH — which is not
# the wheel venv, so it is the less portable of the two.
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

# ── Boot the app ─────────────────────────────────────────────────────
# No build step: the wheel carries the engine, and the app is Python. There is
# nothing between an edit of app.py and this run.
echo "[e2e] Booting $APP_DIR with \`streamlib run\` (${RUN_SECS}s)..."
STREAMLIB_CAMERA_DEVICE="$VIRTUAL_DEVICE" \
RUST_LOG="${RUST_LOG:-warn,streamlib=info}" \
    "$STREAMLIB" run --dir "$APP_DIR" >"$LOG_FILE" 2>&1 &
NODE_PID=$!

# ── Wait for the node to register ────────────────────────────────────
# The registry entry is published after `setup` builds the graph, so its
# appearance is the app's own liveness signal — not a fixed sleep.
RUNTIME_ID=""
for _ in $(seq 1 "$RUN_SECS"); do
    if ! kill -0 "$NODE_PID" 2>/dev/null; then
        echo "[e2e] FAIL: the app exited before registering a node"
        tail -30 "$LOG_FILE"
        exit 1
    fi
    RUNTIME_ID="$("$STREAMLIB" nodes 2>/dev/null | awk 'NR>1 && $4=="yes" {print $1; exit}')"
    [ -n "$RUNTIME_ID" ] && break
    sleep 1
done

if [ -z "$RUNTIME_ID" ]; then
    echo "[e2e] FAIL: no live node registered within ${RUN_SECS}s"
    tail -30 "$LOG_FILE"
    exit 1
fi
echo "[e2e] Node registered: $RUNTIME_ID"

# ── Graph assertions ─────────────────────────────────────────────────
"$STREAMLIB" graph --node "$RUNTIME_ID" >"$GRAPH_FILE" 2>/dev/null || true

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


def processor_id_of(type_fragment):
    for node in nodes:
        if type_fragment in node.get("type", ""):
            return node.get("id")
    return None


camera_id = processor_id_of("CameraSource")
window_id = processor_id_of("DisplayWindow")

missing = [
    name
    for name, found in (("CameraSource", camera_id), ("DisplayWindow", window_id))
    if found is None
]
if missing:
    print(f"missing {', '.join(missing)} in {sorted(n.get('type', '') for n in nodes)}")
    raise SystemExit(0)

# The direction and both port names, not merely "some link exists" — a reversed
# link, a link to an unrelated processor, or one on the wrong port is exactly
# the wiring bug this fixture is here to catch.
wired = [
    link
    for link in links
    if link.get("source", {}).get("processor_id") == camera_id
    and link.get("source", {}).get("port_name") == "video"
    and link.get("target", {}).get("processor_id") == window_id
    and link.get("target", {}).get("port_name") == "video"
]
if not wired:
    present = [
        f"{link.get('source', {}).get('processor_id')}"
        f":{link.get('source', {}).get('port_name')}"
        f" -> {link.get('target', {}).get('processor_id')}"
        f":{link.get('target', {}).get('port_name')}"
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

# ── Stop the node ────────────────────────────────────────────────────
# `rt.run()` owns SIGTERM and tears the engine down before the interpreter
# finalizes — the interpreter-lifecycle contract. A clean exit IS the gate.
echo "[e2e] Stopping the node (SIGTERM)..."
kill -TERM "$NODE_PID" 2>/dev/null || true
SHUTDOWN_STATUS="timeout"
for _ in $(seq 1 15); do
    if ! kill -0 "$NODE_PID" 2>/dev/null; then
        SHUTDOWN_STATUS="clean"
        break
    fi
    sleep 1
done
# Reap only a process that is actually dying. A node that ignores SIGTERM would
# otherwise block `wait` forever — and the EXIT trap cannot fire while we are
# blocked in it, so the fixture would hang instead of reporting the failure it
# just detected. Hanging CI is strictly worse than a FAIL.
if [ "$SHUTDOWN_STATUS" = "timeout" ]; then
    echo "[e2e] Node ignored SIGTERM for 15s — escalating to SIGKILL."
    kill -KILL "$NODE_PID" 2>/dev/null || true
fi
wait "$NODE_PID" 2>/dev/null || true
NODE_PID=""

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
echo "  E2E camera-display (Python app) Results"
echo "══════════════════════════════════════════════════════════════"
echo "  Virtual device:        $VIRTUAL_DEVICE (vivid)"
echo "  Runtime id:            $RUNTIME_ID"
echo "  Graph:                 $GRAPH_VERDICT"
echo "  Window PNG:            $PNG_BYTES bytes ($PNG_PATH)"
echo "  Shutdown on SIGTERM:   $SHUTDOWN_STATUS"
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
    echo "[e2e] FAIL: the node did not exit within 15s of SIGTERM"
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
    echo "[e2e] Last 30 lines of pipeline log:"
    tail -30 "$LOG_FILE"
    exit 1
fi

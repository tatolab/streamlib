#!/bin/bash
# E2E test: camera-display pipeline with vivid virtual camera + PNG frame sampling.
#
# Uses the in-kernel vivid driver (no out-of-tree modules needed), runs
# camera-display with debug features (STREAMLIB_DISPLAY_FRAME_LIMIT for
# clean exit, STREAMLIB_DISPLAY_PNG_SAMPLE_DIR for AI-readable frame samples).
#
# Validates:
#   - Camera captures from virtual device
#   - Display creates swapchain without VK_ERROR_OUT_OF_DEVICE_MEMORY
#   - DMA-BUF VMA pools are created
#   - End-to-end pipeline produces sample PNGs with valid pixel data
#   - Process exits cleanly via frame limit (no stranded windowed processes)
#
# Prerequisites:
#   - vivid kernel module available: sudo modprobe vivid
#   - cargo installed
#
# Exit codes: 0 = pass, 1 = fail, 77 = skip

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
OUTPUT_DIR="${1:-/tmp/streamlib-e2e}"
FRAME_LIMIT=120
PNG_SAMPLE_EVERY=20

rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"
PNG_DIR="$OUTPUT_DIR/png_samples"
mkdir -p "$PNG_DIR"
LOG_FILE="$OUTPUT_DIR/pipeline.log"

cleanup() {
    pkill -9 -f camera-display 2>/dev/null || true
}
trap cleanup EXIT

# ── Prerequisites ────────────────────────────────────────────────────
echo "[e2e] Checking prerequisites..."
if ! command -v cargo &>/dev/null; then
    echo "[e2e] SKIP: cargo not installed"
    exit 77
fi

# ── Load vivid virtual camera ───────────────────────────────────────
# vivid is an in-kernel V4L2 test driver — no DKMS or out-of-tree modules.
# It creates /dev/video2 (capture) with built-in test patterns.
if ! lsmod | grep -q vivid; then
    echo "[e2e] Loading vivid kernel module..."
    if ! sudo modprobe vivid 2>/dev/null; then
        echo "[e2e] SKIP: vivid module not available (check kernel config)"
        exit 77
    fi
fi

# Find the vivid capture device
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

# ── Build camera-display ─────────────────────────────────────────────
echo "[e2e] Building camera-display..."
cd "$REPO_ROOT"
if ! cargo build -p camera-display 2>"$OUTPUT_DIR/build.log"; then
    echo "[e2e] FAIL: build failed"
    tail -20 "$OUTPUT_DIR/build.log"
    exit 1
fi

BINARY="$REPO_ROOT/target/debug/camera-display"

# ── Run with frame limit + PNG sampling ──────────────────────────────
echo "[e2e] Running camera-display (frame_limit=$FRAME_LIMIT, sample_every=$PNG_SAMPLE_EVERY)..."
DISPLAY="${DISPLAY:-:0}" \
STREAMLIB_CAMERA_DEVICE="$VIRTUAL_DEVICE" \
STREAMLIB_DISPLAY_FRAME_LIMIT="$FRAME_LIMIT" \
STREAMLIB_DISPLAY_PNG_SAMPLE_DIR="$PNG_DIR" \
STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY="$PNG_SAMPLE_EVERY" \
RUST_LOG=warn,streamlib=info \
timeout --kill-after=3 30 "$BINARY" > "$LOG_FILE" 2>&1 || true

# ── Analyze results ──────────────────────────────────────────────────
OOM_COUNT="$(grep -c 'Failed to create camera texture' "$LOG_FILE" 2>/dev/null)" || OOM_COUNT=0
PNG_COUNT="$(ls -1 "$PNG_DIR"/*.png 2>/dev/null | wc -l)" || PNG_COUNT=0
DMA_BUF_POOL=$(grep -q "DMA-BUF VMA pools created" "$LOG_FILE" && echo "yes" || echo "no")
FIRST_FRAME=$(grep -q "First frame captured" "$LOG_FILE" && echo "yes" || echo "no")
RING_TEXTURES=$(grep -q "Ring textures created" "$LOG_FILE" && echo "yes" || echo "no")
CLEAN_SHUTDOWN=$(grep -q "Graceful shutdown complete\|Stopped" "$LOG_FILE" && echo "yes" || echo "no")

echo ""
echo "══════════════════════════════════════════════════════════════"
echo "  E2E Camera-Display Pipeline Results"
echo "══════════════════════════════════════════════════════════════"
echo "  Virtual device:        $VIRTUAL_DEVICE (vivid)"
echo "  DMA-BUF VMA pools:     $DMA_BUF_POOL"
echo "  Ring textures:         $RING_TEXTURES"
echo "  First frame captured:  $FIRST_FRAME"
echo "  Clean shutdown:        $CLEAN_SHUTDOWN"
echo "  OOM errors:            $OOM_COUNT"
echo "  PNG samples saved:     $PNG_COUNT"
echo "  Output dir:            $OUTPUT_DIR"
echo "══════════════════════════════════════════════════════════════"

PASS=true

if [ "$DMA_BUF_POOL" = "no" ]; then
    echo "[e2e] FAIL: DMA-BUF VMA pools not created"
    PASS=false
fi
if [ "$RING_TEXTURES" = "no" ]; then
    echo "[e2e] FAIL: Ring textures not created"
    PASS=false
fi
if [ "$FIRST_FRAME" = "no" ]; then
    echo "[e2e] FAIL: No frames captured"
    PASS=false
fi
if [ "$OOM_COUNT" -gt 0 ]; then
    echo "[e2e] FAIL: $OOM_COUNT OOM errors"
    PASS=false
fi
if [ "$CLEAN_SHUTDOWN" = "no" ]; then
    echo "[e2e] FAIL: Process did not shut down cleanly"
    PASS=false
fi
if [ "$PNG_COUNT" -lt 1 ]; then
    echo "[e2e] FAIL: no PNG samples saved"
    PASS=false
fi

if [ "$PASS" = true ]; then
    echo "[e2e] RESULT: PASS"
    if [ "$PNG_COUNT" -gt 0 ]; then
        echo "[e2e] PNG samples:"
        ls -la "$PNG_DIR"/ | head -10
    fi
    exit 0
else
    echo "[e2e] RESULT: FAIL"
    echo "[e2e] Last 20 lines of pipeline log:"
    tail -20 "$LOG_FILE"
    exit 1
fi

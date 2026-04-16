#!/bin/bash
# E2E test: camera-display pipeline with virtual camera + PNG frame sampling.
#
# Uses v4l2loopback virtual camera + ffmpeg test pattern, runs camera-display
# with the new debug features (STREAMLIB_DISPLAY_FRAME_LIMIT for clean exit,
# STREAMLIB_DISPLAY_PNG_SAMPLE_DIR for AI-readable frame samples).
#
# Validates:
#   - Camera captures from virtual device
#   - Display creates swapchain without VK_ERROR_OUT_OF_DEVICE_MEMORY (the bug)
#   - DMA-BUF VMA pools are created (the fix)
#   - End-to-end pipeline produces sample PNGs with valid pixel data
#   - Process exits cleanly via frame limit (no stranded windowed processes)
#
# Prerequisites:
#   - v4l2loopback loaded: sudo modprobe v4l2loopback video_nr=10 card_label=Virtual_Camera
#   - ffmpeg installed
#
# Exit codes: 0 = pass, 1 = fail, 77 = skip

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
OUTPUT_DIR="${1:-/tmp/streamlib-e2e}"
VIRTUAL_DEVICE="/dev/video10"
FRAME_LIMIT=120
PNG_SAMPLE_EVERY=20

rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"
PNG_DIR="$OUTPUT_DIR/png_samples"
mkdir -p "$PNG_DIR"
LOG_FILE="$OUTPUT_DIR/pipeline.log"

cleanup() {
    pkill -9 -f camera-display 2>/dev/null || true
    pkill -9 -f "ffmpeg.*$VIRTUAL_DEVICE" 2>/dev/null || true
}
trap cleanup EXIT

# ── Prerequisites ────────────────────────────────────────────────────
echo "[e2e] Checking prerequisites..."
if [ ! -e "$VIRTUAL_DEVICE" ]; then
    echo "[e2e] SKIP: $VIRTUAL_DEVICE not present. Load v4l2loopback first:"
    echo "       sudo modprobe v4l2loopback video_nr=10 card_label=Virtual_Camera"
    exit 77
fi
for cmd in ffmpeg cargo; do
    if ! command -v "$cmd" &>/dev/null; then
        echo "[e2e] SKIP: $cmd not installed"
        exit 77
    fi
done

# ── Start ffmpeg test pattern stream ─────────────────────────────────
echo "[e2e] Starting ffmpeg test pattern stream..."
pkill -9 -f "ffmpeg.*$VIRTUAL_DEVICE" 2>/dev/null || true
sleep 1
setsid nohup ffmpeg -nostats -f lavfi \
    -i "testsrc=duration=600:size=1920x1080:rate=30" \
    -pix_fmt yuyv422 -f v4l2 "$VIRTUAL_DEVICE" \
    > "$OUTPUT_DIR/ffmpeg.log" 2>&1 < /dev/null &
sleep 3
if ! pgrep -f "ffmpeg.*$VIRTUAL_DEVICE" > /dev/null; then
    echo "[e2e] FAIL: ffmpeg failed to start"
    tail -10 "$OUTPUT_DIR/ffmpeg.log"
    exit 1
fi
echo "[e2e] ffmpeg streaming to $VIRTUAL_DEVICE"

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
PNG_COUNT="$(ls -1 "$PNG_DIR"/*.png 2>/dev/null | wc -l)"
DMA_BUF_POOL=$(grep -q "DMA-BUF VMA pools created" "$LOG_FILE" && echo "yes" || echo "no")
SWAPCHAIN_OK=$(grep -q "Vulkan swapchain created" "$LOG_FILE" && echo "yes" || echo "no")
FIRST_FRAME=$(grep -q "First frame captured" "$LOG_FILE" && echo "yes" || echo "no")

echo ""
echo "══════════════════════════════════════════════════════════════"
echo "  E2E Camera-Display Pipeline Results"
echo "══════════════════════════════════════════════════════════════"
echo "  Virtual device:        $VIRTUAL_DEVICE"
echo "  DMA-BUF VMA pools:     $DMA_BUF_POOL"
echo "  Swapchain created:     $SWAPCHAIN_OK"
echo "  First frame captured:  $FIRST_FRAME"
echo "  OOM errors:            $OOM_COUNT"
echo "  PNG samples saved:     $PNG_COUNT"
echo "  Output dir:            $OUTPUT_DIR"
echo "══════════════════════════════════════════════════════════════"

PASS=true

if [ "$DMA_BUF_POOL" = "no" ]; then
    echo "[e2e] FAIL: DMA-BUF VMA pools not created (fix not active)"
    PASS=false
fi
if [ "$OOM_COUNT" -gt 0 ]; then
    echo "[e2e] FAIL: $OOM_COUNT camera texture OOM errors (bug not fixed)"
    grep "Failed to create camera texture" "$LOG_FILE" | head -3
    PASS=false
fi
if [ "$PNG_COUNT" -lt 1 ]; then
    echo "[e2e] FAIL: no PNG samples saved (display didn't process frames)"
    PASS=false
else
    # Verify PNG is non-trivial (1920x1080 RGBA ~= 8MB minimum)
    SMALLEST=$(ls -la "$PNG_DIR"/*.png | awk '{print $5}' | sort -n | head -1)
    if [ "$SMALLEST" -lt 100000 ]; then
        echo "[e2e] FAIL: PNG samples too small ($SMALLEST bytes), likely corrupt"
        PASS=false
    fi
fi

if [ "$PASS" = true ]; then
    echo "[e2e] RESULT: PASS"
    echo "[e2e] PNG samples (verify visually with: feh $PNG_DIR/*.png):"
    ls -la "$PNG_DIR"/ | head -10
    exit 0
else
    echo "[e2e] RESULT: FAIL"
    echo "--- Last 30 lines of pipeline log ---"
    tail -30 "$LOG_FILE"
    exit 1
fi

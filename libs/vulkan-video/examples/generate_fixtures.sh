#!/usr/bin/env bash
# Generate BGRA fixture files for the H.264 and H.265 codec examples.
#
# Uses testsrc2 which includes SMPTE bars, a timecode overlay, frame counter,
# and animated elements — all built-in, no external libs (libfreetype) needed.
#
# Output: 10 seconds of 1920x1080@60fps raw BGRA frames per example.
#
# Usage:
#   ./generate_fixtures.sh

set -euo pipefail

WIDTH=1920
HEIGHT=1080
FPS=60
DURATION=10
FRAME_COUNT=$((FPS * DURATION))
FRAME_BYTES=$((WIDTH * HEIGHT * 4))
EXPECTED_SIZE=$((FRAME_BYTES * FRAME_COUNT))

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

generate() {
    local dir="$1"
    local fixture_dir="${SCRIPT_DIR}/${dir}/fixtures"
    local fixture_path="${fixture_dir}/smpte_1080p60.bgra"

    mkdir -p "$fixture_dir"

    if [ -f "$fixture_path" ]; then
        actual_size=$(stat -c%s "$fixture_path" 2>/dev/null || stat -f%z "$fixture_path" 2>/dev/null)
        if [ "$actual_size" -eq "$EXPECTED_SIZE" ]; then
            echo "[${dir}] Fixture already exists (${actual_size} bytes), skipping."
            return
        fi
        echo "[${dir}] Fixture size mismatch (${actual_size} != ${EXPECTED_SIZE}), regenerating..."
    fi

    echo "[${dir}] Generating ${WIDTH}x${HEIGHT}@${FPS}fps, ${DURATION}s BGRA fixture..."
    ffmpeg -y \
        -f lavfi \
        -i "testsrc2=size=${WIDTH}x${HEIGHT}:rate=${FPS}:duration=${DURATION}" \
        -frames:v "$FRAME_COUNT" \
        -pix_fmt bgra \
        -f rawvideo \
        "$fixture_path"

    echo "[${dir}] Done: $(du -h "$fixture_path" | cut -f1) → ${fixture_path}"
}

generate "h264-codec"
generate "h265-codec"

echo ""
echo "All fixtures ready. Run the examples with:"
echo "  cd examples/h264-codec && cargo run --release"
echo "  cd examples/h265-codec && cargo run --release"

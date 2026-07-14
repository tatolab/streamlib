#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Vivid color-roundtrip regression gate.
#
# Sister fixture to e2e_fixture_psnr.sh. Where that rig measures
# encode/decode quality against checked-in reference PNGs via PSNR,
# this one guards the V4L2 color path against the matrix
# mis-interpretation regressions that produce the green/magenta
# tint symptom class.
#
# Vivid produces dynamic content without a checked-in ground truth,
# so per-pixel PSNR isn't applicable. Instead the rig forces the
# vivid driver into a saturated single-color test pattern (default
# "100% Red"), captures the rig-wide mean of each RGB channel
# across all sampled decoded frames, and compares to a baseline TSV
# with a fixed absolute tolerance. A saturated chromatic pattern
# magnifies matrix mis-interpretations — bt.601 vs bt.709 on a 100%
# red frame produces a measurable green channel rise (~0.09)
# instead of the ~0.005 shift the same bug produces on the
# color-balanced default colorbar.
#
# Range mis-interpretation is intentionally NOT covered here — the
# range-swap class is caught by the main fixture rig's gradient
# references, where it deterministically drops Y PSNR below FAIL.
#
# Usage:
#   runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh \
#     [output_dir] [codec]
#
# Arguments:
#   output_dir — defaults to /tmp/streamlib-vivid-color-<timestamp>
#   codec      — h264 (default) or h265
#
# Environment overrides:
#   VIVID_TEST_PATTERN — vivid test_pattern index (default 7 =
#                         "100% Red"; 8=Green, 9=Blue work the same
#                         shape if a future regression-classifier
#                         wants per-primary sensitivity)
#   FRAME_LIMIT        — display frames before exit (default 180)
#   PNG_SAMPLE_EVERY   — display sample interval in frames (default 15)
#   DURATION_SECS      — roundtrip run duration (default 12)
#   TOLERANCE          — abs channel-mean drift bound on [0,1] scale
#                         (default 0.05; bug-injection negative test
#                         must drift further than this on at least
#                         one channel for the gate to be non-vacuous)
#   BASELINE_CAPTURE   — set to 1 to overwrite the checked-in
#                         baseline TSV instead of comparing
#   INJECT_BUG         — bt601-bt709 | swap-channels (the matrix /
#                         channel-swap modes from the main rig;
#                         range-swap is intentionally not supported
#                         since the saturated-color pattern is
#                         insensitive to range mis-interpretation —
#                         use the main rig's gradient fixtures for
#                         that)
#
# Exit codes: 0 = pass, 1 = fail, 77 = skip.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
BASELINE_TSV="$SCRIPT_DIR/psnr_vivid_baseline.tsv"

OUTPUT_DIR="${1:-/tmp/streamlib-vivid-color-$(date +%s)}"
CODEC="${2:-h264}"

FRAME_LIMIT="${FRAME_LIMIT:-180}"
PNG_SAMPLE_EVERY="${PNG_SAMPLE_EVERY:-15}"
DURATION_SECS="${DURATION_SECS:-12}"
TOLERANCE="${TOLERANCE:-0.05}"
BASELINE_CAPTURE="${BASELINE_CAPTURE:-}"
INJECT_BUG="${INJECT_BUG:-}"
VIVID_TEST_PATTERN="${VIVID_TEST_PATTERN:-7}"  # 7 = "100% Red"

# ── Prerequisites ────────────────────────────────────────────────────
need() { command -v "$1" >/dev/null || { echo "[vivid-color] missing: $1" >&2; exit 77; }; }
need cargo
need identify
need awk
[ -n "$INJECT_BUG" ] && need ffmpeg

# vivid is an in-kernel V4L2 test driver — no DKMS or out-of-tree modules.
if ! lsmod | grep -q vivid; then
    echo "[vivid-color] Loading vivid kernel module..."
    if ! sudo modprobe vivid 2>/dev/null; then
        echo "[vivid-color] SKIP: vivid module not available (check kernel config)" >&2
        exit 77
    fi
fi

VIVID_DEVICE=""
while read -r dev; do
    if v4l2-ctl -d "$dev" --info 2>/dev/null | grep -q "Video Capture"; then
        VIVID_DEVICE="$dev"
        break
    fi
done < <(v4l2-ctl --list-devices 2>/dev/null | awk '/vivid/{getline; print $1}')

if [ -z "$VIVID_DEVICE" ]; then
    echo "[vivid-color] SKIP: no vivid capture device found" >&2
    exit 77
fi

mkdir -p "$OUTPUT_DIR"
PNG_DIR="$OUTPUT_DIR/png_samples"
LOG_FILE="$OUTPUT_DIR/pipeline.log"
mkdir -p "$PNG_DIR"

# Force vivid into the requested pattern; restore on exit. Captured
# value covers the case where another rig left vivid in a non-default
# state — we still put it back to what it was, not blindly to 0.
# `v4l2-ctl -C test_pattern` formats as "test_pattern: 7 (100% Red)";
# field $2 gives the numeric id only (needed for `-c`).
ORIGINAL_PATTERN="$(v4l2-ctl -d "$VIVID_DEVICE" -C test_pattern 2>/dev/null | awk '{print $2}')"
if ! [[ "$ORIGINAL_PATTERN" =~ ^[0-9]+$ ]]; then
    echo "[vivid-color] WARN: failed to read original vivid test_pattern; will restore to 0" >&2
    ORIGINAL_PATTERN=0
fi
restore_pattern() {
    v4l2-ctl -d "$VIVID_DEVICE" -c "test_pattern=$ORIGINAL_PATTERN" >/dev/null 2>&1 || true
    pkill -9 -f vulkan-video-roundtrip 2>/dev/null || true
}
trap restore_pattern EXIT

if ! v4l2-ctl -d "$VIVID_DEVICE" -c "test_pattern=$VIVID_TEST_PATTERN" 2>"$OUTPUT_DIR/vivid-ctl.log"; then
    echo "[vivid-color] FAIL: could not set vivid test_pattern=$VIVID_TEST_PATTERN" >&2
    cat "$OUTPUT_DIR/vivid-ctl.log" >&2
    exit 1
fi

echo "[vivid-color] Output dir:        $OUTPUT_DIR"
echo "[vivid-color] Vivid device:      $VIVID_DEVICE"
echo "[vivid-color] Test pattern:      $VIVID_TEST_PATTERN (was $ORIGINAL_PATTERN, restored on exit)"
echo "[vivid-color] Codec:             $CODEC"
echo "[vivid-color] Duration:          ${DURATION_SECS}s (frame limit $FRAME_LIMIT)"

# ── Build ────────────────────────────────────────────────────────────
cd "$REPO_ROOT"
echo "[vivid-color] Building vulkan-video-roundtrip..."
if ! cargo build -p vulkan-video-roundtrip 2>"$OUTPUT_DIR/build.log"; then
    echo "[vivid-color] FAIL: build failed" >&2
    tail -30 "$OUTPUT_DIR/build.log" >&2
    exit 1
fi
BINARY="$REPO_ROOT/target/debug/vulkan-video-roundtrip"

# ── Run ──────────────────────────────────────────────────────────────
echo "[vivid-color] Running roundtrip..."
DISPLAY="${DISPLAY:-:0}" \
STREAMLIB_DISPLAY_PNG_SAMPLE_DIR="$PNG_DIR" \
STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY="$PNG_SAMPLE_EVERY" \
STREAMLIB_DISPLAY_FRAME_LIMIT="$FRAME_LIMIT" \
RUST_LOG=warn,streamlib=info \
timeout --kill-after=3 $((DURATION_SECS + 8)) \
    "$BINARY" "$CODEC" "$VIVID_DEVICE" "$DURATION_SECS" \
    > "$LOG_FILE" 2>&1 || true

shopt -s nullglob
SAMPLES=( "$PNG_DIR"/display_*.png )
shopt -u nullglob

if [ "${#SAMPLES[@]}" -eq 0 ]; then
    echo "[vivid-color] FAIL: no PNG samples produced" >&2
    echo "[vivid-color] Last 30 lines of pipeline log:" >&2
    tail -30 "$LOG_FILE" >&2
    exit 1
fi
echo "[vivid-color] Captured ${#SAMPLES[@]} PNG samples"

# ── Optional bug injection (negative-test mode) ──────────────────────
if [ -n "$INJECT_BUG" ]; then
    case "$INJECT_BUG" in
        swap-channels)
            inject_filter="colorchannelmixer=rr=0:rb=1:bb=0:br=1:gg=1"
            ;;
        bt601-bt709)
            inject_filter="scale=out_color_matrix=bt601:flags=accurate_rnd,format=yuv420p,scale=in_color_matrix=bt709:flags=accurate_rnd,format=rgba"
            ;;
        range-swap)
            echo "[vivid-color] ERROR: INJECT_BUG=range-swap is not supported by this rig" >&2
            echo "[vivid-color] saturated single-color patterns are insensitive to range" >&2
            echo "[vivid-color] mis-interpretation; use e2e_fixture_psnr.sh on the" >&2
            echo "[vivid-color] gradient fixtures instead" >&2
            exit 1
            ;;
        *)
            echo "[vivid-color] ERROR: unknown INJECT_BUG=$INJECT_BUG" >&2
            echo "[vivid-color] valid: swap-channels | bt601-bt709" >&2
            exit 1
            ;;
    esac
    echo "[vivid-color] INJECTING BUG: $INJECT_BUG"
    for f in "${SAMPLES[@]}"; do
        ffmpeg -y -hide_banner -loglevel error -i "$f" -vf "$inject_filter" "${f}.injected.png"
        mv "${f}.injected.png" "$f"
    done
fi

# ── Compute rig-wide channel means ───────────────────────────────────
STATS_TSV="$OUTPUT_DIR/channel_means.tsv"
echo -e "sample\tr_mean\tg_mean\tb_mean" > "$STATS_TSV"

sum_r=0; sum_g=0; sum_b=0; n=0
for f in "${SAMPLES[@]}"; do
    rgb=$(identify -format "%[fx:mean.r] %[fx:mean.g] %[fx:mean.b]" "$f")
    r=$(echo "$rgb" | awk '{print $1}')
    g=$(echo "$rgb" | awk '{print $2}')
    b=$(echo "$rgb" | awk '{print $3}')
    sum_r=$(awk -v s="$sum_r" -v v="$r" 'BEGIN{printf "%.6f", s + v}')
    sum_g=$(awk -v s="$sum_g" -v v="$g" 'BEGIN{printf "%.6f", s + v}')
    sum_b=$(awk -v s="$sum_b" -v v="$b" 'BEGIN{printf "%.6f", s + v}')
    n=$((n + 1))
    echo -e "$(basename "$f")\t$r\t$g\t$b" >> "$STATS_TSV"
done

avg_r=$(awk -v s="$sum_r" -v n="$n" 'BEGIN{printf "%.4f", s / n}')
avg_g=$(awk -v s="$sum_g" -v n="$n" 'BEGIN{printf "%.4f", s / n}')
avg_b=$(awk -v s="$sum_b" -v n="$n" 'BEGIN{printf "%.4f", s / n}')

echo ""
echo "══════════════════════════════════════════════════════════════"
echo "  Vivid Color Roundtrip Channel Means ($CODEC)"
echo "══════════════════════════════════════════════════════════════"
echo "  Samples:  $n"
echo "  Mean R:   $avg_r"
echo "  Mean G:   $avg_g"
echo "  Mean B:   $avg_b"
echo "  Per-sample stats: $STATS_TSV"

# ── Capture baseline OR compare ──────────────────────────────────────
if [ "$BASELINE_CAPTURE" = "1" ]; then
    {
        echo "# Vivid color-roundtrip channel-mean baseline."
        echo "# Generated by runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh"
        echo "# Captured with: BASELINE_CAPTURE=1 e2e_fixture_psnr_vivid.sh <out> $CODEC"
        echo "# Vivid test_pattern: $VIVID_TEST_PATTERN"
        echo "# Default verification tolerance is ±0.05 absolute on the [0,1] scale."
        printf "channel\tmean\n"
        printf "r\t%s\n" "$avg_r"
        printf "g\t%s\n" "$avg_g"
        printf "b\t%s\n" "$avg_b"
    } > "$BASELINE_TSV"
    echo "  Baseline written: $BASELINE_TSV"
    echo "══════════════════════════════════════════════════════════════"
    echo "[vivid-color] BASELINE CAPTURED"
    exit 0
fi

if [ ! -s "$BASELINE_TSV" ]; then
    echo "[vivid-color] FAIL: baseline TSV missing at $BASELINE_TSV" >&2
    echo "[vivid-color] capture with: BASELINE_CAPTURE=1 $0 $*" >&2
    exit 1
fi

baseline_r=$(awk -F'\t' '$1 == "r" {print $2}' "$BASELINE_TSV")
baseline_g=$(awk -F'\t' '$1 == "g" {print $2}' "$BASELINE_TSV")
baseline_b=$(awk -F'\t' '$1 == "b" {print $2}' "$BASELINE_TSV")

echo "  Baseline: R=$baseline_r  G=$baseline_g  B=$baseline_b  (±$TOLERANCE)"

OVERALL_PASS=true
check_channel() {
    local chan="$1" actual="$2" base="$3"
    local diff within
    diff=$(awk -v a="$actual" -v b="$base" 'BEGIN{d = a - b; if (d < 0) d = -d; printf "%.4f", d}')
    within=$(awk -v d="$diff" -v t="$TOLERANCE" 'BEGIN{print (d + 0 <= t + 0) ? 1 : 0}')
    if [ "$within" = "1" ]; then
        printf "    %s drift: %s  (limit %s)  PASS\n" "$chan" "$diff" "$TOLERANCE"
    else
        printf "    %s drift: %s  (limit %s)  FAIL\n" "$chan" "$diff" "$TOLERANCE"
        OVERALL_PASS=false
    fi
}
check_channel R "$avg_r" "$baseline_r"
check_channel G "$avg_g" "$baseline_g"
check_channel B "$avg_b" "$baseline_b"

echo "══════════════════════════════════════════════════════════════"
if [ "$OVERALL_PASS" = true ]; then
    echo "[vivid-color] RESULT: PASS"
    exit 0
else
    echo "[vivid-color] RESULT: FAIL — channel drift outside tolerance"
    echo "[vivid-color] (color-management regression suspected — investigate)"
    exit 1
fi

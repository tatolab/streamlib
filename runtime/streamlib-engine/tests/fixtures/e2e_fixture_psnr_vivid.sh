#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Vivid color-roundtrip regression gate (issues #305, #2085).
#
# Sister fixture to e2e_fixture_psnr.sh. Where that rig measures encode/decode
# quality against checked-in reference PNGs via PSNR, this one guards the V4L2
# color path against the matrix mis-interpretation regressions that produce the
# green/magenta tint symptom class.
#
# Vivid produces dynamic content without a checked-in ground truth, so
# per-pixel PSNR isn't applicable. Instead the rig forces the vivid driver into
# a saturated single-color test pattern (default "100% Red"), captures the
# rig-wide mean of each RGB channel across the decoded frames, and compares to
# a baseline TSV with a fixed absolute tolerance. A saturated chromatic pattern
# magnifies matrix mis-interpretations — bt.601 vs bt.709 on a 100% red frame
# produces a measurable green channel rise (~0.09) instead of the ~0.005 shift
# the same bug produces on the color-balanced default colorbar.
#
# Range mis-interpretation is intentionally NOT covered here — a saturated
# primary already sits at the end of the coded range and clips straight back,
# so the range-swap class is caught by the main fixture rig's gradient
# references, where it deterministically drops Y PSNR below FAIL.
#
# Frames are read the way the main rig reads them: the decoded channel is
# tapped and each sampled surface id is exchanged for that frame's exact pixels
# over the control plane. Measurement and injection are both
# `cargo xtask psnr channel-means`, which is what took ffmpeg and ImageMagick
# out of this path — the injection modes are the main rig's, defined once.
#
# Usage:
#   runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh \
#     [output_dir] [codec]
#
# Arguments:
#   output_dir — defaults to /tmp/streamlib-vivid-color-<timestamp>
#   codec      — h264 (default). h265 lands with #2086.
#
# Environment overrides:
#   VIVID_TEST_PATTERN — vivid test_pattern index (default 7 = "100% Red";
#                         8=Green, 9=Blue work the same shape if a future
#                         regression-classifier wants per-primary sensitivity)
#   SAMPLE_COUNT       — decoded frames exchanged (default 6)
#   SAMPLE_EVERY       — exchange every Nth sampled bag, for temporal spread
#                         (default 2). SAMPLE_COUNT x SAMPLE_EVERY is a bag
#                         budget, not a wish: `exchange` gives up after 8 tap
#                         rounds and each round is a ~500 ms window, so at the
#                         5 fps vivid negotiates about 19 bags reach the run.
#                         Asking for more returns short, which is a failure.
#   CONTROL_PLANE_PORT — port the rig's control plane binds (default 9402)
#   RUN_SECONDS        — rig budget (default 60)
#   TOLERANCE          — abs channel-mean drift bound on [0,1] scale
#                         (default 0.05; the bug-injection negative test must
#                         drift further than this on at least one channel for
#                         the gate to be non-vacuous)
#   BASELINE_CAPTURE   — set to 1 to overwrite the checked-in baseline TSV
#                         instead of comparing
#   INJECT_BUG         — bt601-bt709 | swap-channels (the matrix / channel-swap
#                         modes from the main rig; range-swap is rejected here
#                         for the reason above)
#
# Exit codes: 0 = pass, 1 = fail, 77 = skip.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
BASELINE_TSV="$SCRIPT_DIR/psnr_vivid_baseline.tsv"

OUTPUT_DIR="${1:-/tmp/streamlib-vivid-color-$(date +%s)}"
CODEC="${2:-h264}"

SAMPLE_COUNT="${SAMPLE_COUNT:-6}"
SAMPLE_EVERY="${SAMPLE_EVERY:-2}"
CONTROL_PLANE_PORT="${CONTROL_PLANE_PORT:-9402}"
RUN_SECONDS="${RUN_SECONDS:-60}"
TOLERANCE="${TOLERANCE:-0.05}"
BASELINE_CAPTURE="${BASELINE_CAPTURE:-}"
INJECT_BUG="${INJECT_BUG:-}"
VIVID_TEST_PATTERN="${VIVID_TEST_PATTERN:-7}"  # 7 = "100% Red"

# ── Prerequisites ────────────────────────────────────────────────────
need() { command -v "$1" >/dev/null || { echo "[vivid-color] missing: $1" >&2; exit 77; }; }
need cargo
need python3
need v4l2-ctl

if [ "$CODEC" != "h264" ]; then
    echo "[vivid-color] SKIP: the rig encodes h264 only today; the H.265 arm lands with #2086" >&2
    exit 77
fi

if [ "$INJECT_BUG" = "range-swap" ]; then
    echo "[vivid-color] ERROR: INJECT_BUG=range-swap is not supported by this rig" >&2
    echo "[vivid-color] saturated single-color patterns sit at the end of the coded" >&2
    echo "[vivid-color] range and clip straight back; use e2e_fixture_psnr.sh on the" >&2
    echo "[vivid-color] gradient references instead" >&2
    exit 1
fi

STREAMLIB_CLI="$(command -v streamlib || true)"
if [ -z "$STREAMLIB_CLI" ]; then
    STREAMLIB_CLI="$REPO_ROOT/sdk/streamlib-python-wheel/.venv/bin/streamlib"
fi
if [ ! -x "$STREAMLIB_CLI" ]; then
    echo "[vivid-color] SKIP: no streamlib CLI on PATH or at $STREAMLIB_CLI" >&2
    exit 77
fi

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
EXCHANGED_DIR="$OUTPUT_DIR/exchanged"
LOG_FILE="$OUTPUT_DIR/pipeline.log"
CONTROL_PLANE_URL="http://127.0.0.1:$CONTROL_PLANE_PORT"

# Force vivid into the requested pattern; restore on exit. Captured value
# covers the case where another rig left vivid in a non-default state — we
# still put it back to what it was, not blindly to 0.
# `v4l2-ctl -C test_pattern` formats as "test_pattern: 7 (100% Red)"; field $2
# gives the numeric id only (needed for `-c`).
ORIGINAL_PATTERN="$(v4l2-ctl -d "$VIVID_DEVICE" -C test_pattern 2>/dev/null | awk '{print $2}')"
if ! [[ "$ORIGINAL_PATTERN" =~ ^[0-9]+$ ]]; then
    echo "[vivid-color] WARN: failed to read original vivid test_pattern; will restore to 0" >&2
    ORIGINAL_PATTERN=0
fi

RIG_PID=""
RIG_NEEDED_SIGKILL=0
stop_rig() {
    RIG_NEEDED_SIGKILL=0
    if [ -n "$RIG_PID" ] && kill -0 "$RIG_PID" 2>/dev/null; then
        # SIGTERM so the graph tears down the way a real stop does — a killed
        # rig would hide exactly the shutdown race #335 is about.
        kill -TERM "$RIG_PID" 2>/dev/null || true
        for _ in $(seq 1 50); do
            kill -0 "$RIG_PID" 2>/dev/null || break
            sleep 0.2
        done
        # A rig still alive after 10 s of SIGTERM is the #335 shutdown race, not
        # a slow exit. SIGKILL releases the camera; the caller is told, so the
        # kill can never launder a hung teardown into a PASS.
        if kill -0 "$RIG_PID" 2>/dev/null; then
            RIG_NEEDED_SIGKILL=1
            kill -9 "$RIG_PID" 2>/dev/null || true
        fi
        wait "$RIG_PID" 2>/dev/null || true
    fi
    RIG_PID=""
}
restore_pattern_and_stop_rig() {
    stop_rig
    v4l2-ctl -d "$VIVID_DEVICE" -c "test_pattern=$ORIGINAL_PATTERN" >/dev/null 2>&1 || true
}
trap restore_pattern_and_stop_rig EXIT

if ! v4l2-ctl -d "$VIVID_DEVICE" -c "test_pattern=$VIVID_TEST_PATTERN" 2>"$OUTPUT_DIR/vivid-ctl.log"; then
    echo "[vivid-color] FAIL: could not set vivid test_pattern=$VIVID_TEST_PATTERN" >&2
    cat "$OUTPUT_DIR/vivid-ctl.log" >&2
    exit 1
fi

echo "[vivid-color] Output dir:        $OUTPUT_DIR"
echo "[vivid-color] Vivid device:      $VIVID_DEVICE"
echo "[vivid-color] Test pattern:      $VIVID_TEST_PATTERN (was $ORIGINAL_PATTERN, restored on exit)"
echo "[vivid-color] Codec:             $CODEC"
echo "[vivid-color] Control plane:     $CONTROL_PLANE_URL"

# ── Build ────────────────────────────────────────────────────────────
cd "$REPO_ROOT"
echo "[vivid-color] Building codec_roundtrip_rig + xtask (release)..."
if ! cargo build --release --locked -p streamlib-engine --example codec_roundtrip_rig \
        > "$OUTPUT_DIR/build.log" 2>&1; then
    echo "[vivid-color] FAIL: rig build failed" >&2
    tail -40 "$OUTPUT_DIR/build.log" >&2
    exit 1
fi
if ! cargo build --release --locked -p xtask >> "$OUTPUT_DIR/build.log" 2>&1; then
    echo "[vivid-color] FAIL: xtask build failed" >&2
    tail -40 "$OUTPUT_DIR/build.log" >&2
    exit 1
fi

# ── Run ──────────────────────────────────────────────────────────────
echo "[vivid-color] Running the round trip against $VIVID_DEVICE..."
DISPLAY="${DISPLAY:-:0}" \
RUST_LOG="${RUST_LOG:-warn,streamlib=info,streamlib_media_builtins=info}" \
    timeout --kill-after=5 "$RUN_SECONDS" \
        "$REPO_ROOT/target/release/examples/codec_roundtrip_rig" \
        --source camera \
        --camera "$VIVID_DEVICE" \
        --control-plane-port "$CONTROL_PLANE_PORT" \
        > "$LOG_FILE" 2>&1 &
RIG_PID=$!

for _ in $(seq 1 60); do
    if "$STREAMLIB_CLI" graph --url "$CONTROL_PLANE_URL" >/dev/null 2>&1; then
        break
    fi
    sleep 0.5
done

# A channel is `{processor_id}/{output_port}` with the id chunk lowercased, and
# a processor id is a cuid2 minted at add time — `decoder` is the rig's display
# name, not its id. Derived from the live graph rather than guessed.
DECODED_CHANNEL="$("$STREAMLIB_CLI" graph --url "$CONTROL_PLANE_URL" 2>/dev/null | python3 -c '
import json, sys
graph = json.load(sys.stdin)
decoder = next(
    (node for node in graph.get("nodes", []) if node.get("display_name") == "decoder"), None
)
if decoder is None:
    sys.exit("the running graph has no processor named `decoder`")
print(decoder["id"].lower() + "/video")
')" || {
    echo "[vivid-color] FAIL: could not read the decoder channel off the live graph" >&2
    tail -30 "$LOG_FILE" >&2
    exit 1
}
echo "[vivid-color] Decoded channel:   $DECODED_CHANNEL"

if ! "$STREAMLIB_CLI" exchange \
        --channel "$DECODED_CHANNEL" \
        --out "$EXCHANGED_DIR" \
        --count "$SAMPLE_COUNT" \
        --every "$SAMPLE_EVERY" \
        --url "$CONTROL_PLANE_URL" \
        > "$OUTPUT_DIR/exchanged_paths.txt" 2> "$OUTPUT_DIR/exchange.log"; then
    echo "[vivid-color] FAIL: exchanged fewer frames than asked for" >&2
    cat "$OUTPUT_DIR/exchange.log" >&2
    tail -30 "$LOG_FILE" >&2
    exit 1
fi
cat "$OUTPUT_DIR/exchange.log"

# Read the printed paths, not the directory: --out is not cleared, so a listing
# can hand back an earlier run's frames.
MEASURED_DIR="$OUTPUT_DIR/measured"
mkdir -p "$MEASURED_DIR"
sample_index=0
while read -r exchanged_png; do
    [ -f "$exchanged_png" ] || continue
    cp "$exchanged_png" "$MEASURED_DIR/$(printf "%04d" "$sample_index").png"
    sample_index=$(( sample_index + 1 ))
done < "$OUTPUT_DIR/exchanged_paths.txt"

if [ "$sample_index" -ne "$SAMPLE_COUNT" ]; then
    echo "[vivid-color] FAIL: copied $sample_index of $SAMPLE_COUNT exchanged frames;" \
         "a drift lock measured on fewer samples than the run asked for reports a" \
         "thinner gate as a full one" >&2
    cat "$OUTPUT_DIR/exchange.log" >&2
    exit 1
fi
echo "[vivid-color] Captured $sample_index frames"
stop_rig
if [ "$RIG_NEEDED_SIGKILL" -eq 1 ]; then
    echo "[vivid-color] FAIL: the rig did not exit on SIGTERM and needed SIGKILL." \
         "A teardown that hangs is the #335 race class, not a slow exit." >&2
    tail -30 "$LOG_FILE" >&2
    exit 1
fi

# ── Measure ──────────────────────────────────────────────────────────
MEASURE_ARGUMENTS=(
    psnr channel-means
    --images "$MEASURED_DIR"
    --baseline "$BASELINE_TSV"
    --tolerance "$TOLERANCE"
    --report "$OUTPUT_DIR/channel_means.tsv"
)
if [ -n "$INJECT_BUG" ]; then
    MEASURE_ARGUMENTS+=(--inject "$INJECT_BUG")
fi
if [ "$BASELINE_CAPTURE" = "1" ]; then
    MEASURE_ARGUMENTS+=(
        --capture-baseline
        --baseline-note "Vivid test_pattern: $VIVID_TEST_PATTERN"
        --baseline-note "Measured from exact decoded pixels over the control plane's exchange route. The pre-#2085 baseline sampled the display's composited output, whose swapchain colour handling pulled green and blue up; do not compare the two."
    )
fi

echo ""
if "$REPO_ROOT/target/release/xtask" "${MEASURE_ARGUMENTS[@]}"; then
    echo "[vivid-color] Output dir:        $OUTPUT_DIR"
    echo "[vivid-color] Per-sample stats:  $OUTPUT_DIR/channel_means.tsv"
    exit 0
fi
echo "[vivid-color] Output dir:        $OUTPUT_DIR"
echo "[vivid-color] RESULT: FAIL — channel drift outside tolerance"
echo "[vivid-color] (color-management regression suspected — investigate)"
exit 1

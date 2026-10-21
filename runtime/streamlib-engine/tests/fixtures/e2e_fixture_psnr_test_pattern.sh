#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Test-pattern colour round-trip regression gate.
#
# Guards the encode colour path against the matrix mis-interpretation
# regressions that produce the green/magenta tint symptom class, the codec
# round trip built as a deployment builds it: two streams on one runtime. The
# sender (TestPatternSource → encoder) exposes the encoder's port; the receiver
# reads that track at the local relay and decodes it.
#
# The source needs no camera: TestPatternSource renders each frame from its
# index alone, so every run encodes the same pictures. The lock is each SMPTE
# bar's median colour, averaged across the decoded frames and compared to a
# baseline TSV with a fixed absolute tolerance. Each bar is a saturated
# primary, secondary or neutral, which magnifies a matrix mis-interpretation —
# bt.601 vs bt.709 lifts the red bar's green by about 0.09 — where a whole-frame
# average over the colour-balanced bars would move by about 0.005. The median
# is each bar's own colour wherever the potato crosses it, so which frames the
# read caught does not move the measurement.
#
# Range mis-interpretation is not part of this lock: the encoder writes TV-range
# samples under a VUI that signals TV range, so saturated bars sit at the ends of
# the coded range and a range swap clips straight back to them.
#
# Frames are read the way any user reads a port: `tatolab-read-track` reads the
# sender's encoded track at the local relay's loopback listener, the rig's
# ffmpeg decodes the dump to PNGs, and `cargo xtask psnr bar-colors` measures
# and injects. The receiver's own decode is proven by the count of frames its
# decoder wrote, read off `graph`; the engine decoder's pixels are its unit
# tests' to prove.
#
# Usage:
#   runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_test_pattern.sh \
#     [output_dir] [codec]
#
# Arguments:
#   output_dir — defaults to /tmp/streamlib-test-pattern-color-<timestamp>
#   codec      — h264 (default) or h265. Each codec locks against its own
#                baseline TSV.
#
# Environment overrides:
#   OBJECT_COUNT       — encoded frames read off the sender's track and
#                         measured (default 20)
#   DECODED_FRAMES_FLOOR — frames the receiver's decoder must have written
#                         (default 10)
#   RUN_SECONDS        — rig budget (default 90)
#   TOLERANCE          — abs per-bar channel drift bound on [0,1] scale
#                         (default 0.05; the bug-injection negative test must
#                         drift further than this on at least one channel for
#                         the gate to be non-vacuous)
#   BASELINE_CAPTURE   — set to 1 to overwrite the checked-in baseline TSV
#                         instead of comparing
#   INJECT_BUG         — bt601-bt709 | swap-channels | swap-chroma.
#                         swap-chroma suits this rig well: transposing the
#                         difference planes turns a saturated bar into
#                         another colour, which is the largest drift a bar
#                         can carry.
#
# It scores whatever engine the runtime unit's `tatolabd` carries — rebuild it
# before running it: `cargo xtask build-runtime --release` (see
# fixture_runtime_unit.sh). It refuses to run while another runtime holds the
# machine or answers at this user's local API socket.
#
# Exit codes: 0 = pass, 1 = fail, 77 = skip.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
# shellcheck source=fixture_runtime_unit.sh
. "$SCRIPT_DIR/fixture_runtime_unit.sh"
# One baseline per codec, so a codec that reconstructs a bar differently is
# never read as a colour regression.
baseline_tsv_for_codec() {
    echo "$SCRIPT_DIR/psnr_test_pattern_bar_colors_baseline_$1.tsv"
}

OUTPUT_DIR="${1:-/tmp/streamlib-test-pattern-color-$(date +%s)}"
CODEC="${2:-h264}"

OBJECT_COUNT="${OBJECT_COUNT:-20}"
DECODED_FRAMES_FLOOR="${DECODED_FRAMES_FLOOR:-10}"
RUN_SECONDS="${RUN_SECONDS:-90}"
TOLERANCE="${TOLERANCE:-0.05}"
BASELINE_CAPTURE="${BASELINE_CAPTURE:-}"
INJECT_BUG="${INJECT_BUG:-}"

# The names the two streams are loaded under; the receiver builds the sender's
# broadcast from SENDER_STREAM_NAME, so the two are one name.
SENDER_STREAM_NAME="codec_roundtrip_sender"
RECEIVER_STREAM_NAME="codec_roundtrip_receiver"
SENDER_ENCODED_TRACK="encoder/encoded_video"

# ── Prerequisites ────────────────────────────────────────────────────
need() { command -v "$1" >/dev/null || { echo "[test-pattern-color] missing: $1" >&2; exit 77; }; }
need cargo
need python3
need ffmpeg

case "$CODEC" in
    h264) FFMPEG_ELEMENTARY_STREAM_FORMAT="h264" ;;
    h265) FFMPEG_ELEMENTARY_STREAM_FORMAT="hevc" ;;
    *)
        echo "[test-pattern-color] FAIL: codec '$CODEC' is neither h264 nor h265" >&2
        exit 1
        ;;
esac
BASELINE_TSV="$(baseline_tsv_for_codec "$CODEC")"
if [ ! -f "$BASELINE_TSV" ] && [ "$BASELINE_CAPTURE" != "1" ]; then
    echo "[test-pattern-color] FAIL: no baseline for $CODEC at $BASELINE_TSV." >&2
    echo "[test-pattern-color] Capture one on the rig with BASELINE_CAPTURE=1 before gating on it." >&2
    exit 1
fi

if [ "$INJECT_BUG" = "range-swap" ]; then
    echo "[test-pattern-color] ERROR: INJECT_BUG=range-swap is not part of this rig's lock" >&2
    echo "[test-pattern-color] (the header says why)" >&2
    exit 1
fi

require_the_runtime_unit
require_the_fixture_venv

mkdir -p "$OUTPUT_DIR"
ENCODED_TRACK_DIR="$OUTPUT_DIR/encoded_track"
MEASURED_DIR="$OUTPUT_DIR/measured"
# The runtime's output — the engine's log mirror, every stream's records among
# it — with each `tatolab run`'s beside it.
LOG_FILE="$OUTPUT_DIR/runtime.log"
SENDER_RUN_LOG_FILE="$OUTPUT_DIR/sender_run.log"
RECEIVER_RUN_LOG_FILE="$OUTPUT_DIR/receiver_run.log"

STREAM_RUN_PIDS=()
STREAM_RUN_NEEDED_SIGKILL=0
stop_the_stream_runs() {
    local stream_run_pid
    for stream_run_pid in "${STREAM_RUN_PIDS[@]}"; do
        kill -0 "$stream_run_pid" 2>/dev/null || continue
        # SIGTERM so the graph tears down the way a real stop does — a killed
        # run would hide exactly the shutdown race this rig guards against. `timeout`
        # passes it on; `tatolab run` stops its stream, which the runtime
        # unloads before the run exits.
        kill -TERM "$stream_run_pid" 2>/dev/null || true
        for _ in $(seq 1 50); do
            kill -0 "$stream_run_pid" 2>/dev/null || break
            sleep 0.2
        done
        # A run still alive after 10 s of SIGTERM is that shutdown race, not
        # a slow exit. SIGKILL ends it; the caller is told, so the kill can
        # never launder a hung teardown into a PASS.
        if kill -0 "$stream_run_pid" 2>/dev/null; then
            STREAM_RUN_NEEDED_SIGKILL=1
            kill -9 "$stream_run_pid" 2>/dev/null || true
        fi
        wait "$stream_run_pid" 2>/dev/null || true
    done
    STREAM_RUN_PIDS=()
}
stop_the_streams_and_the_runtime() {
    stop_the_stream_runs
    stop_the_fixture_runtime
}
trap stop_the_streams_and_the_runtime EXIT

echo "[test-pattern-color] Output dir:        $OUTPUT_DIR"
echo "[test-pattern-color] Codec:             $CODEC"

# ── Build ────────────────────────────────────────────────────────────
cd "$REPO_ROOT"
echo "[test-pattern-color] Building tatolab-read-track + xtask (release)..."
require_tatolab_read_track
if ! cargo build --release --locked -p xtask > "$OUTPUT_DIR/build.log" 2>&1; then
    echo "[test-pattern-color] FAIL: xtask build failed" >&2
    tail -40 "$OUTPUT_DIR/build.log" >&2
    exit 1
fi

# ── Run ──────────────────────────────────────────────────────────────
# A stream takes no argv, so the codec travels in the runtime's environment,
# which each stream's compile and its processor interpreters inherit.
export RUST_LOG="${RUST_LOG:-warn,streamlib=info,streamlib_media_builtins=info}"
if ! STREAMLIB_FIXTURE_VIDEO_CODEC="$CODEC" \
        start_the_fixture_runtime "$LOG_FILE" "$OUTPUT_DIR/runtime_state"; then
    echo "[test-pattern-color] FAIL: the fixture could not start its runtime" >&2
    exit 1
fi

run_the_fixture_stream() {
    local stream_name="$1" stream_function="$2" run_log_file="$3"
    timeout --kill-after=5 "$RUN_SECONDS" \
        "$TATOLAB_EXECUTABLE" run \
            --dir "$SCRIPT_DIR" \
            --name "$stream_name" \
            "codec_roundtrip_stream.py:$stream_function" \
        > "$run_log_file" 2>&1 &
    STREAM_RUN_PIDS+=("$!")
    if ! wait_until_the_stream_is_running "$stream_name" "$!" 60; then
        echo "[test-pattern-color] FAIL: the stream $stream_name never ran — it never answered over" \
            "the local API socket with every node Running" >&2
        tail -30 "$run_log_file" >&2
        tail -30 "$LOG_FILE" >&2
        exit 1
    fi
    echo "[test-pattern-color] Running:           $stream_name"
}
run_the_fixture_stream "$SENDER_STREAM_NAME" \
    test_pattern_through_the_encoder_exposing_its_encoded_port "$SENDER_RUN_LOG_FILE"
run_the_fixture_stream "$RECEIVER_STREAM_NAME" \
    the_senders_encoded_track_through_the_decoder "$RECEIVER_RUN_LOG_FILE"

MACHINE_NAME="$(the_machine_name_the_loopback_listing_names)" || {
    echo "[test-pattern-color] FAIL: the runtime's loopback listing at $LOOPBACK_HTTP_LISTING_URL" \
        "did not name the machine" >&2
    exit 1
}
SENDER_BROADCAST="$MACHINE_NAME/$SENDER_STREAM_NAME.hang"
echo "[test-pattern-color] Reading:           $SENDER_ENCODED_TRACK of $SENDER_BROADCAST"

if ! read_the_track_at_the_local_relay "$SENDER_BROADCAST" "$SENDER_ENCODED_TRACK" \
        "$OBJECT_COUNT" "$ENCODED_TRACK_DIR" 2> "$OUTPUT_DIR/read_track.log"; then
    echo "[test-pattern-color] FAIL: the sender's encoded track did not read" >&2
    cat "$OUTPUT_DIR/read_track.log" >&2
    tail -30 "$LOG_FILE" >&2
    exit 1
fi
cat "$OUTPUT_DIR/read_track.log"
if ! grep -q '"payload":"codec_payload_of_the_hang_legacy_container"' \
        "$ENCODED_TRACK_DIR/objects.jsonl"; then
    echo "[test-pattern-color] FAIL: tatolab-read-track wrote the encoder's track as sent, not as its" \
        "codec payload — the catalog never listed it as a legacy-container video track" >&2
    cat "$OUTPUT_DIR/read_track.log" >&2
    exit 1
fi

# The receiver decodes the same track; its decoder's written frames, read off
# `graph`, are its proof.
RECEIVER_DECODED_FRAMES=0
for _ in $(seq 1 60); do
    RECEIVER_DECODED_FRAMES="$("$TATOLAB_EXECUTABLE" graph --stream "$RECEIVER_STREAM_NAME" 2>/dev/null \
        | "$FIXTURE_PYTHON" -c '
import json, sys
graph = json.load(sys.stdin)
decoder = next(node for node in graph["nodes"] if node["name"] == "decoder")
print(decoder["components"]["metrics"]["written_bags_by_output_port"].get("video", 0))
')" || RECEIVER_DECODED_FRAMES=0
    [ "$RECEIVER_DECODED_FRAMES" -ge "$DECODED_FRAMES_FLOOR" ] && break
    sleep 0.5
done
echo "[test-pattern-color] Receiver decoded:  $RECEIVER_DECODED_FRAMES frames"
if [ "$RECEIVER_DECODED_FRAMES" -lt "$DECODED_FRAMES_FLOOR" ]; then
    echo "[test-pattern-color] FAIL: the receiver's decoder wrote $RECEIVER_DECODED_FRAMES frames," \
        "fewer than $DECODED_FRAMES_FLOOR" >&2
    tail -30 "$RECEIVER_RUN_LOG_FILE" >&2
    tail -30 "$LOG_FILE" >&2
    exit 1
fi

stop_the_stream_runs
if [ "$STREAM_RUN_NEEDED_SIGKILL" -eq 1 ]; then
    echo "[test-pattern-color] FAIL: a stream run did not exit on SIGTERM and needed SIGKILL." \
         "A teardown that hangs is the shutdown race class, not a slow exit." >&2
    tail -30 "$LOG_FILE" >&2
    exit 1
fi
stop_the_fixture_runtime 15
if ! the_fixture_runtime_stopped_cleanly; then
    echo "[test-pattern-color] FAIL: the runtime did not stop cleanly once its streams were" \
         "unloaded — the shutdown race class again, one level up." >&2
    tail -30 "$LOG_FILE" >&2
    exit 1
fi

# ── Decode ───────────────────────────────────────────────────────────
# The dump's files are the codec's own elementary stream once concatenated in
# index order, which ffmpeg decodes as it would any `.h264` / `.hevc` file.
ENCODED_ELEMENTARY_STREAM="$OUTPUT_DIR/encoded.$FFMPEG_ELEMENTARY_STREAM_FORMAT"
"$FIXTURE_PYTHON" -c '
import json, pathlib, sys
dump = pathlib.Path(sys.argv[1])
with open(sys.argv[2], "wb") as elementary_stream:
    for line in (dump / "objects.jsonl").read_text().splitlines():
        elementary_stream.write((dump / json.loads(line)["file"]).read_bytes())
' "$ENCODED_TRACK_DIR" "$ENCODED_ELEMENTARY_STREAM"
mkdir -p "$MEASURED_DIR"
if ! ffmpeg -hide_banner -loglevel error -nostdin \
        -f "$FFMPEG_ELEMENTARY_STREAM_FORMAT" -i "$ENCODED_ELEMENTARY_STREAM" \
        -fps_mode passthrough -pix_fmt rgb24 \
        "$MEASURED_DIR/%04d.png" 2> "$OUTPUT_DIR/ffmpeg.log"; then
    echo "[test-pattern-color] FAIL: ffmpeg could not decode the sender's track" >&2
    cat "$OUTPUT_DIR/ffmpeg.log" >&2
    exit 1
fi
DECODED_PNG_COUNT="$(find "$MEASURED_DIR" -name '*.png' | wc -l)"
echo "[test-pattern-color] ffmpeg decoded:    $DECODED_PNG_COUNT of $OBJECT_COUNT frames"
if [ "$DECODED_PNG_COUNT" -ne "$OBJECT_COUNT" ]; then
    echo "[test-pattern-color] FAIL: ffmpeg decoded $DECODED_PNG_COUNT of the $OBJECT_COUNT frames" \
         "read; a drift lock measured on fewer frames than the run read reports a thinner" \
         "gate as a full one" >&2
    cat "$OUTPUT_DIR/ffmpeg.log" >&2
    exit 1
fi

# ── Measure ──────────────────────────────────────────────────────────
MEASURE_ARGUMENTS=(
    psnr bar-colors
    --images "$MEASURED_DIR"
    --baseline "$BASELINE_TSV"
    --tolerance "$TOLERANCE"
    --report "$OUTPUT_DIR/bar_colors.tsv"
)
if [ -n "$INJECT_BUG" ]; then
    MEASURE_ARGUMENTS+=(--inject "$INJECT_BUG")
fi
if [ "$BASELINE_CAPTURE" = "1" ]; then
    MEASURE_ARGUMENTS+=(
        --capture-baseline
        --baseline-note "Generated by runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_test_pattern.sh"
        --baseline-note "Captured with: BASELINE_CAPTURE=1 e2e_fixture_psnr_test_pattern.sh <out> <codec>"
        --baseline-note "Codec: $CODEC"
        --baseline-note "Source: TestPatternSource at its default 1280x720."
        --baseline-note "Measured from the sender's encoded track read at the local relay with tatolab-read-track and decoded by ffmpeg to RGB PNGs."
    )
fi

echo ""
if "$REPO_ROOT/target/release/xtask" "${MEASURE_ARGUMENTS[@]}"; then
    echo "[test-pattern-color] Output dir:        $OUTPUT_DIR"
    echo "[test-pattern-color] Per-sample stats:  $OUTPUT_DIR/bar_colors.tsv"
    exit 0
fi
echo "[test-pattern-color] Output dir:        $OUTPUT_DIR"
echo "[test-pattern-color] RESULT: FAIL — bar colour drift outside tolerance"
echo "[test-pattern-color] (color-management regression suspected — investigate)"
exit 1

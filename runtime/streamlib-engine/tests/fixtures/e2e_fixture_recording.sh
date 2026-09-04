#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Recording gate: the vivid camera and the known audio signal into one MP4,
# then that file's video track back through our own decoder (issue #2128).
#
# Third sister to e2e_fixture_psnr.sh and e2e_fixture_psnr_vivid.sh. Where the
# vivid rig scores camera -> encoder -> decoder, this one puts the container in
# the middle: camera -> encoder -> Mp4Sink -> demux -> decoder, and locks the
# result to the same per-codec baseline at the same tolerance. That is the
# whole argument — the decode-back is the proof the container carried the
# encoder's bytes untouched, because it is the path the codec rig already
# scored with one file in between. A mismatch beyond tolerance is a regression
# in the writer, never a reason for a third baseline.
#
# Three phases, each of which can fail on its own terms:
#
#   record   recording_node.py runs until the file holds enough video, then
#            takes SIGTERM. A run that needs SIGKILL is a hard FAIL, not a
#            slow exit — teardown is what closes the last fragment.
#   inspect  `cargo xtask mp4-inspect` on the written file: two tracks named
#            after their producers, the video one an avc1/hvc1 entry matching
#            the codec, the audio one Opus, and fragments actually closed.
#   replay   `codec_roundtrip_rig --source mp4:<file>` demuxes the video track
#            back into access units and publishes them into the decoder, which
#            is then tapped and exchanged exactly as the vivid rig does.
#
# Usage:
#   runtime/streamlib-engine/tests/fixtures/e2e_fixture_recording.sh \
#     [output_dir] [codec]
#
# Arguments:
#   output_dir — defaults to /tmp/streamlib-recording-<timestamp>
#   codec      — h264 (default) or h265. Each locks against the vivid rig's
#                own baseline for that codec; there is no recording baseline.
#
# Environment overrides:
#   VIVID_TEST_PATTERN     — vivid test_pattern index (default 7 = "100% Red"),
#                             which has to match what the baseline was captured
#                             under or the lock is meaningless
#   MIN_RECORDED_FRAMES    — video samples the file must hold before the node is
#                             stopped (default 120). This is what sizes the
#                             replay: the arm publishes at 10 fps, and the
#                             exchange below needs the replay still running.
#   RECORD_SECONDS         — ceiling on the record phase (default 90)
#   SAMPLE_COUNT           — decoded frames exchanged (default 6)
#   SAMPLE_EVERY           — exchange every Nth sampled bag (default 2).
#                             SAMPLE_COUNT x SAMPLE_EVERY is a bag budget, not a
#                             wish: `exchange` gives up after 8 tap rounds of a
#                             ~500 ms window each.
#   CONTROL_PLANE_PORT     — port recording_node.py binds (default 9403)
#   REPLAY_CONTROL_PLANE_PORT — port the replay rig binds (default 9404)
#   REPLAY_SECONDS         — ceiling on the replay phase (default 90)
#   TOLERANCE              — abs channel-mean drift bound on [0,1] (default 0.05)
#   INJECT_BUG             — bt601-bt709 | swap-channels | swap-chroma, the
#                             vivid rig's injection modes, for proving the gate
#                             is non-vacuous
#
# Exit codes: 0 = pass, 1 = fail, 77 = skip.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
# The vivid rig's own baselines, unsuffixed for h264 the way it names them.
baseline_tsv_for_codec() {
    case "$1" in
        h264) echo "$SCRIPT_DIR/psnr_vivid_baseline.tsv" ;;
        *) echo "$SCRIPT_DIR/psnr_vivid_baseline_$1.tsv" ;;
    esac
}

OUTPUT_DIR="${1:-/tmp/streamlib-recording-$(date +%s)}"
CODEC="${2:-h264}"

VIVID_TEST_PATTERN="${VIVID_TEST_PATTERN:-7}"  # 7 = "100% Red"
MIN_RECORDED_FRAMES="${MIN_RECORDED_FRAMES:-120}"
RECORD_SECONDS="${RECORD_SECONDS:-90}"
SAMPLE_COUNT="${SAMPLE_COUNT:-6}"
SAMPLE_EVERY="${SAMPLE_EVERY:-2}"
CONTROL_PLANE_PORT="${CONTROL_PLANE_PORT:-9403}"
REPLAY_CONTROL_PLANE_PORT="${REPLAY_CONTROL_PLANE_PORT:-9404}"
REPLAY_SECONDS="${REPLAY_SECONDS:-90}"
TOLERANCE="${TOLERANCE:-0.05}"
INJECT_BUG="${INJECT_BUG:-}"

# ── Prerequisites ────────────────────────────────────────────────────
need() { command -v "$1" >/dev/null || { echo "[recording] missing: $1" >&2; exit 77; }; }
need cargo
need python3
need v4l2-ctl

case "$CODEC" in
    h264|h265) ;;
    *)
        echo "[recording] FAIL: codec '$CODEC' is neither h264 nor h265" >&2
        exit 1
        ;;
esac

# The vivid rig captures the baselines; this arm's whole proof is locking to
# the number that path produced, so a baseline written through it would leave
# nothing to lock to.
if [ "${BASELINE_CAPTURE:-}" = "1" ]; then
    echo "[recording] ERROR: this arm never writes a baseline" >&2
    echo "[recording] It locks to the number e2e_fixture_psnr_vivid.sh captured, with one" >&2
    echo "[recording] recorded file in between. Capture there; a mismatch here is a finding" >&2
    echo "[recording] in the writer, not a reason for a third baseline." >&2
    exit 1
fi

BASELINE_TSV="$(baseline_tsv_for_codec "$CODEC")"
if [ ! -f "$BASELINE_TSV" ]; then
    echo "[recording] FAIL: no vivid baseline for $CODEC at $BASELINE_TSV." >&2
    echo "[recording] Capture one with e2e_fixture_psnr_vivid.sh before gating on it." >&2
    exit 1
fi

STREAMLIB_CLI="$(command -v streamlib || true)"
if [ -z "$STREAMLIB_CLI" ]; then
    STREAMLIB_CLI="$REPO_ROOT/sdk/streamlib-python-wheel/.venv/bin/streamlib"
fi
if [ ! -x "$STREAMLIB_CLI" ]; then
    echo "[recording] SKIP: no streamlib CLI on PATH or at $STREAMLIB_CLI" >&2
    exit 77
fi

# The interpreter beside the CLI, because that is the one whose environment the
# CLI ships in; a bare `python3` can be an unrelated one that happens to be
# first on PATH.
FIXTURE_NODE_PYTHON="$(dirname "$STREAMLIB_CLI")/python3"
if [ ! -x "$FIXTURE_NODE_PYTHON" ]; then
    FIXTURE_NODE_PYTHON="$(command -v python3)"
fi
# The node runs whatever `_engine.abi3.so` that interpreter imports, so an
# extension predating the sink would be measured and reported as a PASS for
# code that is not in the tree. Refused by name instead.
if ! MARKER_IMPORT_FAILURE="$("$FIXTURE_NODE_PYTHON" -c '
import sys

import streamlib

streamlib.Mp4Sink
streamlib.OpusEncoder
getattr(streamlib, sys.argv[1].upper() + "Encoder")
' "$CODEC" 2>&1)"; then
    echo "[recording] SKIP: $FIXTURE_NODE_PYTHON cannot import streamlib's Mp4Sink," >&2
    echo "[recording] OpusEncoder or $CODEC encoder. Rebuild the wheel with" >&2
    echo "[recording] \`maturin develop\` — this measures the extension, not the tree." >&2
    echo "$MARKER_IMPORT_FAILURE" >&2
    exit 77
fi

# vivid is an in-kernel V4L2 test driver — no DKMS or out-of-tree modules.
if ! lsmod | grep -q vivid; then
    echo "[recording] Loading vivid kernel module..."
    if ! sudo modprobe vivid 2>/dev/null; then
        echo "[recording] SKIP: vivid module not available (check kernel config)" >&2
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
    echo "[recording] SKIP: no vivid capture device found" >&2
    exit 77
fi

mkdir -p "$OUTPUT_DIR"
RECORDING_PATH="$OUTPUT_DIR/recording.mp4"
RECORD_LOG="$OUTPUT_DIR/recording.log"
REPLAY_LOG="$OUTPUT_DIR/replay.log"
EXCHANGED_DIR="$OUTPUT_DIR/exchanged"
CONTROL_PLANE_URL="http://127.0.0.1:$CONTROL_PLANE_PORT"
REPLAY_CONTROL_PLANE_URL="http://127.0.0.1:$REPLAY_CONTROL_PLANE_PORT"

# `v4l2-ctl -C test_pattern` formats as "test_pattern: 7 (100% Red)"; field $2
# gives the numeric id only, which is what `-c` takes. Captured rather than
# assumed zero, so another rig's state is put back the way it was found.
ORIGINAL_PATTERN="$(v4l2-ctl -d "$VIVID_DEVICE" -C test_pattern 2>/dev/null | awk '{print $2}')"
if ! [[ "$ORIGINAL_PATTERN" =~ ^[0-9]+$ ]]; then
    echo "[recording] WARN: failed to read original vivid test_pattern; will restore to 0" >&2
    ORIGINAL_PATTERN=0
fi

RUNNING_PID=""
# How the last phase's process ended. Three outcomes, because only one of them
# is a clean stop and the other two are different failures:
#   stopped-cleanly  it was running, took SIGTERM, and exited inside the budget
#   needed-sigkill   it was running, ignored SIGTERM, and had to be killed
#   already-gone     it was not running when we went to stop it
# `already-gone` is a failure and not a shortcut: the process crashed, or its
# own `timeout` wrapper fired and killed it. Either way the graph never took a
# SIGTERM, so teardown — which is what closes the last fragment — never ran,
# and the file on disk stops at whatever the writer had already flushed.
STOP_OUTCOME=""
STOP_EXIT_STATUS=""
stop_running_process() {
    STOP_OUTCOME="never-started"
    STOP_EXIT_STATUS=""
    if [ -z "$RUNNING_PID" ]; then
        return 0
    fi
    if ! kill -0 "$RUNNING_PID" 2>/dev/null; then
        STOP_OUTCOME="already-gone"
    else
        # SIGTERM so the graph tears down the way a real stop does. For the
        # record phase that is load-bearing rather than tidy: teardown is what
        # closes the open fragment.
        kill -TERM "$RUNNING_PID" 2>/dev/null || true
        for _ in $(seq 1 50); do
            kill -0 "$RUNNING_PID" 2>/dev/null || break
            sleep 0.2
        done
        if kill -0 "$RUNNING_PID" 2>/dev/null; then
            STOP_OUTCOME="needed-sigkill"
            kill -9 "$RUNNING_PID" 2>/dev/null || true
        else
            STOP_OUTCOME="stopped-cleanly"
        fi
    fi
    STOP_EXIT_STATUS=0
    wait "$RUNNING_PID" 2>/dev/null || STOP_EXIT_STATUS=$?
    RUNNING_PID=""
}

# Every phase ends here, and only `stopped-cleanly` continues.
require_clean_stop() {
    local phase="$1" log_file="$2"
    case "$STOP_OUTCOME" in
        stopped-cleanly)
            return 0
            ;;
        needed-sigkill)
            echo "[recording] FAIL: the $phase process did not exit on SIGTERM and needed" >&2
            echo "[recording] SIGKILL. Teardown is what closes the last fragment, so a hung" >&2
            echo "[recording] stop is a truncated recording as well as a shutdown defect." >&2
            ;;
        already-gone)
            echo "[recording] FAIL: the $phase process was already gone before it was asked" >&2
            echo "[recording] to stop (exit status $STOP_EXIT_STATUS). It crashed, or its budget" >&2
            echo "[recording] ran out and \`timeout\` killed it — 124 is the wrapper firing, 137" >&2
            echo "[recording] its SIGKILL escalation. The graph never took a SIGTERM either way," >&2
            echo "[recording] so teardown never closed the last fragment." >&2
            ;;
        *)
            echo "[recording] FAIL: the $phase process was never started" >&2
            ;;
    esac
    tail -30 "$log_file" >&2
    exit 1
}
restore_pattern_and_stop() {
    stop_running_process
    v4l2-ctl -d "$VIVID_DEVICE" -c "test_pattern=$ORIGINAL_PATTERN" >/dev/null 2>&1 || true
}
trap restore_pattern_and_stop EXIT

if ! v4l2-ctl -d "$VIVID_DEVICE" -c "test_pattern=$VIVID_TEST_PATTERN" 2>"$OUTPUT_DIR/vivid-ctl.log"; then
    echo "[recording] FAIL: could not set vivid test_pattern=$VIVID_TEST_PATTERN" >&2
    cat "$OUTPUT_DIR/vivid-ctl.log" >&2
    exit 1
fi

echo "[recording] Output dir:        $OUTPUT_DIR"
echo "[recording] Vivid device:      $VIVID_DEVICE"
echo "[recording] Test pattern:      $VIVID_TEST_PATTERN (was $ORIGINAL_PATTERN, restored on exit)"
echo "[recording] Codec:             $CODEC"
echo "[recording] Recording:         $RECORDING_PATH"

# ── Build ────────────────────────────────────────────────────────────
cd "$REPO_ROOT"
echo "[recording] Building codec_roundtrip_rig + xtask (release)..."
if ! cargo build --release --locked -p streamlib-engine --example codec_roundtrip_rig \
        > "$OUTPUT_DIR/build.log" 2>&1; then
    echo "[recording] FAIL: rig build failed" >&2
    tail -40 "$OUTPUT_DIR/build.log" >&2
    exit 1
fi
if ! cargo build --release --locked -p xtask >> "$OUTPUT_DIR/build.log" 2>&1; then
    echo "[recording] FAIL: xtask build failed" >&2
    tail -40 "$OUTPUT_DIR/build.log" >&2
    exit 1
fi
XTASK="$REPO_ROOT/target/release/xtask"

# ── Record ───────────────────────────────────────────────────────────
echo "[recording] Recording $VIVID_DEVICE and the known signal..."
RUST_LOG="${RUST_LOG:-warn,streamlib=info,streamlib_media_builtins=info}" \
    timeout --kill-after=5 "$RECORD_SECONDS" \
        "$FIXTURE_NODE_PYTHON" "$SCRIPT_DIR/recording_node.py" \
        --codec "$CODEC" \
        --camera "$VIVID_DEVICE" \
        --path "$RECORDING_PATH" \
        --control-plane-port "$CONTROL_PLANE_PORT" \
        > "$RECORD_LOG" 2>&1 &
RUNNING_PID=$!

for _ in $(seq 1 60); do
    if "$STREAMLIB_CLI" graph --url "$CONTROL_PLANE_URL" >/dev/null 2>&1; then
        break
    fi
    sleep 0.5
done

# How much video has landed on disk so far. The writer buffers, so this lags
# what the graph has produced — which is the point: the phase ends when the
# *file* holds enough to replay, not when the camera claims it published it.
VIDEO_SAMPLE_COUNT_PY=$(cat <<'COUNT_PY'
import json
import sys

report = json.load(sys.stdin)
video_tracks = [track for track in report["tracks"] if track["handler"] == "vide"]
print(video_tracks[0]["samples"] if video_tracks else 0)
COUNT_PY
)
recorded_video_samples() {
    # A file whose `moov` has not landed, or whose last box is still inside the
    # writer's buffer, reads as zero rather than as a failure: both are ordinary
    # states of a healthy run, and the loop below is polling for growth.
    if ! "$XTASK" mp4-inspect "$RECORDING_PATH" \
            > "$OUTPUT_DIR/mp4_inspect_progress.json" 2>/dev/null; then
        echo 0
        return 0
    fi
    python3 -c "$VIDEO_SAMPLE_COUNT_PY" \
        < "$OUTPUT_DIR/mp4_inspect_progress.json" 2>/dev/null || echo 0
}

RECORDED_FRAMES=0
for _ in $(seq 1 $(( RECORD_SECONDS * 2 ))); do
    RECORDED_FRAMES="$(recorded_video_samples)"
    if [ "$RECORDED_FRAMES" -ge "$MIN_RECORDED_FRAMES" ]; then
        break
    fi
    if ! kill -0 "$RUNNING_PID" 2>/dev/null; then
        break
    fi
    sleep 0.5
done
echo "[recording] Landed on disk:    $RECORDED_FRAMES video samples"

stop_running_process
require_clean_stop "recording" "$RECORD_LOG"

if [ ! -s "$RECORDING_PATH" ]; then
    echo "[recording] FAIL: $RECORDING_PATH is empty or missing" >&2
    tail -30 "$RECORD_LOG" >&2
    exit 1
fi

# ── Inspect ──────────────────────────────────────────────────────────
echo "[recording] Inspecting the file..."
if ! "$XTASK" mp4-inspect "$RECORDING_PATH" > "$OUTPUT_DIR/mp4_inspect.json" 2> "$OUTPUT_DIR/mp4_inspect.log"; then
    echo "[recording] FAIL: the recording did not parse" >&2
    cat "$OUTPUT_DIR/mp4_inspect.log" >&2
    exit 1
fi
RECORDING_SHAPE_PY=$(cat <<'INSPECT_PY'
import json
import sys

codec, least_video_samples = sys.argv[1], int(sys.argv[2])
report = json.load(sys.stdin)

failures = []
tracks = report["tracks"]
if len(tracks) != 2:
    failures.append(f"two links entered `tracks`, so the file owes two tracks; it has {len(tracks)}")

video = [track for track in tracks if track["handler"] == "vide"]
audio = [track for track in tracks if track["handler"] == "soun"]
if len(video) != 1 or len(audio) != 1:
    failures.append(
        f"one camera and one signal were recorded, so the file owes one `vide` and one "
        f"`soun` track; it has {len(video)} and {len(audio)}"
    )

expected_sample_entry = {"h264": "avc1", "h265": "hvc1"}[codec]
for track in video:
    if not track["name"].endswith("/encoded_video"):
        failures.append(
            f"the video track is named `{track['name']}`, not after the encoder channel "
            "its link subscribed to"
        )
    if track["sample_entry"]["kind"] != expected_sample_entry:
        failures.append(
            f"the video track is a `{track['sample_entry']['kind']}` entry; a {codec} "
            f"recording writes `{expected_sample_entry}`"
        )
    if track["samples"] < least_video_samples:
        failures.append(
            f"the video track holds {track['samples']} samples, fewer than the "
            f"{least_video_samples} the run waited for — the last fragment did not close"
        )

for track in audio:
    if not track["name"].endswith("/encoded_audio"):
        failures.append(
            f"the audio track is named `{track['name']}`, not after the encoder channel "
            "its link subscribed to"
        )
    if track["sample_entry"]["kind"] != "Opus":
        failures.append(f"the audio track is a `{track['sample_entry']['kind']}` entry, not Opus")
    if track["samples"] == 0:
        failures.append("the audio track holds no samples")

if report["fragment_count"] < 2:
    failures.append(
        f"the file closed {report['fragment_count']} fragments; a recording that never "
        "closed a second one was never playable while it was being written"
    )

for failure in failures:
    print(f"[recording]   {failure}", file=sys.stderr)
sys.exit(1 if failures else 0)
INSPECT_PY
)
if ! python3 -c "$RECORDING_SHAPE_PY" "$CODEC" "$MIN_RECORDED_FRAMES" \
        < "$OUTPUT_DIR/mp4_inspect.json"; then
    echo "[recording] FAIL: the recording is not the two-track file the graph owed" >&2
    echo "[recording] Report: $OUTPUT_DIR/mp4_inspect.json" >&2
    exit 1
fi
echo "[recording] Inspector:         PASS ($OUTPUT_DIR/mp4_inspect.json)"

# ── Replay ───────────────────────────────────────────────────────────
echo "[recording] Replaying the recorded video track through the $CODEC decoder..."
DISPLAY="${DISPLAY:-:0}" \
RUST_LOG="${RUST_LOG:-warn,streamlib=info,streamlib_media_builtins=info}" \
    timeout --kill-after=5 "$REPLAY_SECONDS" \
        "$REPO_ROOT/target/release/examples/codec_roundtrip_rig" \
        --source "mp4:$RECORDING_PATH" \
        --codec "$CODEC" \
        --control-plane-port "$REPLAY_CONTROL_PLANE_PORT" \
        > "$REPLAY_LOG" 2>&1 &
RUNNING_PID=$!

for _ in $(seq 1 60); do
    if "$STREAMLIB_CLI" graph --url "$REPLAY_CONTROL_PLANE_URL" >/dev/null 2>&1; then
        break
    fi
    sleep 0.5
done

# A channel is `{processor_id}/{output_port}` with the id chunk lowercased, and
# a processor id is a cuid2 minted at add time — `decoder` is the rig's display
# name, not its id. Derived from the live graph rather than guessed.
DECODED_CHANNEL="$("$STREAMLIB_CLI" graph --url "$REPLAY_CONTROL_PLANE_URL" 2>/dev/null | python3 -c '
import json, sys
graph = json.load(sys.stdin)
decoder = next(
    (node for node in graph.get("nodes", []) if node.get("display_name") == "decoder"), None
)
if decoder is None:
    sys.exit("the running graph has no processor named `decoder`")
print(decoder["id"].lower() + "/video")
')" || {
    echo "[recording] FAIL: could not read the decoder channel off the replay graph" >&2
    tail -30 "$REPLAY_LOG" >&2
    exit 1
}
echo "[recording] Decoded channel:   $DECODED_CHANNEL"

if ! "$STREAMLIB_CLI" exchange \
        --channel "$DECODED_CHANNEL" \
        --out "$EXCHANGED_DIR" \
        --count "$SAMPLE_COUNT" \
        --every "$SAMPLE_EVERY" \
        --url "$REPLAY_CONTROL_PLANE_URL" \
        > "$OUTPUT_DIR/exchanged_paths.txt" 2> "$OUTPUT_DIR/exchange.log"; then
    echo "[recording] FAIL: exchanged fewer frames than asked for" >&2
    cat "$OUTPUT_DIR/exchange.log" >&2
    tail -30 "$REPLAY_LOG" >&2
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
    echo "[recording] FAIL: copied $sample_index of $SAMPLE_COUNT exchanged frames;" \
         "a drift lock measured on fewer samples than the run asked for reports a" \
         "thinner gate as a full one" >&2
    cat "$OUTPUT_DIR/exchange.log" >&2
    exit 1
fi
echo "[recording] Captured $sample_index frames"

stop_running_process
require_clean_stop "replay" "$REPLAY_LOG"

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

echo ""
if "$XTASK" "${MEASURE_ARGUMENTS[@]}"; then
    echo "[recording] Output dir:        $OUTPUT_DIR"
    echo "[recording] Recording:         $RECORDING_PATH"
    echo "[recording] Inspector JSON:    $OUTPUT_DIR/mp4_inspect.json"
    echo "[recording] Per-sample stats:  $OUTPUT_DIR/channel_means.tsv"
    exit 0
fi
echo "[recording] Output dir:        $OUTPUT_DIR"
echo "[recording] RESULT: FAIL — the decode-back drifted from the vivid baseline"
echo "[recording] The container did not carry the encoder's bytes untouched; the live"
echo "[recording] camera path locks to this same number with no file in between."
exit 1

#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# The WebRTC live proof: the vivid camera and the microphone published to a WHIP
# endpoint, the same stream played back over WHEP, and the decoded video scored
# against the baseline the codec rig captured.
#
# The lock is the decode-back, the argument `Mp4Sink` made. A liveness check
# proves a session opened; a channel mean matched to the vivid baseline within
# ±0.05 proves the pixels that came back are the pixels that went out. The
# network sits inside a path the codec rig already scored, so a mismatch here is
# this wheel's.
#
# Two arms, each reported separately:
#   video   the decode-back, `cargo xtask psnr channel-means` against
#           `psnr_vivid_baseline.tsv`. This is the gate.
#   audio   the block-level channel contract on the decoded audio port, over
#           the real path — cadence, timestamp continuity, rate, channels,
#           dtype. It is the microphone and not the engine's known-signal
#           fixture because that fixture publishes once over 3.78 s and stops,
#           which is shorter than a WHIP ingest plus a WHEP subscribe: the
#           signal would be over before the far side was playing. Scoring
#           signal identity across a network hop wants a looping source the
#           engine does not have, and is `/verify-audio`'s shape.
#
# CREDENTIALS. Cloudflare Stream carries the stream key as a path segment, so
# each URL is itself a credential. Both are read from the environment, passed to
# the node through the environment (never argv, which `/proc` publishes), and
# never printed, logged, or written into the output directory. `streamlib graph`
# renders every processor's config, so this script reads the graph in a pipe and
# never persists it.
#
# Absent credentials are a cannot-run (exit 77) and never a pass.
#
# ONE THING THIS RIG CANNOT SEPARATE. WHIP ingest and WHEP playback of the same
# live input pass through the endpoint's own pipeline, which may transcode. The
# gate is a per-channel mean on a saturated primary, which survives a re-encode;
# a per-pixel PSNR against the reference frames would not, and is why the
# unsuffixed `e2e_fixture_psnr.sh` rig is not the one reused here.
#
# Usage:
#   packages/streamlib-webrtc/tests/live/whip_whep_roundtrip.sh [output_dir]
#
# Environment:
#   STREAMLIB_WHIP_URL / STREAMLIB_WHEP_URL   the two endpoints. Fall back to
#                             CLOUDFLARE_WHIP_URL / CLOUDFLARE_WHEP_URL,
#                             sourced from the repo-root `.env` when present.
#   STREAMLIB_WHIP_BEARER_TOKEN / STREAMLIB_WHEP_BEARER_TOKEN
#                             for an endpoint wanting RFC 9725 bearer auth;
#                             Cloudflare Stream does not.
#   SAMPLE_COUNT/SAMPLE_EVERY the exchange budget (defaults 6 / 2)
#   CONTROL_PLANE_PORT        default 9422
#   RUN_SECONDS               node budget (default 120)
#   MEDIA_DEADLINE_SECONDS    how long to wait for the first decoded frame
#                             after the graph is up (default 60) — an ingest
#                             has to be live before playback can subscribe
#   TOLERANCE                 abs channel-mean drift bound (default 0.05)
#   VIVID_TEST_PATTERN        vivid pattern index (default 7 = "100% Red")
#
# Exit codes: 0 = pass, 1 = fail, 77 = cannot run.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
REPO_ROOT="$(cd "$PACKAGE_DIR/../.." && pwd)"
ENGINE_FIXTURES="$REPO_ROOT/runtime/streamlib-engine/tests/fixtures"
BASELINE_TSV="$ENGINE_FIXTURES/psnr_vivid_baseline.tsv"

OUTPUT_DIR="${1:-/tmp/streamlib-webrtc-live-$(date +%s)}"
SAMPLE_COUNT="${SAMPLE_COUNT:-6}"
SAMPLE_EVERY="${SAMPLE_EVERY:-2}"
CONTROL_PLANE_PORT="${CONTROL_PLANE_PORT:-9422}"
RUN_SECONDS="${RUN_SECONDS:-120}"
MEDIA_DEADLINE_SECONDS="${MEDIA_DEADLINE_SECONDS:-60}"
TOLERANCE="${TOLERANCE:-0.05}"
VIVID_TEST_PATTERN="${VIVID_TEST_PATTERN:-7}"

say() { echo "[webrtc-live] $*"; }
cannot_run() { echo "[webrtc-live] SKIP: $*" >&2; exit 77; }
fail() { echo "[webrtc-live] FAIL: $*" >&2; exit 1; }

# ── What has to be here ──────────────────────────────────────────────
for tool in cargo python3 v4l2-ctl; do
    command -v "$tool" >/dev/null || cannot_run "missing: $tool"
done
compgen -G "/dev/dri/renderD*" >/dev/null \
    || cannot_run "no DRM render node, so no GPU-backed Runtime can start here"
[ -f "$BASELINE_TSV" ] || fail "no vivid baseline at $BASELINE_TSV"

VENV_PYTHON="$PACKAGE_DIR/.venv/bin/python"
STREAMLIB_CLI="$PACKAGE_DIR/.venv/bin/streamlib"
[ -x "$VENV_PYTHON" ] || cannot_run \
    "no venv at $PACKAGE_DIR/.venv — create it and \`maturin develop\` this wheel into it"
[ -x "$STREAMLIB_CLI" ] || cannot_run "the venv has no streamlib CLI; install the engine wheel into it"

# This arm scores whatever `_native.so` that venv holds, so a stale extension
# would be measured and reported as a PASS for code that is not in the tree.
if ! IMPORT_FAILURE="$("$VENV_PYTHON" -c '
import streamlib
from streamlib_webrtc import WhepPlayer, WhipPublisher
_ = (streamlib.H264Decoder, WhepPlayer, WhipPublisher)
' 2>&1)"; then
    say "$IMPORT_FAILURE" >&2
    cannot_run "the venv cannot import this wheel beside the engine. Rebuild with \`maturin develop\` — this arm measures the extension, not the tree."
fi

# ── The credentials, from the environment and never from the tree ────
# `.env` is gitignored and is where the rig keeps them; an already-exported
# value wins, so CI or a shell that set one is never overridden.
if { [ -z "${STREAMLIB_WHIP_URL:-}" ] || [ -z "${STREAMLIB_WHEP_URL:-}" ]; } \
    && [ -f "$REPO_ROOT/.env" ]; then
    set -a
    # shellcheck disable=SC1091
    . "$REPO_ROOT/.env" >/dev/null 2>&1 || true
    set +a
fi
: "${STREAMLIB_WHIP_URL:=${CLOUDFLARE_WHIP_URL:-}}"
: "${STREAMLIB_WHEP_URL:=${CLOUDFLARE_WHEP_URL:-}}"
[ -n "$STREAMLIB_WHIP_URL" ] && [ -n "$STREAMLIB_WHEP_URL" ] || cannot_run \
    "no endpoint credentials. Export STREAMLIB_WHIP_URL and STREAMLIB_WHEP_URL, or put CLOUDFLARE_WHIP_URL and CLOUDFLARE_WHEP_URL in the repo-root .env. Absent credentials are a cannot-run, never a pass."
export STREAMLIB_WHIP_URL STREAMLIB_WHEP_URL

# ── The camera ───────────────────────────────────────────────────────
lsmod | grep -q vivid || sudo modprobe vivid 2>/dev/null \
    || cannot_run "vivid module not available (check kernel config)"
VIVID_DEVICE=""
while read -r dev; do
    if v4l2-ctl -d "$dev" --info 2>/dev/null | grep -q "Video Capture"; then
        VIVID_DEVICE="$dev"
        break
    fi
done < <(v4l2-ctl --list-devices 2>/dev/null | awk '/vivid/{getline; print $1}')
[ -n "$VIVID_DEVICE" ] || cannot_run "no vivid capture device found"

# A busy control port would misdirect this run rather than fail it: the API
# server walks up to ten ports when the one it was given is taken, so a second
# node already on this one would be measured instead.
if (echo >"/dev/tcp/127.0.0.1/$CONTROL_PLANE_PORT") 2>/dev/null; then
    fail "something is already listening on 127.0.0.1:$CONTROL_PLANE_PORT; this run would measure that node instead of its own"
fi

mkdir -p "$OUTPUT_DIR"
EXCHANGED_DIR="$OUTPUT_DIR/exchanged"
MEASURED_DIR="$OUTPUT_DIR/measured"
LOG_FILE="$OUTPUT_DIR/pipeline.log"
CONTROL_PLANE_URL="http://127.0.0.1:$CONTROL_PLANE_PORT"

ORIGINAL_PATTERN="$(v4l2-ctl -d "$VIVID_DEVICE" -C test_pattern 2>/dev/null | awk '{print $2}')"
[[ "$ORIGINAL_PATTERN" =~ ^[0-9]+$ ]] || ORIGINAL_PATTERN=0

NODE_PID=""
NODE_NEEDED_SIGKILL=0
# Redefined once the fixture sink is up; declared here so the EXIT trap,
# which is installed before that, can always call it.
stop_the_signal() { :; }
stop_node() {
    NODE_NEEDED_SIGKILL=0
    if [ -n "$NODE_PID" ] && kill -0 "$NODE_PID" 2>/dev/null; then
        # SIGTERM so the graph tears down the way a real stop does; a killed
        # node would hide a hung teardown — and a WHIP DELETE is part of it.
        kill -TERM "$NODE_PID" 2>/dev/null || true
        for _ in $(seq 1 50); do
            kill -0 "$NODE_PID" 2>/dev/null || break
            sleep 0.2
        done
        if kill -0 "$NODE_PID" 2>/dev/null; then
            NODE_NEEDED_SIGKILL=1
            kill -9 "$NODE_PID" 2>/dev/null || true
        fi
        wait "$NODE_PID" 2>/dev/null || true
    fi
    NODE_PID=""
}
restore_and_stop() {
    stop_node
    stop_the_signal
    v4l2-ctl -d "$VIVID_DEVICE" -c "test_pattern=$ORIGINAL_PATTERN" >/dev/null 2>&1 || true
}
trap restore_and_stop EXIT

v4l2-ctl -d "$VIVID_DEVICE" -c "test_pattern=$VIVID_TEST_PATTERN" 2>"$OUTPUT_DIR/vivid-ctl.log" \
    || fail "could not set vivid test_pattern=$VIVID_TEST_PATTERN"

say "Output dir:        $OUTPUT_DIR"
say "Vivid device:      $VIVID_DEVICE"
say "Test pattern:      $VIVID_TEST_PATTERN (was $ORIGINAL_PATTERN, restored on exit)"
say "Control plane:     $CONTROL_PLANE_URL"
say "Endpoints:         <redacted — each URL carries the account's stream key>"

# ── Build the scorer ─────────────────────────────────────────────────
cd "$REPO_ROOT" || fail "cannot enter $REPO_ROOT"
say "Building xtask (release)..."
cargo build --release --locked -p xtask > "$OUTPUT_DIR/build.log" 2>&1 \
    || { tail -40 "$OUTPUT_DIR/build.log" >&2; fail "xtask build failed"; }

# ── The audio the run measures ───────────────────────────────────────
# The known signal, looped into the fixture's own null sink, with that sink's
# monitor handed to the capture block. Without it the arm measures whatever the
# machine's default input happens to be — on a rig with no live source that is
# nothing, and an empty decoder then reads as this wheel losing audio it was
# never handed. The signal is 3.78 s and plays once, which is shorter than a
# connect, so it is replayed for as long as the node runs.
AUDIO_CAPTURE_DEVICE=""
SIGNAL_PLAYER_PID=""
FIXTURE_SINK=""
stop_the_signal() {
    if [ -n "$SIGNAL_PLAYER_PID" ]; then
        pkill -P "$SIGNAL_PLAYER_PID" 2>/dev/null || true
        kill "$SIGNAL_PLAYER_PID" 2>/dev/null || true
        SIGNAL_PLAYER_PID=""
    fi
    if [ -n "$FIXTURE_SINK" ]; then
        "$ENGINE_FIXTURES/virtual_audio_device.sh" stop >/dev/null 2>&1 || true
        FIXTURE_SINK=""
    fi
}
if "$ENGINE_FIXTURES/virtual_audio_device.sh" check >/dev/null 2>&1 \
    && FIXTURE_SINK="$("$ENGINE_FIXTURES/virtual_audio_device.sh" start 2>/dev/null)" \
    && [ -n "$FIXTURE_SINK" ]; then
    if python3 "$ENGINE_FIXTURES/known_audio_signal.py" generate \
            "$OUTPUT_DIR/known_signal.wav" >/dev/null 2>&1; then
        AUDIO_CAPTURE_DEVICE="$FIXTURE_SINK.monitor"
        ( while true; do
              pw-play --target="$FIXTURE_SINK" "$OUTPUT_DIR/known_signal.wav" \
                  >/dev/null 2>&1 || break
          done ) &
        SIGNAL_PLAYER_PID=$!
        say "Audio source:      the known signal, looped into $FIXTURE_SINK"
    else
        stop_the_signal
        say "Audio source:      the backend default (the known signal would not generate)"
    fi
else
    FIXTURE_SINK=""
    say "Audio source:      the backend default (no PipeWire session for the fixture sink)"
fi

# ── Run ──────────────────────────────────────────────────────────────
say "Publishing over WHIP and playing back over WHEP..."
DISPLAY="${DISPLAY:-:0}" \
RUST_LOG="${RUST_LOG:-warn,streamlib=info,streamlib_media_builtins=info}" \
    timeout --kill-after=5 "$RUN_SECONDS" \
        "$VENV_PYTHON" "$SCRIPT_DIR/whip_whep_roundtrip_node.py" \
            --camera "$VIVID_DEVICE" \
            ${AUDIO_CAPTURE_DEVICE:+--audio-capture-device "$AUDIO_CAPTURE_DEVICE"} \
            --control-plane-port "$CONTROL_PLANE_PORT" \
        > "$LOG_FILE" 2>&1 &
NODE_PID=$!

for _ in $(seq 1 120); do
    "$STREAMLIB_CLI" graph --url "$CONTROL_PLANE_URL" >/dev/null 2>&1 && break
    sleep 0.5
done

# A channel is `{processor_id}/{output_port}` with the id chunk lowercased, and
# the id is a cuid2 minted at add time — the display name is not it. Derived
# from the live graph, in a pipe: the graph renders every processor's config,
# and this graph's config holds both endpoint URLs.
channel_of() {
    "$STREAMLIB_CLI" graph --url "$CONTROL_PLANE_URL" 2>/dev/null | python3 -c '
import json, sys
graph = json.load(sys.stdin)
wanted_display_name, wanted_port = sys.argv[1], sys.argv[2]
node = next(
    (n for n in graph.get("nodes", []) if n.get("display_name") == wanted_display_name),
    None,
)
if node is None:
    sys.exit(f"the running graph has no processor named `{wanted_display_name}`")
print(node["id"].lower() + "/" + wanted_port)
' "$1" "$2"
}

DECODED_VIDEO_CHANNEL="$(channel_of video_decoder video)" \
    || { tail -40 "$LOG_FILE" >&2; fail "could not read the video decoder's channel off the live graph"; }
DECODED_AUDIO_CHANNEL="$(channel_of audio_decoder audio)" \
    || { tail -40 "$LOG_FILE" >&2; fail "could not read the audio decoder's channel off the live graph"; }
say "Decoded video:     $DECODED_VIDEO_CHANNEL"
say "Decoded audio:     $DECODED_AUDIO_CHANNEL"

# The WHIP offer/answer, the endpoint making the live input available, the WHEP
# subscribe and the first IDR all sit between the graph coming up and the first
# decoded frame. Waiting for one bag before spending the exchange budget is what
# keeps a slow ingest from reading as an empty channel — `exchange` gives up
# after 8 tap rounds.
say "Waiting for the first decoded frame (deadline ${MEDIA_DEADLINE_SECONDS}s)..."
# `tap` never fails on a quiet channel — it returns a partial sample and exits 0
# — so the bag count is the only readiness signal. Reading its exit code instead
# reports a channel that has produced nothing as ready, and the run then spends
# the exchange budget before the far side has connected.
tapped_bag_count() {
    "$STREAMLIB_CLI" tap "$1" --count 1 --url "$CONTROL_PLANE_URL" 2>/dev/null | python3 -c '
import json, sys
try:
    print(json.load(sys.stdin).get("received", 0))
except Exception:
    print(0)
'
}
FIRST_FRAME_SEEN=0
for _ in $(seq 1 "$MEDIA_DEADLINE_SECONDS"); do
    if [ "$(tapped_bag_count "$DECODED_VIDEO_CHANNEL")" -gt 0 ] 2>/dev/null; then
        FIRST_FRAME_SEEN=1
        break
    fi
    kill -0 "$NODE_PID" 2>/dev/null || break
    sleep 1
done
if [ "$FIRST_FRAME_SEEN" -ne 1 ]; then
    tail -60 "$LOG_FILE" >&2
    fail "no frame reached the decoder within ${MEDIA_DEADLINE_SECONDS}s. The publish never reached the endpoint, or playback never subscribed to it."
fi

# ── The video arm: the decode-back ───────────────────────────────────
if ! "$STREAMLIB_CLI" exchange \
        --channel "$DECODED_VIDEO_CHANNEL" \
        --out "$EXCHANGED_DIR" \
        --count "$SAMPLE_COUNT" \
        --every "$SAMPLE_EVERY" \
        --url "$CONTROL_PLANE_URL" \
        > "$OUTPUT_DIR/exchanged_paths.txt" 2> "$OUTPUT_DIR/exchange.log"; then
    cat "$OUTPUT_DIR/exchange.log" >&2
    tail -40 "$LOG_FILE" >&2
    fail "exchanged fewer frames than asked for"
fi
cat "$OUTPUT_DIR/exchange.log"

# Read the printed paths, not the directory: --out is not cleared, so a listing
# can hand back an earlier run's frames.
mkdir -p "$MEASURED_DIR"
sample_index=0
while read -r exchanged_png; do
    [ -f "$exchanged_png" ] || continue
    cp "$exchanged_png" "$MEASURED_DIR/$(printf "%04d" "$sample_index").png"
    sample_index=$(( sample_index + 1 ))
done < "$OUTPUT_DIR/exchanged_paths.txt"
[ "$sample_index" -eq "$SAMPLE_COUNT" ] \
    || fail "copied $sample_index of $SAMPLE_COUNT exchanged frames; a drift lock measured on fewer samples than the run asked for reports a thinner gate as a full one"
say "Captured $sample_index frames"

# ── The audio arm: the block contract on what came back ──────────────
# Judged against what was *sent*, not in isolation. A rig whose default capture
# device publishes nothing — no live input, muted, no source — would otherwise
# read as this wheel losing the audio it was handed, which is the one confusion
# an audio arm exists to prevent. So the encoder's own output is tapped first:
# silent there means the arm cannot run, and only a decoder that stayed empty
# while the encoder spoke is a failure.
ENCODED_AUDIO_CHANNEL="$(channel_of audio_encoder encoded_audio)" || ENCODED_AUDIO_CHANNEL=""
PUBLISHED_AUDIO_BAGS=0
if [ -n "$ENCODED_AUDIO_CHANNEL" ]; then
    PUBLISHED_AUDIO_BAGS="$(tapped_bag_count "$ENCODED_AUDIO_CHANNEL")"
fi
if [ "${PUBLISHED_AUDIO_BAGS:-0}" -eq 0 ] 2>/dev/null; then
    AUDIO_VERDICT="cannot run — this rig's capture device published no Opus packets, so nothing was sent to measure coming back"
elif PYTHON="$VENV_PYTHON" "$ENGINE_FIXTURES/verify_audio_channel.sh" audio_decoder \
        --url "$CONTROL_PLANE_URL" --port audio --count 8 \
        > "$OUTPUT_DIR/audio_channel.json" 2> "$OUTPUT_DIR/audio_channel.log"; then
    AUDIO_VERDICT="pass"
else
    AUDIO_VERDICT="fail"
fi
say "Audio channel:     $AUDIO_VERDICT ($OUTPUT_DIR/audio_channel.json)"

stop_node
[ "$NODE_NEEDED_SIGKILL" -eq 0 ] \
    || fail "the node did not exit on SIGTERM and needed SIGKILL; a teardown that hangs is a finding, not a slow exit"

# ── Measure ──────────────────────────────────────────────────────────
echo ""
say "Log gates:"
for pattern in OUT_OF_DEVICE_MEMORY DEVICE_LOST "process() failed" "Validation Error"; do
    printf '  %-24s %s\n' "$pattern" "$(grep -cF "$pattern" "$LOG_FILE" 2>/dev/null; true)"
done
echo ""

if "$REPO_ROOT/target/release/xtask" psnr channel-means \
        --images "$MEASURED_DIR" \
        --baseline "$BASELINE_TSV" \
        --tolerance "$TOLERANCE" \
        --report "$OUTPUT_DIR/channel_means.tsv"; then
    say "Output dir:        $OUTPUT_DIR"
    say "Per-sample stats:  $OUTPUT_DIR/channel_means.tsv"
    say "RESULT: video PASS · audio $AUDIO_VERDICT"
    # A `cannot run` audio verdict is a silent rig and stays a report; a `fail`
    # one is audio this wheel was handed and did not deliver back.
    case "$AUDIO_VERDICT" in
        fail*) exit 1 ;;
    esac
    exit 0
fi
say "Output dir:        $OUTPUT_DIR"
say "RESULT: FAIL — the frames that came back off the endpoint drift from the baseline"
exit 1

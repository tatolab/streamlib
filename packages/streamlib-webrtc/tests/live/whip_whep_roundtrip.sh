#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# The WebRTC live proof: the test pattern and the microphone published to a
# WHIP endpoint by one stream, the same live input played back over WHEP by
# another, both on one runtime, and the video that came back scored bar by bar
# against this wheel's own baseline.
#
# It is the engine's codec round trip (`e2e_fixture_psnr_test_pattern.sh`) with
# the network where the local relay was: the sender
# (`TestPatternSource → H264Encoder → WhipPublisher`) and the receiver
# (`WhepPlayer → H264Decoder`) share nothing on this machine. The receiver's
# player port — the frames that crossed the network — is read the way any user
# reads a port: `tatolab-read-track` at the local relay's loopback listener,
# the rig's ffmpeg decodes the dump to PNGs, and `cargo xtask psnr bar-colors`
# measures each SMPTE bar's median colour and injects. The receiver's own
# decode is proven by the count of frames its decoder wrote, read off `graph`.
#
# Two arms, each reported separately:
#   video   the decode-back, `cargo xtask psnr bar-colors` against
#           `psnr_test_pattern_bar_colors_baseline_whip_whep_h264.tsv`. This is
#           the gate.
#   audio   the block-level channel contract on the decoded audio port, over
#           the real path — cadence, timestamp continuity, rate, channels,
#           dtype. It is the microphone and not the engine's known-signal
#           fixture because that fixture publishes once over 3.78 s and stops,
#           which is shorter than a WHIP ingest plus a WHEP subscribe: the
#           signal would be over before the far side was playing. Scoring
#           signal identity across a network hop wants a looping source the
#           engine does not have, and is `/verify-audio`'s shape.
#
# The baseline is this wheel's own, not the codec rig's: WHIP ingest and WHEP
# playback of one live input pass through the endpoint's own pipeline, which
# may transcode. A saturated bar's median survives a re-encode; a matrix
# mis-interpretation moves it by far more than the tolerance.
#
# Range mis-interpretation is not part of this lock, for the reason the codec
# rig's header gives: saturated bars sit at the ends of the coded range and a
# range swap clips straight back to them.
#
# The streams (`whip_whep_roundtrip_stream.py`) are loaded with `tatolab run
# --name`, compiled in this package's own `.venv` — `tatolab-stream` and a
# current `maturin develop` of this wheel, and no engine — on a `tatolabd` this
# script starts and stops itself. The runtime unit, its `tatolab` verbs and the
# runtime's start and stop are the engine fixtures' `fixture_runtime_unit.sh`,
# so the same rules hold: the runtime takes the machine's lock and serves this
# user's local API socket, so the run is refused while another runtime holds
# the machine; and the streams' settings go in the runtime's environment, which
# their compiles and their processor interpreters inherit, never `tatolab
# run`'s.
#
# CREDENTIALS. Cloudflare Stream carries the stream key as a path segment, so
# each URL is itself a credential. Both are read from the environment, passed to
# the streams through the runtime's environment (never argv, which `/proc`
# publishes), and never printed, logged, or written into the output directory —
# which the run checks, since the runtime's log is kept there. `tatolab graph`
# renders every processor's config, so this script reads the graph in a pipe and
# never persists it.
#
# Absent credentials are a cannot-run (exit 77) and never a pass.
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
#   OBJECT_COUNT              encoded frames read off the player's track and
#                             measured (default 20)
#   DECODED_FRAMES_FLOOR      frames the receiver's decoder must have written
#                             (default 10)
#   RUN_SECONDS               budget of each stream run (default 120)
#   MEDIA_DEADLINE_SECONDS    how long to wait for the first decoded frame
#                             after both streams are up (default 60) — an
#                             ingest has to be live before playback can
#                             subscribe
#   TOLERANCE                 abs per-bar channel drift bound on [0,1] scale
#                             (default 0.05)
#   BASELINE_CAPTURE          set to 1 to overwrite the checked-in baseline
#                             instead of comparing
#   INJECT_BUG                bt601-bt709 | swap-channels | swap-chroma; each
#                             must fail naming bars
#
# Exit codes: 0 = pass, 1 = fail, 77 = cannot run.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
REPO_ROOT="$(cd "$PACKAGE_DIR/../.." && pwd)"
ENGINE_FIXTURES="$REPO_ROOT/runtime/streamlib-engine/tests/fixtures"
BASELINE_TSV="$SCRIPT_DIR/psnr_test_pattern_bar_colors_baseline_whip_whep_h264.tsv"

OUTPUT_DIR="${1:-/tmp/streamlib-webrtc-live-$(date +%s)}"
OBJECT_COUNT="${OBJECT_COUNT:-20}"
DECODED_FRAMES_FLOOR="${DECODED_FRAMES_FLOOR:-10}"
RUN_SECONDS="${RUN_SECONDS:-120}"
MEDIA_DEADLINE_SECONDS="${MEDIA_DEADLINE_SECONDS:-60}"
TOLERANCE="${TOLERANCE:-0.05}"
BASELINE_CAPTURE="${BASELINE_CAPTURE:-}"
INJECT_BUG="${INJECT_BUG:-}"

SENDER_STREAM_NAME="whip_whep_roundtrip_sender"
RECEIVER_STREAM_NAME="whip_whep_roundtrip_receiver"
PLAYER_ENCODED_VIDEO_TRACK="player/encoded_video"

say() { echo "[webrtc-live] $*"; }
cannot_run() { echo "[webrtc-live] SKIP: $*" >&2; exit 77; }
fail() { echo "[webrtc-live] FAIL: $*" >&2; exit 1; }

# ── What has to be here ──────────────────────────────────────────────
for tool in cargo python3 ffmpeg; do
    command -v "$tool" >/dev/null || cannot_run "missing: $tool"
done
compgen -G "/dev/dri/renderD*" >/dev/null \
    || cannot_run "no DRM render node, so no GPU-backed runtime can start here"
if [ ! -f "$BASELINE_TSV" ] && [ "$BASELINE_CAPTURE" != "1" ]; then
    fail "no baseline at $BASELINE_TSV. Capture one on the rig with BASELINE_CAPTURE=1 before gating on it."
fi
[ "$INJECT_BUG" != "range-swap" ] \
    || fail "INJECT_BUG=range-swap is not part of this rig's lock (the header says why)"

# shellcheck source=../../../../runtime/streamlib-engine/tests/fixtures/fixture_runtime_unit.sh
. "$ENGINE_FIXTURES/fixture_runtime_unit.sh"
# The fixture venv reads the live graph's state, and the audio arm decodes bags in it.
require_the_runtime_unit
require_the_fixture_venv
refuse_while_a_runtime_answers_at_the_local_api_socket \
    || cannot_run "a runtime already holds this machine; this run starts its own"

VENV_PYTHON="$PACKAGE_DIR/.venv/bin/python"
[ -x "$VENV_PYTHON" ] || cannot_run \
    "no venv at $PACKAGE_DIR/.venv — create it with tatolab-stream and \`maturin develop\` this wheel into it"

# This arm scores whatever `_native.so` that venv holds, so a stale extension
# would be measured and reported as a PASS for code that is not in the tree.
if ! IMPORT_FAILURE="$("$VENV_PYTHON" -c '
import tatolab.stream
from tatolab.webrtc import WhepPlayer, WhipPublisher
_ = (tatolab.stream.TestPatternSource, tatolab.stream.H264Decoder, WhepPlayer, WhipPublisher)
' 2>&1)"; then
    say "$IMPORT_FAILURE" >&2
    cannot_run "the venv cannot import this wheel beside tatolab-stream. Rebuild with \`maturin develop\` — this arm measures the extension, not the tree."
fi

# ── The credentials, from the environment and never from the tree ────
# `.env` is gitignored and is where the rig keeps them. Sourced only when a
# `STREAMLIB_` name is missing, and `set -a` then exports everything it holds —
# so a `CLOUDFLARE_` value it carries can replace one already in the
# environment. Export the `STREAMLIB_` names to pin a run to exactly what you
# meant; those are read first and are never re-derived.
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

mkdir -p "$OUTPUT_DIR"
ENCODED_TRACK_DIR="$OUTPUT_DIR/encoded_track"
MEASURED_DIR="$OUTPUT_DIR/measured"
# The runtime's output — the engine's log mirror, every stream's records among
# it — with each `tatolab run`'s beside it.
LOG_FILE="$OUTPUT_DIR/runtime.log"
SENDER_RUN_LOG_FILE="$OUTPUT_DIR/sender_run.log"
RECEIVER_RUN_LOG_FILE="$OUTPUT_DIR/receiver_run.log"
# Outside the output directory and removed on exit: the runtime keeps what it
# loaded there, and the streams' config holds both endpoint URLs.
RUNTIME_STATE_HOME="$(mktemp -d "${TMPDIR:-/tmp}/streamlib-webrtc-live-runtime-state-XXXXXX")"

STREAM_RUN_PIDS=()
STREAM_RUN_NEEDED_SIGKILL=0
# Redefined once the fixture sink is up; declared here so the EXIT trap,
# which is installed before that, can always call it.
stop_the_signal() { :; }
stop_the_stream_runs() {
    local stream_run_pid
    for stream_run_pid in "${STREAM_RUN_PIDS[@]}"; do
        kill -0 "$stream_run_pid" 2>/dev/null || continue
        # SIGTERM so the graph tears down the way a real stop does; a killed
        # run would hide a hung teardown — and a WHIP DELETE is part of it.
        # `timeout` passes it on; `tatolab run` stops its stream, which the
        # runtime unloads before the run exits.
        kill -TERM "$stream_run_pid" 2>/dev/null || true
        for _ in $(seq 1 50); do
            kill -0 "$stream_run_pid" 2>/dev/null || break
            sleep 0.2
        done
        if kill -0 "$stream_run_pid" 2>/dev/null; then
            STREAM_RUN_NEEDED_SIGKILL=1
            kill -9 "$stream_run_pid" 2>/dev/null || true
        fi
        wait "$stream_run_pid" 2>/dev/null || true
    done
    STREAM_RUN_PIDS=()
}
# The runtime holds every credential in its environment and the URLs in the
# streams' config, and its log sits in the output directory — so it is scanned
# however the run ends, a failed run's above all.
refuse_a_credential_written_into_the_output_directory() {
    local credential
    for credential in "${STREAMLIB_WHIP_URL:-}" "${STREAMLIB_WHEP_URL:-}" \
        "${STREAMLIB_WHIP_BEARER_TOKEN:-}" "${STREAMLIB_WHEP_BEARER_TOKEN:-}"; do
        [ -n "$credential" ] || continue
        if grep -rqF -- "$credential" "$OUTPUT_DIR"; then
            echo "[webrtc-live] FAIL: an endpoint credential was written into $OUTPUT_DIR;" \
                "delete the directory before sharing any of it" >&2
            exit 1
        fi
    done
}
stop_everything_and_scan_for_credentials() {
    stop_the_stream_runs
    stop_the_fixture_runtime
    rm -rf "$RUNTIME_STATE_HOME"
    stop_the_signal
    refuse_a_credential_written_into_the_output_directory
}
trap stop_everything_and_scan_for_credentials EXIT

say "Output dir:        $OUTPUT_DIR"
say "Endpoints:         <redacted — each URL carries the account's stream key>"

# ── Build the reader and the scorer ──────────────────────────────────
cd "$REPO_ROOT" || fail "cannot enter $REPO_ROOT"
say "Building tatolab-read-track + xtask (release)..."
require_tatolab_read_track
cargo build --release --locked -p xtask > "$OUTPUT_DIR/build.log" 2>&1 \
    || { tail -40 "$OUTPUT_DIR/build.log" >&2; fail "xtask build failed"; }

# ── The audio the run measures ───────────────────────────────────────
# The known signal, looped into the fixture's own null sink, with that sink's
# monitor handed to the capture block. Without it the arm measures whatever the
# machine's default input happens to be — on a rig with no live source that is
# nothing, and an empty decoder then reads as this wheel losing audio it was
# never handed. The signal is 3.78 s and plays once, which is shorter than a
# connect, so it is replayed for as long as the streams run.
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
if ! STREAMLIB_AUDIO_CAPTURE_DEVICE_ID="$AUDIO_CAPTURE_DEVICE" \
    RUST_LOG="${RUST_LOG:-warn,streamlib=info,streamlib_media_builtins=info}" \
        start_the_fixture_runtime "$LOG_FILE" "$RUNTIME_STATE_HOME"; then
    fail "could not start the runtime the streams run on"
fi

run_the_fixture_stream() {
    local stream_name="$1" stream_function="$2" run_log_file="$3"
    timeout --kill-after=5 "$RUN_SECONDS" \
        "$TATOLAB_EXECUTABLE" run --dir "$PACKAGE_DIR" \
            --name "$stream_name" \
            "tests/live/whip_whep_roundtrip_stream.py:$stream_function" \
        > "$run_log_file" 2>&1 &
    STREAM_RUN_PIDS+=("$!")
    if ! wait_until_the_stream_is_running "$stream_name" "$!" 60; then
        tail -40 "$run_log_file" >&2
        tail -40 "$LOG_FILE" >&2
        fail "the stream $stream_name never ran — it never answered over the local API socket with every node Running"
    fi
    say "Running:           $stream_name"
}
say "Publishing over WHIP and playing back over WHEP..."
run_the_fixture_stream "$SENDER_STREAM_NAME" \
    test_pattern_and_microphone_published_over_whip "$SENDER_RUN_LOG_FILE"
run_the_fixture_stream "$RECEIVER_STREAM_NAME" \
    the_same_input_played_back_over_whep_through_the_decoders "$RECEIVER_RUN_LOG_FILE"

# How many messages <node>'s <port> in <stream> has written, off the live
# graph, in a pipe: the graph renders every node's config, and these hold both
# endpoint URLs.
written_bags_of() {
    tatolab_observation_verb graph --stream "$1" 2>/dev/null | "$FIXTURE_PYTHON" -c '
import json, sys
try:
    graph = json.load(sys.stdin)
    node = next(node for node in graph["nodes"] if node["name"] == sys.argv[1])
    print(node["components"]["metrics"]["written_bags_by_output_port"].get(sys.argv[2], 0))
except Exception:
    print(0)
' "$2" "$3"
}
wait_until_the_decoder_has_written() {
    local frame_floor="$1" deadline_seconds="$2"
    for _ in $(seq 1 "$deadline_seconds"); do
        RECEIVER_DECODED_FRAMES="$(written_bags_of "$RECEIVER_STREAM_NAME" video_decoder video)"
        [ "$RECEIVER_DECODED_FRAMES" -ge "$frame_floor" ] 2>/dev/null && return 0
        sleep 1
    done
    return 1
}

# The WHIP offer/answer, the endpoint making the live input available, the WHEP
# subscribe and the first IDR all sit between the streams coming up and the
# first decoded frame. The player states its port's catalog entry from the
# stream it receives, so its track is read only once a frame has come back.
say "Waiting for the first decoded frame (deadline ${MEDIA_DEADLINE_SECONDS}s)..."
RECEIVER_DECODED_FRAMES=0
wait_until_the_decoder_has_written 1 "$MEDIA_DEADLINE_SECONDS" || {
    tail -60 "$LOG_FILE" >&2
    fail "no frame reached the decoder within ${MEDIA_DEADLINE_SECONDS}s. The publish never reached the endpoint, or playback never subscribed to it."
}

# ── The video arm: what came back off the endpoint ───────────────────
MACHINE_NAME="$(the_machine_name_the_loopback_listing_names)" \
    || fail "the runtime's loopback listing at $LOOPBACK_HTTP_LISTING_URL did not name the machine"
RECEIVER_BROADCAST="$MACHINE_NAME/$RECEIVER_STREAM_NAME.hang"
say "Reading:           $PLAYER_ENCODED_VIDEO_TRACK of $RECEIVER_BROADCAST"
if ! read_the_track_at_the_local_relay "$RECEIVER_BROADCAST" "$PLAYER_ENCODED_VIDEO_TRACK" \
        "$OBJECT_COUNT" "$ENCODED_TRACK_DIR" 2> "$OUTPUT_DIR/read_track.log"; then
    cat "$OUTPUT_DIR/read_track.log" >&2
    tail -30 "$LOG_FILE" >&2
    fail "the player's encoded track did not read"
fi
cat "$OUTPUT_DIR/read_track.log"
grep -q '"payload":"codec_payload_of_the_hang_legacy_container"' \
        "$ENCODED_TRACK_DIR/objects.jsonl" \
    || fail "tatolab-read-track wrote the player's track as sent, not as its codec payload — the catalog never listed it as a legacy-container video track"

wait_until_the_decoder_has_written "$DECODED_FRAMES_FLOOR" 30
say "Receiver decoded:  $RECEIVER_DECODED_FRAMES frames"
[ "$RECEIVER_DECODED_FRAMES" -ge "$DECODED_FRAMES_FLOOR" ] 2>/dev/null || {
    tail -30 "$RECEIVER_RUN_LOG_FILE" >&2
    tail -30 "$LOG_FILE" >&2
    fail "the receiver's decoder wrote $RECEIVER_DECODED_FRAMES frames, fewer than $DECODED_FRAMES_FLOOR"
}

# ── The audio arm: the block contract on what came back ──────────────
# Judged against what was *sent*, not in isolation. A rig whose default capture
# device publishes nothing — no live input, muted, no source — would otherwise
# read as this wheel losing the audio it was handed, which is the one confusion
# an audio arm exists to prevent. So the sender's encoder count is read first:
# silent there means the arm cannot run, and only a decoder that stayed empty
# while the encoder spoke is a failure.
PUBLISHED_AUDIO_BAGS="$(written_bags_of "$SENDER_STREAM_NAME" audio_encoder encoded_audio)"
if [ "${PUBLISHED_AUDIO_BAGS:-0}" -eq 0 ] 2>/dev/null; then
    AUDIO_VERDICT="cannot run — this rig's capture device published no Opus packets, so nothing was sent to measure coming back"
elif "$ENGINE_FIXTURES/verify_audio_channel.sh" audio_decoder \
        --stream "$RECEIVER_STREAM_NAME" --port audio --count 8 \
        > "$OUTPUT_DIR/audio_channel.json" 2> "$OUTPUT_DIR/audio_channel.log"; then
    AUDIO_VERDICT="pass"
else
    AUDIO_VERDICT="fail"
fi
say "Audio channel:     $AUDIO_VERDICT ($OUTPUT_DIR/audio_channel.json)"

stop_the_stream_runs
[ "$STREAM_RUN_NEEDED_SIGKILL" -eq 0 ] \
    || fail "a stream run did not exit on SIGTERM and needed SIGKILL; a teardown that hangs is a finding, not a slow exit"
stop_the_fixture_runtime 15
the_fixture_runtime_stopped_cleanly \
    || fail "the runtime did not stop cleanly once its streams were unloaded; a teardown that hangs is a finding, not a slow exit"

echo ""
say "Log gates:"
for pattern in OUT_OF_DEVICE_MEMORY DEVICE_LOST "process() failed" "Validation Error"; do
    printf '  %-24s %s\n' "$pattern" "$(grep -cF "$pattern" "$LOG_FILE" 2>/dev/null; true)"
done
echo ""

# ── Decode ───────────────────────────────────────────────────────────
# The dump's files are an H.264 elementary stream once concatenated in index
# order, which ffmpeg decodes as it would any `.h264` file.
ENCODED_ELEMENTARY_STREAM="$OUTPUT_DIR/encoded.h264"
"$FIXTURE_PYTHON" -c '
import json, pathlib, sys
dump = pathlib.Path(sys.argv[1])
with open(sys.argv[2], "wb") as elementary_stream:
    for line in (dump / "objects.jsonl").read_text().splitlines():
        elementary_stream.write((dump / json.loads(line)["file"]).read_bytes())
' "$ENCODED_TRACK_DIR" "$ENCODED_ELEMENTARY_STREAM" \
    || fail "could not concatenate the dump into an elementary stream"
mkdir -p "$MEASURED_DIR"
ffmpeg -hide_banner -loglevel error -nostdin \
        -f h264 -i "$ENCODED_ELEMENTARY_STREAM" \
        -fps_mode passthrough -pix_fmt rgb24 \
        "$MEASURED_DIR/%04d.png" 2> "$OUTPUT_DIR/ffmpeg.log" \
    || { cat "$OUTPUT_DIR/ffmpeg.log" >&2; fail "ffmpeg could not decode the player's track"; }
DECODED_PNG_COUNT="$(find "$MEASURED_DIR" -name '*.png' | wc -l)"
say "ffmpeg decoded:    $DECODED_PNG_COUNT of $OBJECT_COUNT frames"
[ "$DECODED_PNG_COUNT" -eq "$OBJECT_COUNT" ] || {
    cat "$OUTPUT_DIR/ffmpeg.log" >&2
    fail "ffmpeg decoded $DECODED_PNG_COUNT of the $OBJECT_COUNT frames read; a drift lock measured on fewer frames than the run read reports a thinner gate as a full one"
}

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
        --baseline-note "Generated by packages/streamlib-webrtc/tests/live/whip_whep_roundtrip.sh"
        --baseline-note "Captured with: BASELINE_CAPTURE=1 whip_whep_roundtrip.sh <out>"
        --baseline-note "Codec: h264"
        --baseline-note "Source: TestPatternSource at its default 1280x720, published over WHIP and played back over WHEP."
        --baseline-note "Measured from the receiver's WhepPlayer encoded track read at the local relay with tatolab-read-track and decoded by ffmpeg to RGB PNGs."
    )
fi

if "$REPO_ROOT/target/release/xtask" "${MEASURE_ARGUMENTS[@]}"; then
    say "Output dir:        $OUTPUT_DIR"
    say "Per-sample stats:  $OUTPUT_DIR/bar_colors.tsv"
    say "RESULT: video PASS · audio $AUDIO_VERDICT"
    # A `fail` is audio this wheel was handed and did not deliver back; a
    # `cannot run` is a silent rig. Neither may exit 0, because 0 is read as
    # "everything this run claims to prove was proved" — and an arm that did not
    # run proved nothing. 77 keeps the distinction: a cannot-run is never a pass.
    case "$AUDIO_VERDICT" in
        fail*) exit 1 ;;
        "cannot run"*)
            say "RESULT: cannot run — the audio arm never ran, so this run verified less than it reports"
            exit 77
            ;;
    esac
    exit 0
fi
say "Output dir:        $OUTPUT_DIR"
say "RESULT: FAIL — bar colour drift outside tolerance on the frames that came back off the endpoint"
exit 1

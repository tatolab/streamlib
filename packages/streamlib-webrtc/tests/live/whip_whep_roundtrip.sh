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
# the network in the codec rig's relay's place: the sender
# (`TestPatternSource → H264Encoder → WhipPublisher`) and the receiver
# (`WhepPlayer → H264Decoder`) share nothing on this machine. The receiver's
# player port — the frames that crossed the network — is read the way any user
# reads a track: the receiver sends it to a `moq-relay` the rig starts,
# `tatolab-read-track` reads it there, the rig's ffmpeg decodes the dump to PNGs, and `cargo xtask psnr bar-colors`
# measures each SMPTE bar's median colour and injects. The receiver's own
# decode is proven by the count of frames its decoder wrote, read off `graph`.
#
# Two arms, each reported separately:
#   video   the decode-back, `cargo xtask psnr bar-colors` against
#           `psnr_test_pattern_bar_colors_baseline_whip_whep_h264.tsv`. This is
#           the gate.
#   audio   the receiver's decoder wrote audio blocks off the packets that
#           crossed the network, read off `graph`. It is the microphone and not
#           the engine's known-signal
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
# The streams (`whip_whep_roundtrip_stream.py`) are loaded each as a project of
# its own with `tatolab start --dir`, each graph built in this package's own
# `.venv`, which each project's `.venv` links to — `tatolab-stream` and a current
# `maturin develop` of this wheel, and no engine — on a `tatolabd` this script
# starts and stops itself. The runtime unit, its `tatolab` verbs and the
# runtime's start and stop are the engine fixtures' `fixture_runtime_unit.sh`,
# so the same rules hold: the runtime takes the machine's lock and serves this
# user's local API socket, so the run is refused while another runtime holds
# the machine; and the streams' settings go in the environment each graph is
# built in, never argv.
#
# CREDENTIALS. Cloudflare Stream carries the stream key as a path segment, so
# each URL is itself a credential. Both are read from the environment, passed to
# the graph generators through it (never argv, which `/proc` publishes), written
# only into the owner-only graph documents under the runtime's state home, which
# the run removes, and never printed, logged, or written into the output
# directory — which the run checks, since the runtime's log is kept there. `tatolab graph`
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
MEDIA_DEADLINE_SECONDS="${MEDIA_DEADLINE_SECONDS:-60}"
TOLERANCE="${TOLERANCE:-0.05}"
BASELINE_CAPTURE="${BASELINE_CAPTURE:-}"
INJECT_BUG="${INJECT_BUG:-}"

# The broadcast and track `whip_whep_roundtrip_stream.py`'s receiver sends to.
RECEIVER_BROADCAST="whip-whep-roundtrip-receiver.hang"
PLAYER_ENCODED_VIDEO_TRACK="player_encoded_video"

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
    "no venv at $PACKAGE_DIR/.venv — create it with tatolab-stream and tatolab-opus, and \`maturin develop\` this wheel into it"

# The streams draw, capture, play, code and send video through the test
# pattern, microphone, speaker, H.264 and MoQ gateway modules, built from this
# checkout into that venv when it cannot import them.
if ! "$VENV_PYTHON" -c 'import tatolab.microphone, tatolab.speaker, tatolab.h264, tatolab.testpattern, tatolab.moq' >/dev/null 2>&1; then
    say "building tatolab-microphone, tatolab-speaker, tatolab-h264, tatolab-testpattern and tatolab-moq into $PACKAGE_DIR/.venv"
    VIRTUAL_ENV="$PACKAGE_DIR/.venv" uv pip install --quiet \
        "$REPO_ROOT/packages/tatolab-microphone" "$REPO_ROOT/packages/tatolab-speaker" \
        "$REPO_ROOT/packages/streamlib-h264" "$REPO_ROOT/packages/tatolab-testpattern" \
        "$REPO_ROOT/packages/tatolab-moq" >&2 \
        || cannot_run "the audio, H.264, test pattern and MoQ modules did not build into $PACKAGE_DIR/.venv"
fi

# This arm scores whatever `_native.so` that venv holds, so a stale extension
# would be measured and reported as a PASS for code that is not in the tree.
if ! IMPORT_FAILURE="$("$VENV_PYTHON" -c '
import tatolab.h264
from tatolab.testpattern import TestPatternSource
from tatolab.opus import OpusDecoder, OpusEncoder
from tatolab.webrtc import WhepPlayer, WhipPublisher
_ = (TestPatternSource, tatolab.h264.H264Decoder, OpusEncoder, OpusDecoder, WhepPlayer, WhipPublisher)
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
# it — with each `tatolab start`'s beside it.
LOG_FILE="$OUTPUT_DIR/runtime.log"
SENDER_RUN_LOG_FILE="$OUTPUT_DIR/sender_run.log"
RECEIVER_RUN_LOG_FILE="$OUTPUT_DIR/receiver_run.log"
# Outside the output directory and removed on exit: the runtime keeps what it
# loaded there, and the streams' config holds both endpoint URLs.
RUNTIME_STATE_HOME="$(mktemp -d "${TMPDIR:-/tmp}/streamlib-webrtc-live-runtime-state-XXXXXX")"

# Redefined once the fixture sink is up; declared here so the EXIT trap,
# which is installed before that, can always call it.
stop_the_signal() { :; }
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
    remove_the_fixture_projects
    stop_the_fixture_runtime
    stop_the_fixture_relay
    rm -rf "$RUNTIME_STATE_HOME"
    stop_the_signal
    refuse_a_credential_written_into_the_output_directory
}
trap stop_everything_and_scan_for_credentials EXIT

say "Output dir:        $OUTPUT_DIR"
say "Endpoints:         <redacted — each URL carries the account's stream key>"

# ── Build the reader and the scorer ──────────────────────────────────
cd "$REPO_ROOT" || fail "cannot enter $REPO_ROOT"
say "Building tatolab-read-track, moq-relay and xtask (release)..."
require_tatolab_read_track
require_the_fixture_relay
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
start_the_fixture_relay "$OUTPUT_DIR/relay.log" "$RUNTIME_STATE_HOME/relay" \
    || fail "could not start the relay the receiver sends its player's track to"
say "Relay:             $FIXTURE_RELAY_URL"
if ! RUST_LOG="${RUST_LOG:-warn,streamlib=info,streamlib_media_builtins=info}" \
        start_the_fixture_runtime "$LOG_FILE" "$RUNTIME_STATE_HOME"; then
    fail "could not start the runtime the streams run on"
fi

# Beside the state directory and removed with it, so the credential scan of
# the output directory never walks the venv each `.venv` links to.
SENDER_PROJECT_DIR="$RUNTIME_STATE_HOME/whip-whep-roundtrip-sender"
RECEIVER_PROJECT_DIR="$RUNTIME_STATE_HOME/whip-whep-roundtrip-receiver"
write_the_whip_whep_project() {
    local project_directory="$1" graph_function="$2" stream_name="$3"
    STREAMLIB_AUDIO_CAPTURE_DEVICE_ID="$AUDIO_CAPTURE_DEVICE" \
        STREAMLIB_FIXTURE_RELAY_URL="$FIXTURE_RELAY_URL" \
        write_a_one_stream_fixture_project "$project_directory" \
            "$SCRIPT_DIR/whip_whep_roundtrip_stream.py" "$graph_function" "$stream_name" \
            "$PACKAGE_DIR/.venv" \
        || fail "the graph of $stream_name could not be built"
}
write_the_whip_whep_project "$SENDER_PROJECT_DIR" \
    test_pattern_and_microphone_published_over_whip whip_whep_roundtrip_sender
write_the_whip_whep_project "$RECEIVER_PROJECT_DIR" \
    the_same_input_played_back_over_whep_through_the_decoders whip_whep_roundtrip_receiver

run_the_fixture_project() {
    local project_directory="$1" run_log_file="$2"
    start_the_fixture_project "$project_directory" "$run_log_file" || {
        tail -40 "$run_log_file" >&2
        tail -40 "$LOG_FILE" >&2
        fail "the stream of $(basename "$project_directory") never ran — it never answered over the local API socket with every node Running"
    }
    say "Running:           $FIXTURE_PROJECT_STREAM_ID"
}
say "Publishing over WHIP and playing back over WHEP..."
run_the_fixture_project "$SENDER_PROJECT_DIR" "$SENDER_RUN_LOG_FILE"
SENDER_STREAM_ID="$FIXTURE_PROJECT_STREAM_ID"
run_the_fixture_project "$RECEIVER_PROJECT_DIR" "$RECEIVER_RUN_LOG_FILE"
RECEIVER_STREAM_ID="$FIXTURE_PROJECT_STREAM_ID"

# A graph that does not answer yet is a frame not decoded yet; the floor is
# what fails.
wait_until_the_decoder_has_written() {
    local frame_floor="$1" deadline_seconds="$2"
    for _ in $(seq 1 "$deadline_seconds"); do
        RECEIVER_DECODED_FRAMES="$(written_messages_of_the_output_port \
            "$RECEIVER_STREAM_ID" video_decoder video)" || RECEIVER_DECODED_FRAMES=0
        [ "$RECEIVER_DECODED_FRAMES" -ge "$frame_floor" ] 2>/dev/null && return 0
        sleep 1
    done
    return 1
}

# The WHIP offer/answer, the endpoint making the live input available, the WHEP
# subscribe and the first IDR all sit between the streams coming up and the
# first decoded frame. The player states its port's stream facts from the
# stream it receives, so its track is read only once a frame has come back.
say "Waiting for the first decoded frame (deadline ${MEDIA_DEADLINE_SECONDS}s)..."
RECEIVER_DECODED_FRAMES=0
wait_until_the_decoder_has_written 1 "$MEDIA_DEADLINE_SECONDS" || {
    tail -60 "$LOG_FILE" >&2
    fail "no frame reached the decoder within ${MEDIA_DEADLINE_SECONDS}s. The publish never reached the endpoint, or playback never subscribed to it."
}

# ── The video arm: what came back off the endpoint ───────────────────
say "Reading:           $PLAYER_ENCODED_VIDEO_TRACK of $RECEIVER_BROADCAST"
if ! read_the_track_at_the_fixture_relay "$RECEIVER_BROADCAST" "$PLAYER_ENCODED_VIDEO_TRACK" \
        "$OBJECT_COUNT" "$ENCODED_TRACK_DIR" 2> "$OUTPUT_DIR/read_track.log"; then
    cat "$OUTPUT_DIR/read_track.log" >&2
    tail -30 "$LOG_FILE" >&2
    fail "the player's encoded track did not read"
fi
cat "$OUTPUT_DIR/read_track.log"
grep -q '"payload":"codec_payload_of_the_hang_legacy_container"' \
        "$ENCODED_TRACK_DIR/objects.jsonl" \
    || fail "tatolab-read-track wrote the player's track as sent, not as its codec payload — the catalog never listed it as a legacy-container video track"

wait_until_the_decoder_has_written "$DECODED_FRAMES_FLOOR" 30 || {
    tail -30 "$RECEIVER_RUN_LOG_FILE" >&2
    tail -30 "$LOG_FILE" >&2
    fail "the receiver's decoder wrote $RECEIVER_DECODED_FRAMES frames, fewer than $DECODED_FRAMES_FLOOR"
}
say "Receiver decoded:  $RECEIVER_DECODED_FRAMES frames"

# ── The audio arm: decoded audio came back ───────────────────────────
# Judged against what was *sent*, not in isolation. A rig whose default capture
# device publishes nothing — no live input, muted, no source — would otherwise
# read as this wheel losing the audio it was handed, which is the one confusion
# an audio arm exists to prevent. So the sender's encoder count is read first:
# silent there means the arm cannot run, and only a decoder that stayed empty
# while the encoder spoke is a failure.
PUBLISHED_AUDIO_PACKETS="$(written_messages_of_the_output_port \
    "$SENDER_STREAM_ID" audio_encoder encoded_audio)" \
    || fail "could not read the audio encoder's written count off the sender's graph"
if [ "$PUBLISHED_AUDIO_PACKETS" -eq 0 ]; then
    AUDIO_VERDICT="cannot run — this rig's capture device published no Opus packets, so nothing was sent to measure coming back"
else
    RECEIVED_AUDIO_BLOCKS="$(written_messages_of_the_output_port \
        "$RECEIVER_STREAM_ID" audio_decoder audio)" \
        || fail "could not read the audio decoder's written count off the receiver's graph"
    if [ "$RECEIVED_AUDIO_BLOCKS" -gt 0 ]; then
        AUDIO_VERDICT="pass"
    else
        AUDIO_VERDICT="fail"
    fi
fi
say "Audio decoded:     $AUDIO_VERDICT (${RECEIVED_AUDIO_BLOCKS:-0} blocks off $PUBLISHED_AUDIO_PACKETS packets sent)"

remove_the_fixture_projects
[ "$FIXTURE_PROJECT_REMOVAL_TIMED_OUT" -eq 0 ] \
    || fail "a stream was still loaded 10 s after its project's removal; a teardown that hangs is a finding, not a slow unload"
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
DECODED_PNG_COUNT="$(decode_the_encoded_track_dump_to_pngs "$ENCODED_TRACK_DIR" h264 \
        "$OUTPUT_DIR/encoded.h264" "$MEASURED_DIR" "$OUTPUT_DIR/ffmpeg.log")" \
    || { cat "$OUTPUT_DIR/ffmpeg.log" >&2; fail "ffmpeg could not decode the player's track"; }
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
        --baseline-note "Measured from the receiver's WhepPlayer encoded track read at the rig's relay with tatolab-read-track and decoded by ffmpeg to RGB PNGs."
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

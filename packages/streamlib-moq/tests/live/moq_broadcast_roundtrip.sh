#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# The MoQ live proof: the vivid camera and the microphone out to a draft-16
# relay, the same broadcast pulled back down, and the decoded video scored
# against the baseline the codec rig captured.
#
# The lock is the decode-back, the argument `Mp4Sink` made. A liveness check
# proves a socket opened; a channel mean matched to the vivid baseline within
# ±0.05 proves the pixels that came back are the pixels that went out. The
# network sits inside a path the codec rig already scored, so a mismatch here is
# this wheel's.
#
# Three arms, each reported separately:
#   video   the decode-back, `cargo xtask psnr channel-means` against
#           `psnr_vivid_baseline.tsv`. This is the gate.
#   audio   the block-level channel contract on the decoded audio port, over
#           the real path — cadence, timestamp continuity, rate, channels,
#           dtype. It is the microphone and not the engine's known-signal
#           fixture because that fixture publishes once over 3.78 s and stops,
#           which is shorter than a relay connect: the signal would be over
#           before the far side subscribed. Scoring signal identity across a
#           network hop wants a looping source the engine does not have, and is
#           `/verify-audio`'s shape rather than this one's.
#   interop the CMAF proof (owner, 2026-09-05): `moq-sub`, built from
#           `cloudflare/moq-rs`, reads the same broadcast. A third-party client
#           parsing the catalog, accepting the init segment and decoding the
#           media is stronger than matching a captured reference in-repo — so
#           the arm asks for all three. `--catalog` is passed, or moq-sub never
#           fetches `.catalog` at all and falls back to the hardcoded track
#           names; and the verdict reads the fragment count out of the
#           inspector, because a capture carrying only `ftyp` + `moov` parses
#           perfectly and proves no media moved.
#
# CREDENTIALS. The relay URL is itself a credential — a draft-16 relay is
# provisioned per account and carries its token in the URL path — so it is read
# from the environment, passed to the node through the environment (never argv,
# which `/proc` publishes), and never printed, logged, or written into the
# output directory. `streamlib graph` renders every processor's config, so this
# script reads the graph in a pipe and never persists it.
#
# One exception, stated because it cannot be avoided: `moq-sub` takes its URL as
# a positional argument and reads no environment variable, so the *subscribe*
# token is in that process's `/proc/<pid>/cmdline` for the arm's 25 seconds. It
# is the subscribe-only token, not the publish one, and its stderr is scrubbed
# of the URL before the log is kept.
#
# Absent credentials are a cannot-run (exit 77) and never a pass — the interop
# arm's included. Only `SKIP_INTEROP=1` or a genuinely missing `moq-sub` binary
# downgrades that arm to a report.
#
# Usage:
#   packages/streamlib-moq/tests/live/moq_broadcast_roundtrip.sh [output_dir]
#
# Environment:
#   STREAMLIB_MOQ_RELAY_URL   the relay, token in the path. Falls back to
#                             CLOUDFLARE_MOQ_DRAFT_16_URL + the publish token,
#                             sourced from the repo-root `.env` when present.
#   STREAMLIB_MOQ_SUB_URL     what `moq-sub` dials for the interop arm. Falls
#                             back to the relay host + CLOUDFLARE_MOQ_SUB_TOKEN.
#   STREAMLIB_MOQ_BROADCAST   the broadcast both halves name (default below)
#   SAMPLE_COUNT/SAMPLE_EVERY the exchange budget (defaults 6 / 2)
#   CONTROL_PLANE_PORT        default 9412
#   RUN_SECONDS               node budget (default 120)
#   MEDIA_DEADLINE_SECONDS    how long to wait for the first decoded frame
#                             after the graph is up (default 45) — a relay
#                             connect and a CMAF init handshake sit inside it
#   TOLERANCE                 abs channel-mean drift bound (default 0.05)
#   VIVID_TEST_PATTERN        vivid pattern index (default 7 = "100% Red")
#   SKIP_INTEROP              set to 1 to leave the moq-sub arm out
#
# Exit codes: 0 = pass, 1 = fail, 77 = cannot run.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
REPO_ROOT="$(cd "$PACKAGE_DIR/../.." && pwd)"
ENGINE_FIXTURES="$REPO_ROOT/runtime/streamlib-engine/tests/fixtures"
BASELINE_TSV="$ENGINE_FIXTURES/psnr_vivid_baseline.tsv"

OUTPUT_DIR="${1:-/tmp/streamlib-moq-live-$(date +%s)}"
SAMPLE_COUNT="${SAMPLE_COUNT:-6}"
SAMPLE_EVERY="${SAMPLE_EVERY:-2}"
CONTROL_PLANE_PORT="${CONTROL_PLANE_PORT:-9412}"
RUN_SECONDS="${RUN_SECONDS:-120}"
MEDIA_DEADLINE_SECONDS="${MEDIA_DEADLINE_SECONDS:-45}"
TOLERANCE="${TOLERANCE:-0.05}"
VIVID_TEST_PATTERN="${VIVID_TEST_PATTERN:-7}"
BROADCAST="${STREAMLIB_MOQ_BROADCAST:-streamlib/moq-live-proof}"
SKIP_INTEROP="${SKIP_INTEROP:-}"

say() { echo "[moq-live] $*"; }
cannot_run() { echo "[moq-live] SKIP: $*" >&2; exit 77; }
fail() { echo "[moq-live] FAIL: $*" >&2; exit 1; }

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
# Refused by name instead.
if ! IMPORT_FAILURE="$("$VENV_PYTHON" -c '
import streamlib
from streamlib_moq import MoqBroadcastPublisher, MoqBroadcastSubscriber
_ = (streamlib.H264Decoder, MoqBroadcastPublisher, MoqBroadcastSubscriber)
' 2>&1)"; then
    say "$IMPORT_FAILURE" >&2
    cannot_run "the venv cannot import this wheel beside the engine. Rebuild with \`maturin develop\` — this arm measures the extension, not the tree."
fi

# ── The credentials, from the environment and never from the tree ────
# `.env` is gitignored and is where the rig keeps them. Sourced only when a
# `STREAMLIB_` name is missing, and `set -a` then exports everything it holds —
# so a `CLOUDFLARE_` value it carries can replace one already in the
# environment. Export the `STREAMLIB_` names to pin a run to exactly what you
# meant; those are read first and are never re-derived.
if { [ -z "${STREAMLIB_MOQ_RELAY_URL:-}" ] || [ -z "${STREAMLIB_MOQ_SUB_URL:-}" ]; } \
    && [ -f "$REPO_ROOT/.env" ]; then
    set -a
    # shellcheck disable=SC1091
    . "$REPO_ROOT/.env" >/dev/null 2>&1 || true
    set +a
fi
if [ -z "${STREAMLIB_MOQ_RELAY_URL:-}" ] \
    && [ -n "${CLOUDFLARE_MOQ_DRAFT_16_URL:-}" ] \
    && [ -n "${CLOUDFLARE_MOQ_PUB_SUB_TOKEN:-}" ]; then
    STREAMLIB_MOQ_RELAY_URL="https://${CLOUDFLARE_MOQ_DRAFT_16_URL#https://}/${CLOUDFLARE_MOQ_PUB_SUB_TOKEN}"
fi
[ -n "${STREAMLIB_MOQ_RELAY_URL:-}" ] || cannot_run \
    "no relay credential. Export STREAMLIB_MOQ_RELAY_URL, or put CLOUDFLARE_MOQ_DRAFT_16_URL and CLOUDFLARE_MOQ_PUB_SUB_TOKEN in the repo-root .env. Absent credentials are a cannot-run, never a pass."
export STREAMLIB_MOQ_RELAY_URL

if [ -z "${STREAMLIB_MOQ_SUB_URL:-}" ] \
    && [ -n "${CLOUDFLARE_MOQ_DRAFT_16_URL:-}" ] \
    && [ -n "${CLOUDFLARE_MOQ_SUB_TOKEN:-}" ]; then
    STREAMLIB_MOQ_SUB_URL="https://${CLOUDFLARE_MOQ_DRAFT_16_URL#https://}/${CLOUDFLARE_MOQ_SUB_TOKEN}"
fi
# The interop arm is the owner's CMAF proof, not a bonus: a run that cannot
# reach it has not verified what it reports, so an absent subscribe credential
# is a cannot-run like any other. `SKIP_INTEROP=1` is the way to ask for the
# video and audio arms alone.
if [ "$SKIP_INTEROP" != "1" ] && [ -z "${STREAMLIB_MOQ_SUB_URL:-}" ]; then
    cannot_run "no subscribe credential for the CMAF interop arm. Export STREAMLIB_MOQ_SUB_URL, or put CLOUDFLARE_MOQ_SUB_TOKEN in the repo-root .env. Pass SKIP_INTEROP=1 to run the video and audio arms without it."
fi

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
        # node would hide a hung teardown.
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
say "Container:         cmaf"
say "Broadcast:         $BROADCAST"
say "Control plane:     $CONTROL_PLANE_URL"
say "Relay:             <redacted — the URL carries the account's token>"

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
say "Publishing and subscribing through the relay..."
DISPLAY="${DISPLAY:-:0}" \
RUST_LOG="${RUST_LOG:-warn,streamlib=info,streamlib_media_builtins=info}" \
    timeout --kill-after=5 "$RUN_SECONDS" \
        "$VENV_PYTHON" "$SCRIPT_DIR/moq_broadcast_roundtrip_node.py" \
            --camera "$VIVID_DEVICE" \
            ${AUDIO_CAPTURE_DEVICE:+--audio-capture-device "$AUDIO_CAPTURE_DEVICE"} \
            --broadcast "$BROADCAST" \
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
# and this graph's config holds the relay token.
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

# The relay connect, the CMAF init handshake and the first IDR all sit between
# the graph coming up and the first decoded frame. Waiting for one bag before
# spending the exchange budget is what keeps a slow connect from reading as an
# empty channel — `exchange` gives up after 8 tap rounds.
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
    fail "no frame reached the decoder within ${MEDIA_DEADLINE_SECONDS}s. The relay never delivered, or the subscriber is asking for a track name nothing publishes."
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

# ── The interop arm: a third-party client reads the same broadcast ───
INTEROP_VERDICT="skipped"
if [ "$SKIP_INTEROP" != "1" ]; then
    if ! command -v moq-sub >/dev/null; then
        INTEROP_VERDICT="cannot run — moq-sub is not on PATH (cargo install --git https://github.com/cloudflare/moq-rs moq-sub)"
    elif [ -z "${STREAMLIB_MOQ_SUB_URL:-}" ]; then
        INTEROP_VERDICT="cannot run — no subscribe credential (STREAMLIB_MOQ_SUB_URL or CLOUDFLARE_MOQ_SUB_TOKEN)"
    else
        # `--name` is the broadcast, not a track. `--catalog` is what makes this
        # the catalog proof: without it moq-sub never asks for `.catalog` and
        # falls straight to its hardcoded `0.mp4` / `{track_id}.m4s` names, so
        # the whole catalog writer could be reverted and the arm would still be
        # green. It writes one fMP4 stream to stdout.
        timeout 25 moq-sub --catalog --name "$BROADCAST" "$STREAMLIB_MOQ_SUB_URL" \
            > "$OUTPUT_DIR/moq_sub_output.mp4" 2> "$OUTPUT_DIR/moq_sub_raw.log" || true
        # The URL is a credential and this is a third-party binary's stderr, so
        # it is scrubbed before the log is kept rather than trusted not to echo.
        sed "s|$STREAMLIB_MOQ_SUB_URL|<redacted subscribe url>|g" \
            "$OUTPUT_DIR/moq_sub_raw.log" > "$OUTPUT_DIR/moq_sub.log" 2>/dev/null || true
        rm -f "$OUTPUT_DIR/moq_sub_raw.log"
        interop_bytes="$(stat -c %s "$OUTPUT_DIR/moq_sub_output.mp4" 2>/dev/null || echo 0)"
        if [ "$interop_bytes" -gt 0 ] \
            && "$REPO_ROOT/target/release/xtask" mp4-inspect "$OUTPUT_DIR/moq_sub_output.mp4" \
                > "$OUTPUT_DIR/moq_sub_inspect.json" 2>/dev/null; then
            # `mp4-inspect` bails only on a missing `moov`, so a capture holding
            # the init segment and nothing else parses cleanly. The fragment
            # count is what says media actually arrived and decoded.
            INTEROP_FRAGMENTS="$(python3 -c '
import json, sys
try:
    print(len(json.load(open(sys.argv[1])).get("fragments", [])))
except Exception:
    print(0)
' "$OUTPUT_DIR/moq_sub_inspect.json")"
            if [ "${INTEROP_FRAGMENTS:-0}" -gt 0 ] 2>/dev/null; then
                INTEROP_VERDICT="pass — moq-sub fetched the catalog, accepted the init segment and decoded $INTEROP_FRAGMENTS fragments ($interop_bytes bytes)"
            else
                INTEROP_VERDICT="fail — moq-sub read $interop_bytes bytes but the capture carries no media fragment, so only the init segment arrived"
            fi
        else
            INTEROP_VERDICT="fail — moq-sub produced $interop_bytes bytes that no parser could read; see moq_sub.log"
        fi
    fi
fi
say "CMAF interop:      $INTEROP_VERDICT"

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
    say "RESULT: video PASS · audio $AUDIO_VERDICT · interop $INTEROP_VERDICT"
    # A `cannot run` interop verdict is an absent third-party tool and stays a
    # report; a `fail` one is `moq-sub` refusing a broadcast this wheel wrote,
    # which is the interop claim itself failing.
    case "$AUDIO_VERDICT$INTEROP_VERDICT" in
        *fail*) exit 1 ;;
    esac
    exit 0
fi
say "Output dir:        $OUTPUT_DIR"
say "RESULT: FAIL — the frames that came back off the relay drift from the baseline"
exit 1

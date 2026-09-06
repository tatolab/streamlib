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
# Two containers, run in turn as two nodes: `cmaf`, which `moq-sub` reads, and
# `streamlib_bag`, which carries a data track beside the media and names its
# tracks rather than numbering them. Each is a separate node on its own port
# and its own broadcast, written into its own subdirectory of the output.
#
# Four arms, each reported separately per container:
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
#   data    `streamlib_bag` only: the telemetry bags the publisher classified
#           as data, read back off the subscriber's `data_bags` and compared
#           with what was sent. Every bag says which frame it is, and both its
#           `blob` and its `stamp_ns` are derived from that — so each bag
#           carries its own expected value across the network and the
#           comparison is exact rather than tolerant. The stamp is read twice,
#           from the payload and from the transport frame's header, and they
#           agree only if the producer's instant survived the round trip
#           untouched.
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
#   STREAMLIB_MOQ_BROADCAST   the broadcast both halves name (default below).
#                             `cmaf` keeps it bare, so an interop run is named
#                             exactly what it always was; every other container
#                             appends its own name, so no two arms share one
#                             broadcast whatever order they run in
#   CONTAINER_FORMATS         which arms to run, space separated (default
#                             "cmaf streamlib_bag")
#   SAMPLE_COUNT/SAMPLE_EVERY the exchange budget (defaults 6 / 2)
#   DATA_TAP_ROUNDS           taps the data arm merges (default 3)
#   DATA_BAG_COUNT            bags asked for per round (default 8)
#   MINIMUM_DATA_BAGS         fewer than this back is a failure (default 3)
#   CONTROL_PLANE_PORT        the first arm's port; each later arm takes the
#                             next one up (default 9412)
#   RUN_SECONDS               node budget (default 120)
#   MEDIA_DEADLINE_SECONDS    how long to wait for the first decoded frame
#                             after the graph is up (default 45) — a relay
#                             connect and a CMAF init handshake sit inside it
#   TOLERANCE                 abs channel-mean drift bound (default 0.05)
#   VIVID_TEST_PATTERN        vivid pattern index (default 7 = "100% Red")
#   DELIVERY_DEADLINE_MS      the publisher's delivery deadline in ms. Unset is
#                             the baseline arm — every bag is published however
#                             late it is. Set it for the policy-on arm; the
#                             publisher's own count of what it shed is in the
#                             log, per inbound link, and a run that shed
#                             nothing says so
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
CONTAINER_FORMATS="${CONTAINER_FORMATS:-cmaf streamlib_bag}"
SAMPLE_COUNT="${SAMPLE_COUNT:-6}"
SAMPLE_EVERY="${SAMPLE_EVERY:-2}"
DATA_TAP_ROUNDS="${DATA_TAP_ROUNDS:-3}"
DATA_BAG_COUNT="${DATA_BAG_COUNT:-8}"
# A floor no healthy run misses rather than a target: what the data arm gates
# on is every bag matching, and one tap collects over a window of about half a
# second, so a count large enough to be interesting would make the floor itself
# the flake.
MINIMUM_DATA_BAGS="${MINIMUM_DATA_BAGS:-3}"
CONTROL_PLANE_PORT="${CONTROL_PLANE_PORT:-9412}"
RUN_SECONDS="${RUN_SECONDS:-120}"
MEDIA_DEADLINE_SECONDS="${MEDIA_DEADLINE_SECONDS:-45}"
TOLERANCE="${TOLERANCE:-0.05}"
VIVID_TEST_PATTERN="${VIVID_TEST_PATTERN:-7}"
BROADCAST="${STREAMLIB_MOQ_BROADCAST:-streamlib/moq-live-proof}"
DELIVERY_DEADLINE_MS="${DELIVERY_DEADLINE_MS:-}"
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
# video and audio arms alone. Asked for only where a `cmaf` arm is actually
# going to run — a `streamlib_bag`-only run has no interop arm to credential.
if [ "$SKIP_INTEROP" != "1" ] && [ -z "${STREAMLIB_MOQ_SUB_URL:-}" ] \
    && [[ " $CONTAINER_FORMATS " == *" cmaf "* ]]; then
    cannot_run "no subscribe credential for the CMAF interop arm. Export STREAMLIB_MOQ_SUB_URL, or put CLOUDFLARE_MOQ_SUB_TOKEN in the repo-root .env. Pass SKIP_INTEROP=1 to run the video and audio arms without it."
fi

# ── The camera ───────────────────────────────────────────────────────
# `/proc/modules` rather than `lsmod | grep -q`: under `pipefail`, `grep -q`
# closing the pipe on its first match kills `lsmod` with SIGPIPE and the
# pipeline reads as failed — a race that reports a loaded module as absent.
grep -q '^vivid ' /proc/modules || sudo modprobe vivid 2>/dev/null \
    || cannot_run "vivid module not available (check kernel config)"
VIVID_DEVICE=""
while read -r dev; do
    if v4l2-ctl -d "$dev" --info 2>/dev/null | grep -q "Video Capture"; then
        VIVID_DEVICE="$dev"
        break
    fi
done < <(v4l2-ctl --list-devices 2>/dev/null | awk '/vivid/{getline; print $1}')
[ -n "$VIVID_DEVICE" ] || cannot_run "no vivid capture device found"

# ── What every arm shares ────────────────────────────────────────────
mkdir -p "$OUTPUT_DIR"

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
say "Containers:        $CONTAINER_FORMATS"
say "Broadcast:         $BROADCAST"
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

# ── One arm: one container format, launched to verdict ───────────────
# The two containers are two runs of one graph rather than two graphs. Each
# takes its own control port, its own broadcast name and its own output
# directory, and the node is stopped before the next arm starts: two nodes on
# one port would let a run measure the other one's.
run_one_container_format() {
    local container_format="$1"
    local control_plane_port="$2"
    local broadcast="$3"
    local arm_dir="$OUTPUT_DIR/$container_format"
    local exchanged_dir="$arm_dir/exchanged"
    local measured_dir="$arm_dir/measured"

    CONTROL_PLANE_URL="http://127.0.0.1:$control_plane_port"
    LOG_FILE="$arm_dir/pipeline.log"
    mkdir -p "$arm_dir"

    ARM_VIDEO_VERDICT="fail — the arm did not reach its measurement"
    ARM_AUDIO_VERDICT="skipped"
    ARM_INTEROP_VERDICT="skipped"
    ARM_DATA_VERDICT="n/a — this container carries no data track"

    echo ""
    say "── $container_format ──────────────────────────────────────────"
    say "Broadcast:         $broadcast"
    say "Control plane:     $CONTROL_PLANE_URL"

    # A busy control port would misdirect this run rather than fail it: the API
    # server walks up to ten ports when the one it was given is taken, so a
    # second node already on this one would be measured instead.
    if (echo >"/dev/tcp/127.0.0.1/$control_plane_port") 2>/dev/null; then
        fail "something is already listening on 127.0.0.1:$control_plane_port; this run would measure that node instead of its own"
    fi

    # ── Run ──────────────────────────────────────────────────────────
    say "Publishing and subscribing through the relay..."
    DISPLAY="${DISPLAY:-:0}" \
    RUST_LOG="${RUST_LOG:-warn,streamlib=info,streamlib_media_builtins=info}" \
        timeout --kill-after=5 "$RUN_SECONDS" \
            "$VENV_PYTHON" "$SCRIPT_DIR/moq_broadcast_roundtrip_node.py" \
                --camera "$VIVID_DEVICE" \
                ${AUDIO_CAPTURE_DEVICE:+--audio-capture-device "$AUDIO_CAPTURE_DEVICE"} \
                --broadcast "$broadcast" \
                --container-format "$container_format" \
                --control-plane-port "$control_plane_port" \
                ${DELIVERY_DEADLINE_MS:+--delivery-deadline-ms "$DELIVERY_DEADLINE_MS"} \
            > "$LOG_FILE" 2>&1 &
    NODE_PID=$!

    for _ in $(seq 1 120); do
        "$STREAMLIB_CLI" graph --url "$CONTROL_PLANE_URL" >/dev/null 2>&1 && break
        sleep 0.5
    done

    local decoded_video_channel decoded_audio_channel
    decoded_video_channel="$(channel_of video_decoder video)" \
        || { tail -40 "$LOG_FILE" >&2; fail "could not read the video decoder's channel off the live graph"; }
    decoded_audio_channel="$(channel_of audio_decoder audio)" \
        || { tail -40 "$LOG_FILE" >&2; fail "could not read the audio decoder's channel off the live graph"; }
    say "Decoded video:     $decoded_video_channel"
    say "Decoded audio:     $decoded_audio_channel"

    # The relay connect, the CMAF init handshake and the first IDR all sit
    # between the graph coming up and the first decoded frame. Waiting for one
    # bag before spending the exchange budget is what keeps a slow connect from
    # reading as an empty channel — `exchange` gives up after 8 tap rounds.
    say "Waiting for the first decoded frame (deadline ${MEDIA_DEADLINE_SECONDS}s)..."
    local first_frame_seen=0
    for _ in $(seq 1 "$MEDIA_DEADLINE_SECONDS"); do
        if [ "$(tapped_bag_count "$decoded_video_channel")" -gt 0 ] 2>/dev/null; then
            first_frame_seen=1
            break
        fi
        kill -0 "$NODE_PID" 2>/dev/null || break
        sleep 1
    done
    if [ "$first_frame_seen" -ne 1 ]; then
        tail -60 "$LOG_FILE" >&2
        fail "no frame reached the decoder within ${MEDIA_DEADLINE_SECONDS}s. The relay never delivered, or the subscriber is asking for a track name nothing publishes."
    fi

    # ── The video arm: the decode-back ───────────────────────────────
    if ! "$STREAMLIB_CLI" exchange \
            --channel "$decoded_video_channel" \
            --out "$exchanged_dir" \
            --count "$SAMPLE_COUNT" \
            --every "$SAMPLE_EVERY" \
            --url "$CONTROL_PLANE_URL" \
            > "$arm_dir/exchanged_paths.txt" 2> "$arm_dir/exchange.log"; then
        cat "$arm_dir/exchange.log" >&2
        tail -40 "$LOG_FILE" >&2
        fail "exchanged fewer frames than asked for"
    fi
    cat "$arm_dir/exchange.log"

    # Read the printed paths, not the directory: --out is not cleared, so a
    # listing can hand back an earlier run's frames.
    mkdir -p "$measured_dir"
    local sample_index=0
    while read -r exchanged_png; do
        [ -f "$exchanged_png" ] || continue
        cp "$exchanged_png" "$measured_dir/$(printf "%04d" "$sample_index").png"
        sample_index=$(( sample_index + 1 ))
    done < "$arm_dir/exchanged_paths.txt"
    [ "$sample_index" -eq "$SAMPLE_COUNT" ] \
        || fail "copied $sample_index of $SAMPLE_COUNT exchanged frames; a drift lock measured on fewer samples than the run asked for reports a thinner gate as a full one"
    say "Captured $sample_index frames"

    # ── The audio arm: the block contract on what came back ──────────
    # Judged against what was *sent*, not in isolation. A rig whose default
    # capture device publishes nothing — no live input, muted, no source —
    # would otherwise read as this wheel losing the audio it was handed, which
    # is the one confusion an audio arm exists to prevent. So the encoder's own
    # output is tapped first: silent there means the arm cannot run, and only a
    # decoder that stayed empty while the encoder spoke is a failure.
    local encoded_audio_channel published_audio_bags
    encoded_audio_channel="$(channel_of audio_encoder encoded_audio)" || encoded_audio_channel=""
    published_audio_bags=0
    if [ -n "$encoded_audio_channel" ]; then
        published_audio_bags="$(tapped_bag_count "$encoded_audio_channel")"
    fi
    if [ "${published_audio_bags:-0}" -eq 0 ] 2>/dev/null; then
        ARM_AUDIO_VERDICT="cannot run — this rig's capture device published no Opus packets, so nothing was sent to measure coming back"
    elif PYTHON="$VENV_PYTHON" "$ENGINE_FIXTURES/verify_audio_channel.sh" audio_decoder \
            --url "$CONTROL_PLANE_URL" --port audio --count 8 \
            > "$arm_dir/audio_channel.json" 2> "$arm_dir/audio_channel.log"; then
        ARM_AUDIO_VERDICT="pass"
    else
        ARM_AUDIO_VERDICT="fail"
    fi
    say "Audio channel:     $ARM_AUDIO_VERDICT ($arm_dir/audio_channel.json)"

    run_the_data_arm "$container_format" "$arm_dir"
    run_the_interop_arm "$container_format" "$arm_dir" "$broadcast"

    stop_node
    [ "$NODE_NEEDED_SIGKILL" -eq 0 ] \
        || fail "the node did not exit on SIGTERM and needed SIGKILL; a teardown that hangs is a finding, not a slow exit"

    report_what_the_data_track_accounted_for "$container_format" "$arm_dir"

    # ── Measure ──────────────────────────────────────────────────────
    echo ""
    say "Log gates:"
    for pattern in OUT_OF_DEVICE_MEMORY DEVICE_LOST "process() failed" "Validation Error"; do
        printf '  %-24s %s\n' "$pattern" "$(grep -cF "$pattern" "$LOG_FILE" 2>/dev/null; true)"
    done
    echo ""

    if "$REPO_ROOT/target/release/xtask" psnr channel-means \
            --images "$measured_dir" \
            --baseline "$BASELINE_TSV" \
            --tolerance "$TOLERANCE" \
            --report "$arm_dir/channel_means.tsv"; then
        ARM_VIDEO_VERDICT="pass"
        say "Per-sample stats:  $arm_dir/channel_means.tsv"
    else
        ARM_VIDEO_VERDICT="fail — the frames that came back off the relay drift from the baseline"
    fi
    say "Video decode-back: $ARM_VIDEO_VERDICT"
}

# ── The data arm: what came back is what went out ────────────────────
# The media arms lock on a channel mean; a data track has no such statistic and
# needs none. Every telemetry bag says which frame it is, and its `blob` and
# its `stamp_ns` are both derived from that — so the bag carries its own
# expected value across the network and the comparison is exact rather than
# tolerant. Several tap rounds because one collects over about half a second,
# and a sample that narrow could miss an intermittent corruption entirely.
run_the_data_arm() {
    local container_format="$1"
    local arm_dir="$2"
    [ "$container_format" = "streamlib_bag" ] || return 0

    local data_channel
    data_channel="$(channel_of subscriber data_bags)" || data_channel=""
    if [ -z "$data_channel" ]; then
        ARM_DATA_VERDICT="fail — the running graph has no subscriber data_bags channel"
        say "Data track:        $ARM_DATA_VERDICT"
        return 0
    fi
    say "Data bags:         $data_channel"

    # The relay's data subscription settles on its own schedule, so the first
    # bag is waited for the way the first frame is rather than assumed to have
    # arrived with the video.
    local first_bag_seen=0
    for _ in $(seq 1 "$MEDIA_DEADLINE_SECONDS"); do
        if [ "$(tapped_bag_count "$data_channel")" -gt 0 ] 2>/dev/null; then
            first_bag_seen=1
            break
        fi
        kill -0 "$NODE_PID" 2>/dev/null || break
        sleep 1
    done
    if [ "$first_bag_seen" -ne 1 ]; then
        ARM_DATA_VERDICT="fail — no data bag reached the subscriber within ${MEDIA_DEADLINE_SECONDS}s"
        say "Data track:        $ARM_DATA_VERDICT"
        return 0
    fi

    local round tapped_files=()
    for round in $(seq 1 "$DATA_TAP_ROUNDS"); do
        local tapped_json="$arm_dir/tapped_data_bags_$round.json"
        if "$STREAMLIB_CLI" tap "$data_channel" --count "$DATA_BAG_COUNT" \
                --url "$CONTROL_PLANE_URL" > "$tapped_json" \
                2>> "$arm_dir/tap_data_bags.log"; then
            tapped_files+=("$tapped_json")
        fi
    done
    if [ "${#tapped_files[@]}" -eq 0 ]; then
        ARM_DATA_VERDICT="fail — every tap on $data_channel failed; see $arm_dir/tap_data_bags.log"
        say "Data track:        $ARM_DATA_VERDICT"
        return 0
    fi

    if "$VENV_PYTHON" "$SCRIPT_DIR/verify_tapped_telemetry_bags.py" \
            "${tapped_files[@]}" --minimum-bags "$MINIMUM_DATA_BAGS" \
            > "$arm_dir/data_bags.json" 2> "$arm_dir/data_bags.log"; then
        ARM_DATA_VERDICT="pass — $(data_bag_tally "$arm_dir/data_bags.json") bags came back byte-for-byte and stamped as sent"
    else
        ARM_DATA_VERDICT="fail — $(data_bag_tally "$arm_dir/data_bags.json") matched; see $arm_dir/data_bags.json"
    fi
    say "Data track:        $ARM_DATA_VERDICT"
}

# The subscriber counts what the relay never delivered and says so at its
# progress cadence and again at teardown. Read after the node has stopped
# because a run shorter than one cadence has only the teardown line, and that
# line is the one that always fires. A gap is not a failure — it is loss the
# wheel saw and accounted for — but a run reporting none is a different claim
# from one reporting some, so it is kept rather than summarised away.
report_what_the_data_track_accounted_for() {
    local container_format="$1"
    local arm_dir="$2"
    [ "$container_format" = "streamlib_bag" ] || return 0

    grep -F "sequence_gaps" "$LOG_FILE" | tail -1 \
        > "$arm_dir/data_bags_accounting.log" || true
    [ -s "$arm_dir/data_bags_accounting.log" ] || return 0
    say "Data accounting:   $(sed 's/.*MoqBroadcastSubscriber: //' "$arm_dir/data_bags_accounting.log")"
}

data_bag_tally() {
    python3 -c '
import json, sys
try:
    report = json.load(open(sys.argv[1]))
except Exception:
    print("no")
    sys.exit(0)
print("%d of %d" % (report["bags_matching_what_was_sent"], report["bags_compared"]))
' "$1"
}

# ── The interop arm: a third-party client reads the same broadcast ───
# CMAF only, and not because `streamlib_bag` is unproven: `moq-sub` decodes an
# fMP4 stream, and a bag broadcast is not one. What proves that container is
# the data arm above and this wheel's own subscriber beside it.
run_the_interop_arm() {
    local container_format="$1"
    local arm_dir="$2"
    local broadcast="$3"
    if [ "$container_format" != "cmaf" ]; then
        ARM_INTEROP_VERDICT="n/a — moq-sub reads fMP4, which this container is not"
        say "CMAF interop:      $ARM_INTEROP_VERDICT"
        return 0
    fi
    if [ "$SKIP_INTEROP" = "1" ]; then
        ARM_INTEROP_VERDICT="skipped"
        say "CMAF interop:      $ARM_INTEROP_VERDICT"
        return 0
    fi
    if ! command -v moq-sub >/dev/null; then
        ARM_INTEROP_VERDICT="cannot run — moq-sub is not on PATH (cargo install --git https://github.com/cloudflare/moq-rs moq-sub)"
        say "CMAF interop:      $ARM_INTEROP_VERDICT"
        return 0
    fi
    if [ -z "${STREAMLIB_MOQ_SUB_URL:-}" ]; then
        ARM_INTEROP_VERDICT="cannot run — no subscribe credential (STREAMLIB_MOQ_SUB_URL or CLOUDFLARE_MOQ_SUB_TOKEN)"
        say "CMAF interop:      $ARM_INTEROP_VERDICT"
        return 0
    fi

    # `--name` is the broadcast, not a track. `--catalog` is what makes this
    # the catalog proof: without it moq-sub never asks for `.catalog` and falls
    # straight to its hardcoded `0.mp4` / `{track_id}.m4s` names, so the whole
    # catalog writer could be reverted and the arm would still be green. It
    # writes one fMP4 stream to stdout.
    timeout 25 moq-sub --catalog --name "$broadcast" "$STREAMLIB_MOQ_SUB_URL" \
        > "$arm_dir/moq_sub_output.mp4" 2> "$arm_dir/moq_sub_raw.log" || true
    # The URL is a credential and this is a third-party binary's stderr, so it
    # is scrubbed before the log is kept rather than trusted not to echo. A
    # literal replacement, not `sed`: a URL is not a regular expression, and one
    # with an IPv6 literal host (`https://[::1]/<token>`) reads as a bracket
    # expression that matches something else entirely — leaving the token in the
    # log while the scrub reports success.
    REDACT_SUBSCRIBE_URL="$STREAMLIB_MOQ_SUB_URL" python3 -c '
import os, sys

secret = os.environ["REDACT_SUBSCRIBE_URL"]
with open(sys.argv[1], "rb") as raw:
    body = raw.read()
with open(sys.argv[2], "wb") as scrubbed:
    scrubbed.write(body.replace(secret.encode(), b"<redacted subscribe url>"))
' "$arm_dir/moq_sub_raw.log" "$arm_dir/moq_sub.log" 2>/dev/null || true
    rm -f "$arm_dir/moq_sub_raw.log"

    local interop_bytes interop_fragments
    interop_bytes="$(stat -c %s "$arm_dir/moq_sub_output.mp4" 2>/dev/null || echo 0)"
    if [ "$interop_bytes" -gt 0 ] \
        && "$REPO_ROOT/target/release/xtask" mp4-inspect "$arm_dir/moq_sub_output.mp4" \
            > "$arm_dir/moq_sub_inspect.json" 2>/dev/null; then
        # `mp4-inspect` bails only on a missing `moov`, so a capture holding the
        # init segment and nothing else parses cleanly. The fragment count is
        # what says media actually arrived and decoded.
        interop_fragments="$(python3 -c '
import json, sys
try:
    print(len(json.load(open(sys.argv[1])).get("fragments", [])))
except Exception:
    print(0)
' "$arm_dir/moq_sub_inspect.json")"
        if [ "${interop_fragments:-0}" -gt 0 ] 2>/dev/null; then
            ARM_INTEROP_VERDICT="pass — moq-sub fetched the catalog, accepted the init segment and decoded $interop_fragments fragments ($interop_bytes bytes)"
        else
            ARM_INTEROP_VERDICT="fail — moq-sub read $interop_bytes bytes but the capture carries no media fragment, so only the init segment arrived"
        fi
    else
        ARM_INTEROP_VERDICT="fail — moq-sub produced $interop_bytes bytes that no parser could read; see $arm_dir/moq_sub.log"
    fi
    say "CMAF interop:      $ARM_INTEROP_VERDICT"
}

# ── Every arm, then one verdict ──────────────────────────────────────
RESULT_LINES=()
EVERY_VERDICT=""
ARM_PORT="$CONTROL_PLANE_PORT"
for container_format in $CONTAINER_FORMATS; do
    arm_broadcast="$BROADCAST"
    [ "$container_format" = "cmaf" ] || arm_broadcast="$BROADCAST-$container_format"
    run_one_container_format "$container_format" "$ARM_PORT" "$arm_broadcast"
    RESULT_LINES+=(
        "$container_format: video $ARM_VIDEO_VERDICT · audio $ARM_AUDIO_VERDICT · interop $ARM_INTEROP_VERDICT · data $ARM_DATA_VERDICT"
    )
    EVERY_VERDICT="$EVERY_VERDICT$ARM_VIDEO_VERDICT$ARM_AUDIO_VERDICT$ARM_INTEROP_VERDICT$ARM_DATA_VERDICT"
    ARM_PORT=$(( ARM_PORT + 1 ))
done

echo ""
say "Output dir:        $OUTPUT_DIR"
for result_line in "${RESULT_LINES[@]}"; do
    say "RESULT: $result_line"
done

# A `fail` is a wheel that did not deliver; a `cannot run` is an arm that never
# ran. Neither may exit 0, because 0 is read as "everything this run claims to
# prove was proved" — and an arm that did not run proved nothing. 77 keeps the
# distinction: a cannot-run is never a pass. `SKIP_INTEROP=1` is the way to ask
# for the other arms on purpose, and is not this.
case "$EVERY_VERDICT" in
    *fail*) exit 1 ;;
esac
case "$EVERY_VERDICT" in
    *"cannot run"*)
        say "RESULT: cannot run — an arm above never ran, so this run verified less than it reports"
        exit 77
        ;;
esac
exit 0

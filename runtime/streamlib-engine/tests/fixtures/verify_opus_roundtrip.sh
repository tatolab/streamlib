#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# The known signal through Opus encode and decode, scored as audio.
#
# Sister to `verify_audio_loopback.sh`, and the arm that closes the codec rung.
# The loopback proves the transport with a device at each end; this proves the
# codec with no device at all — the whole loop is inside the graph, so a
# failure here with the loopback green is the codec's.
#
# What is scored is lossy by design, so the verdict is the analysis's own:
# tone identity and the DTMF timing grid, which Opus preserves, rather than a
# sample-exact match no codec would give.
#
# Usage:
#   ./verify_opus_roundtrip.sh [--record-seconds SECONDS]
#
# Exit status is the verdict, stdout is the report JSON and nothing else, so a
# caller can pipe it. Progress goes to stderr. The stream runs on the runtime
# unit with `tatolab run` (see fixture_runtime_unit.sh).
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=fixture_runtime_unit.sh
. "$HERE/fixture_runtime_unit.sh"

# The signal is 2.78 s and the source stops publishing at 3.78 s, so the record
# window sits between them: past the source's end nothing further arrives and
# the recorder would never write.
RECORD_SECONDS=3.0
while [ $# -gt 0 ]; do
    case "$1" in
        --record-seconds) RECORD_SECONDS="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

# What this arm can be skipped for. No audio device is in the path, and libopus
# is linked into the engine — what is left is the runtime unit, and the GPU the
# engine's own context needs. A render node that exists but cannot make a
# Vulkan device is an engine-visible failure and gets the failure verdict, not
# a quiet skip.
require_the_runtime_unit
require_the_fixture_venv
if ! compgen -G "/dev/dri/renderD*" >/dev/null; then
    echo "SKIP: no DRM render node, so no GPU-backed runtime can start here" >&2
    exit 77
fi

OUTPUT_DIR="$(mktemp -d -t streamlib-opus-roundtrip-XXXXXX)"
CAPTURED_WAVEFORM="$OUTPUT_DIR/decoded.wav"

NODE_PID=""
# Installed before the node starts and idempotent — `kill` of an unset pid is
# swallowed. A strand here costs a live engine holding a GPU context and an
# iceoryx2 node, which contaminates every later run on the same rig; the
# SIGTERM reaches `tatolab run`, which forwards it to `tatolabd`, and the wait
# is bounded so a stop that hangs cannot hold the script open.
stop_and_wait_for_the_stream() {
    [ -n "$NODE_PID" ] || return 0
    kill "$NODE_PID" 2>/dev/null || return 0
    for _ in $(seq 60); do
        kill -0 "$NODE_PID" 2>/dev/null || break
        sleep 0.5
    done
    wait "$NODE_PID" 2>/dev/null
}
trap stop_and_wait_for_the_stream EXIT
# Without this the shell survives its interrupted children and runs on to the
# analysis, which can report PASS for a run the user aborted.
trap 'exit 130' INT TERM

echo "starting the Opus round-trip stream" >&2
# The recorder runs in its own helper process, and `tatolab run` hands a stream
# no argv, so where it writes and how much it records travel in the environment
# every helper inherits.
STREAMLIB_CAPTURED_WAVEFORM="$CAPTURED_WAVEFORM" \
    STREAMLIB_CAPTURED_WAVEFORM_SECONDS="$RECORD_SECONDS" \
    "$TATOLAB_EXECUTABLE" run --dir "$HERE" opus_roundtrip_stream.py \
    >"$OUTPUT_DIR/node.log" 2>&1 &
NODE_PID=$!

# Matched by the launched pid rather than by name, so another node on the
# machine declaring an OpusDecoder of its own is never the one measured.

# Polled rather than slept: the node has a GPU context and an iceoryx2 node to
# bring up, and a fixed sleep is either flaky or slow.
RUNTIME_ID=""
NODE_ANSWERED=0
for _ in $(seq 60); do
    if ! kill -0 "$NODE_PID" 2>/dev/null; then
        echo "ERROR: the round-trip node exited before serving its local API" >&2
        cat "$OUTPUT_DIR/node.log" >&2
        exit 1
    fi
    if [ -z "$RUNTIME_ID" ]; then
        RUNTIME_ID="$(runtime_id_of_the_node_launched_as "$NODE_PID")" || RUNTIME_ID=""
    fi
    if [ -n "$RUNTIME_ID" ] \
        && tatolab_observation_verb graph --node "$RUNTIME_ID" >/dev/null 2>&1; then
        NODE_ANSWERED=1
        break
    fi
    sleep 0.5
done
if [ "$NODE_ANSWERED" -ne 1 ]; then
    echo "ERROR: the round-trip node never answered over its local API socket" >&2
    tail -40 "$OUTPUT_DIR/node.log" >&2
    exit 1
fi

# A bag either block refused, by name rather than left to show up as silence —
# silence is also what a stalled graph looks like and the two need different
# fixes. The decoder refuses a packet it cannot read; the encoder refuses a
# channel count Opus cannot place.
#
# Two patterns because a refusal reaches the log two ways. A reactive
# processor reports by returning `Err`, which the thread runner renders at
# WARN as `process() failed: <the refusal>` — the decoder's only channel, since
# it calls `tracing::error!` nowhere. The encoder additionally logs its own
# errors directly.
#
# Neither pattern matches the healthy run: both blocks narrate their mint and
# their teardown counts at INFO, and the decoder's gap line is a WARN whose
# message is "a gap in the encoded stream" rather than "process() failed".
OPUS_REFUSALS='process\(\) failed: .*Opus(Encoder|Decoder)|\[ERROR\].*Opus(Encoder|Decoder)'
if grep -qE "$OPUS_REFUSALS" "$OUTPUT_DIR/node.log"; then
    echo "ERROR: an Opus block refused what it was handed — see the reason below" >&2
    grep -E "$OPUS_REFUSALS" "$OUTPUT_DIR/node.log" >&2
    exit 1
fi

# First verdict: the block-level contract on the decoder's own output port —
# cadence and timestamp continuity, read off the wire rather than from the
# recorder that also does the measuring.
if ! "$HERE/verify_audio_channel.sh" opusdecoder \
    --node "$RUNTIME_ID" --count 64 --port audio >&2; then
    echo "ERROR: the decoder's channel failed its block-level contract" >&2
    exit 1
fi

# Second verdict, and the one this fixture exists for: the signal itself.
echo "waiting for the node to write what it decoded" >&2
for _ in $(seq 120); do
    if grep -q "MARKER:WAVEFORM_WRITTEN" "$OUTPUT_DIR/node.log"; then
        break
    fi
    if ! kill -0 "$NODE_PID" 2>/dev/null; then
        echo "ERROR: the round-trip node exited before writing its capture" >&2
        tail -40 "$OUTPUT_DIR/node.log" >&2
        exit 1
    fi
    sleep 0.5
done
if ! [ -s "$CAPTURED_WAVEFORM" ]; then
    echo "ERROR: the node never wrote a waveform to measure" >&2
    tail -40 "$OUTPUT_DIR/node.log" >&2
    exit 1
fi

"$FIXTURE_PYTHON" "$HERE/known_audio_signal.py" analyse \
    "$CAPTURED_WAVEFORM" "$OUTPUT_DIR/spectrogram.png"
VERDICT=$?

echo "artifacts: $OUTPUT_DIR" >&2
exit "$VERDICT"

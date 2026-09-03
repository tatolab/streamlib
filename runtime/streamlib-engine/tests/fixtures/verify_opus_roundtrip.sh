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
#   ./verify_opus_roundtrip.sh [--port PORT] [--record-seconds SECONDS]
#
# Exit status is the verdict, stdout is the report JSON and nothing else, so a
# caller can pipe it. Progress goes to stderr.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PYTHON="${PYTHON:-python3}"

CONTROL_PORT="${CONTROL_PORT:-9078}"
# The signal is 2.7 s and the source stops publishing at 3.7 s, so the record
# window sits between them: past the source's end nothing further arrives and
# the recorder would never write.
RECORD_SECONDS=3.0
while [ $# -gt 0 ]; do
    case "$1" in
        --port) CONTROL_PORT="$2"; shift 2 ;;
        --record-seconds) RECORD_SECONDS="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

# The only thing this arm can be skipped for. No audio device is in the path,
# and libopus is linked into the wheel — what is left is the GPU the engine's
# own context needs. A render node that exists but cannot make a Vulkan device
# is an engine-visible failure and gets the failure verdict, not a quiet skip.
if ! compgen -G "/dev/dri/renderD*" >/dev/null; then
    echo "SKIP: no DRM render node, so no GPU-backed Runtime can start here" >&2
    exit 77
fi

OUTPUT_DIR="$(mktemp -d -t streamlib-opus-roundtrip-XXXXXX)"
CONTROL_URL="http://127.0.0.1:$CONTROL_PORT"
CAPTURED_WAVEFORM="$OUTPUT_DIR/decoded.wav"

NODE_PID=""
# Installed before the node starts and idempotent — `kill` of an unset pid is
# swallowed. A strand here costs a live engine holding a GPU context and an
# iceoryx2 node, which contaminates every later run on the same rig.
trap 'kill "$NODE_PID" 2>/dev/null' EXIT
# Without this the shell survives its interrupted children and runs on to the
# analysis, which can report PASS for a run the user aborted.
trap 'exit 130' INT TERM

echo "starting the Opus round-trip node" >&2
(
    cd "$HERE" || exit 1
    "$PYTHON" opus_roundtrip_node.py "$CAPTURED_WAVEFORM" \
        --control-plane-port "$CONTROL_PORT" \
        --record-seconds "$RECORD_SECONDS"
) >"$OUTPUT_DIR/node.log" 2>&1 &
NODE_PID=$!

# Polled rather than slept: the node has a GPU context and an iceoryx2 node to
# bring up, and a fixed sleep is either flaky or slow.
for _ in $(seq 60); do
    if ! kill -0 "$NODE_PID" 2>/dev/null; then
        echo "ERROR: the round-trip node exited before serving its control plane" >&2
        cat "$OUTPUT_DIR/node.log" >&2
        exit 1
    fi
    if "$PYTHON" -m streamlib.cli graph --url "$CONTROL_URL" >/dev/null 2>&1; then
        break
    fi
    sleep 0.5
done

# A bag either block refused, by name rather than left to show up as silence —
# silence is also what a stalled graph looks like and the two need different
# fixes. The decoder refuses a packet it cannot read; the encoder refuses a
# channel count Opus cannot place.
#
# Gated on the level, because both blocks narrate their healthy mint and their
# teardown counts at INFO and a gap at WARN — a bare name match would call
# every passing run a refusal.
OPUS_REFUSALS='\[ERROR\].*Opus(Encoder|Decoder)'
if grep -qE "$OPUS_REFUSALS" "$OUTPUT_DIR/node.log"; then
    echo "ERROR: an Opus block refused what it was handed — see the reason below" >&2
    grep -E "$OPUS_REFUSALS" "$OUTPUT_DIR/node.log" >&2
    exit 1
fi

# First verdict: the block-level contract on the decoder's own output port —
# cadence and timestamp continuity, read off the wire rather than from the
# recorder that also does the measuring.
if ! "$HERE/verify_audio_channel.sh" OpusDecoder \
    --url "$CONTROL_URL" --count 64 --port audio >&2; then
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

"$PYTHON" "$HERE/known_audio_signal.py" analyse \
    "$CAPTURED_WAVEFORM" "$OUTPUT_DIR/spectrogram.png"
VERDICT=$?

echo "artifacts: $OUTPUT_DIR" >&2
exit "$VERDICT"

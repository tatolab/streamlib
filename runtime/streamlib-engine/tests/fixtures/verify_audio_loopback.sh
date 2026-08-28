#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# The known-signal loopback, with the engine carrying the audio both ways.
#
# Third of three audio fixtures, and the one that closes the rung. Where
# `e2e_audio_loopback.sh` proves the *rig* with no StreamLib in the path, and
# `verify_audio_channel.sh` proves one processor off its own port, this proves
# the round trip: `SpeakerSink` plays the known signal into a null sink and
# `MicrophoneSource` captures it back off that sink's monitor. Both ends are
# StreamLib, so a failure here with the rig fixture green is the engine's.
#
# Usage:
#   ./verify_audio_loopback.sh [--count N] [--port PORT]
#
# Exit status is the verdict, stdout is the report JSON and nothing else, so a
# caller can pipe it. Progress goes to stderr.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PYTHON="${PYTHON:-python3}"

# What the block-level tap asks for. Bounded by the control plane's own 500 ms
# sample window in practice, which is why the signal itself is measured off the
# waveform the node writes rather than off the tap.
BAG_COUNT=64
CONTROL_PORT="${CONTROL_PORT:-9077}"
while [ $# -gt 0 ]; do
    case "$1" in
        --count) BAG_COUNT="$2"; shift 2 ;;
        --port) CONTROL_PORT="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

OUTPUT_DIR="$(mktemp -d -t streamlib-audio-loopback-XXXXXX)"
CONTROL_URL="http://127.0.0.1:$CONTROL_PORT"

if ! "$HERE/virtual_audio_device.sh" check >&2; then
    echo "SKIP: no virtual audio device available on this machine" >&2
    exit 77
fi

NODE_PID=""
# Installed BEFORE the sink is created and before the node starts, and
# idempotent — `stop` handles "not running", and `kill` of an unset pid is
# swallowed. Installing it after would leave a window in which a failure
# strands an Audio/Sink in the user's live session, and `object.linger` means
# it outlives this process; wireplumber can then promote it to the default sink
# and silence the machine.
trap 'kill "$NODE_PID" 2>/dev/null; "$HERE/virtual_audio_device.sh" stop >&2' EXIT
# Without this the shell survives its interrupted children and runs on to the
# analysis, which can report PASS for a run the user aborted.
trap 'exit 130' INT TERM

SINK="$("$HERE/virtual_audio_device.sh" start)" || exit 1

CAPTURED_WAVEFORM="$OUTPUT_DIR/captured.wav"
echo "starting the loopback node against $SINK" >&2
(
    cd "$HERE" || exit 1
    STREAMLIB_AUDIO_SINK="$SINK" CONTROL_PORT="$CONTROL_PORT" \
        STREAMLIB_CAPTURED_WAVEFORM="$CAPTURED_WAVEFORM" \
        "$PYTHON" audio_loopback_node.py
) >"$OUTPUT_DIR/node.log" 2>&1 &
NODE_PID=$!

# Polled rather than slept: the node has a GPU context and an iceoryx2 node to
# bring up, and a fixed sleep is either flaky or slow.
for _ in $(seq 60); do
    if ! kill -0 "$NODE_PID" 2>/dev/null; then
        echo "ERROR: the loopback node exited before serving its control plane" >&2
        cat "$OUTPUT_DIR/node.log" >&2
        exit 1
    fi
    if "$PYTHON" -m streamlib.cli graph --url "$CONTROL_URL" >/dev/null 2>&1; then
        break
    fi
    sleep 0.5
done

# The refusal `SpeakerSink` owes a block it cannot play. Checked by name rather
# than left to show up as silence, because silence is also what a dead sink
# looks like and the two need different fixes.
if grep -q "cannot be played on a device running at" "$OUTPUT_DIR/node.log"; then
    echo "ERROR: the speaker refused the signal's format — there is no resampler yet" >&2
    grep "cannot be played on a device running at" "$OUTPUT_DIR/node.log" >&2
    exit 1
fi

# First verdict: the block-level contract on the microphone's own port —
# cadence, timestamp continuity, and a frame the engine did not re-stamp.
if ! "$HERE/verify_audio_channel.sh" MicrophoneSource \
    --url "$CONTROL_URL" --count "$BAG_COUNT" --port audio \
    --expect-frame-not-restamped >&2; then
    echo "ERROR: the microphone's channel failed its block-level contract" >&2
    exit 1
fi

# Second verdict, and the one this fixture exists for: the signal itself. The
# node writes what it captured once it has the whole thing.
echo "waiting for the node to write what it captured" >&2
for _ in $(seq 120); do
    if grep -q "MARKER:WAVEFORM_WRITTEN" "$OUTPUT_DIR/node.log"; then
        break
    fi
    if ! kill -0 "$NODE_PID" 2>/dev/null; then
        echo "ERROR: the loopback node exited before writing its capture" >&2
        tail -40 "$OUTPUT_DIR/node.log" >&2
        exit 1
    fi
    sleep 0.5
done
if ! [ -s "$CAPTURED_WAVEFORM" ]; then
    echo "ERROR: the node never wrote a capture to measure" >&2
    tail -40 "$OUTPUT_DIR/node.log" >&2
    exit 1
fi

"$PYTHON" "$HERE/known_audio_signal.py" analyse \
    "$CAPTURED_WAVEFORM" "$OUTPUT_DIR/spectrogram.png"
VERDICT=$?

echo "artifacts: $OUTPUT_DIR" >&2
exit "$VERDICT"

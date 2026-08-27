#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Known-signal audio loopback gate.
#
# Sister fixture to e2e_fixture_psnr_vivid.sh. Where that one guards the V4L2
# colour path by comparing captured channel means to a baseline, this one
# guards the audio path by playing a signal whose every property is known and
# measuring what comes back: the tone's frequency, amplitude and distortion,
# and the symbol stream's identity AND spacing.
#
# The spacing is the part that earns its keep. A tone survives a dropped block
# almost invisibly and so does a symbol's identity, so the gate measures the
# interval between symbol onsets — audio that goes missing between two symbols
# shortens exactly that interval, which both detects the loss and says where.
#
# No StreamLib anywhere in this path, deliberately. When the engine will not
# build, this still answers "is the rig sound", which is the question a
# verification tool that lives inside the runtime can never answer.
#
# Usage:
#   ./e2e_audio_loopback.sh [output_dir]
#
# Exit status is the verdict. Artifacts land in output_dir: the signal that was
# played, what came back, the JSON report, and a spectrogram to read by eye.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PYTHON="${PYTHON:-python3}"
OUTPUT_DIR="${1:-$(mktemp -d -t streamlib-audio-loopback-XXXXXX)}"

# Recording starts first and runs long, because a capture that opens after the
# signal begins loses the lead-in the analysis aligns on.
CAPTURE_LEAD_SECONDS=1.0
CAPTURE_SECONDS=8

mkdir -p "$OUTPUT_DIR"

if ! "$HERE/virtual_audio_device.sh" check; then
    echo "SKIP: no virtual audio device available on this machine" >&2
    exit 77
fi

SINK="$("$HERE/virtual_audio_device.sh" start)" || exit 1
trap '"$HERE/virtual_audio_device.sh" stop >/dev/null 2>&1' EXIT

"$PYTHON" "$HERE/known_audio_signal.py" generate "$OUTPUT_DIR/known_signal.wav" || exit 1

# `stream.capture.sink` is load-bearing: without it a capture stream aimed at a
# sink silently attaches to the session's default source instead and records
# whatever the machine's real microphone hears — a run that looks green while
# measuring nothing at all.
timeout "$CAPTURE_SECONDS" pw-record --target="$SINK" \
    -P '{ stream.capture.sink=true }' \
    --rate=48000 --channels=2 --format=s16 \
    "$OUTPUT_DIR/captured.wav" >/dev/null 2>&1 &
RECORDER_PID=$!

sleep "$CAPTURE_LEAD_SECONDS"
timeout "$CAPTURE_SECONDS" pw-play --target="$SINK" "$OUTPUT_DIR/known_signal.wav" >/dev/null 2>&1
sleep 0.5
kill "$RECORDER_PID" 2>/dev/null
wait "$RECORDER_PID" 2>/dev/null

"$PYTHON" "$HERE/known_audio_signal.py" analyse \
    "$OUTPUT_DIR/captured.wav" "$OUTPUT_DIR/spectrogram.png" \
    | tee "$OUTPUT_DIR/report.json"
VERDICT=${PIPESTATUS[0]}

echo "artifacts: $OUTPUT_DIR"
exit "$VERDICT"

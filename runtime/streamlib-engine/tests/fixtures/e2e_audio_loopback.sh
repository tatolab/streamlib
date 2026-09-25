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
# On macOS there is no null sink to play into, so the loop runs through the
# air: `afplay` out of the built-in speakers and `ffmpeg` in off the built-in
# microphone, pinned by name, scored with the analyser's acoustic parameter
# set. That is audible, so it runs attended only: it refuses with 77 unless
# STREAMLIB_RUN_ATTENDED_AUDIBLE_TESTS=1 says someone is listening.
#
# Usage:
#   ./e2e_audio_loopback.sh [output_dir]
#
# Exit status is the verdict, and stdout is the report JSON and nothing else,
# so a caller can pipe it. Progress goes to stderr. Artifacts land in
# output_dir: the signal that was played, what came back, the JSON report, the
# link that was actually captured, and a spectrogram to read by eye.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PYTHON="${PYTHON:-python3}"
PLATFORM="$(uname -s)"

if [ "$PLATFORM" = Darwin ] && [ "${STREAMLIB_RUN_ATTENDED_AUDIBLE_TESTS:-}" != 1 ]; then
    echo "SKIP: on macOS the rig-only loop runs through the air and plays out loud, so it" >&2
    echo "      runs attended only — set STREAMLIB_RUN_ATTENDED_AUDIBLE_TESTS=1 with someone listening" >&2
    exit 77
fi

# Spelled in full rather than `mktemp -t`, which BSD reads as a prefix to
# suffix under $TMPDIR — so the directory is where the skill looks on both.
TEMPORARY_DIRECTORY="${TMPDIR:-/tmp}"
OUTPUT_DIR="${1:-$(mktemp -d "${TEMPORARY_DIRECTORY%/}/streamlib-audio-loopback-XXXXXX")}"

# Recording starts first and runs long, because a capture that opens after the
# signal begins loses the lead-in the analysis aligns on.
CAPTURE_LEAD_SECONDS=1.0
CAPTURE_SECONDS=8

mkdir -p "$OUTPUT_DIR"

if [ "$PLATFORM" = Darwin ]; then
    for tool in afplay ffmpeg; do
        if ! command -v "$tool" &>/dev/null; then
            echo "SKIP: $tool not found" >&2
            exit 77
        fi
    done
    if ! "$PYTHON" -c "import numpy" &>/dev/null; then
        echo "SKIP: $PYTHON cannot import numpy" >&2
        exit 77
    fi
    # The microphone is pinned by name, so Camo, Wave Link or a Continuity
    # iPhone can never be what is measured. afplay takes no device, so the
    # speakers are pinned by requiring them to be the default output.
    MICROPHONE_NAME="$("$PYTHON" "$HERE/coreaudio_process_tap.py" built-in-microphone-name 2>/dev/null)"
    MICROPHONE_UID="$("$PYTHON" "$HERE/coreaudio_process_tap.py" built-in-microphone-uid 2>/dev/null)"
    SPEAKER_UID="$("$PYTHON" "$HERE/coreaudio_process_tap.py" built-in-speaker-uid 2>/dev/null)"
    DEFAULT_OUTPUT_UID="$("$PYTHON" "$HERE/coreaudio_process_tap.py" default-output-uid 2>/dev/null)"
    if [ -z "$MICROPHONE_NAME" ] || [ -z "$SPEAKER_UID" ]; then
        echo "SKIP: the acoustic loop needs the Mac's built-in speakers and microphone, and" >&2
        echo "      this one lacks one (or headphones are on the jack). What it has:" >&2
        "$PYTHON" "$HERE/coreaudio_process_tap.py" devices >&2
        exit 77
    fi
    if [ "$DEFAULT_OUTPUT_UID" != "$SPEAKER_UID" ]; then
        echo "SKIP: afplay plays to the default output, which is $DEFAULT_OUTPUT_UID rather than" >&2
        echo "      the built-in speakers — choose them in System Settings › Sound › Output" >&2
        exit 77
    fi

    INJECT_BUG="${INJECT_BUG:-}"
    if [ -n "$INJECT_BUG" ]; then
        echo "INJECTING FAULT: $INJECT_BUG — this run is expected to FAIL" >&2
        "$PYTHON" "$HERE/known_audio_signal.py" generate \
            "$OUTPUT_DIR/known_signal.wav" --inject "$INJECT_BUG" || exit 1
    else
        "$PYTHON" "$HERE/known_audio_signal.py" generate \
            "$OUTPUT_DIR/known_signal.wav" || exit 1
    fi

    {
        echo "microphone: $MICROPHONE_NAME ($MICROPHONE_UID), recorded by ffmpeg avfoundation"
        echo "speaker: $SPEAKER_UID, the default output afplay plays to"
    } >"$OUTPUT_DIR/capture_device.txt"

    RECORDER_PID=""
    trap 'kill "$RECORDER_PID" 2>/dev/null' EXIT
    trap 'exit 130' INT TERM
    # `-t` bounds the recording itself, so nothing here needs `timeout`.
    ffmpeg -hide_banner -nostdin -f avfoundation -i ":$MICROPHONE_NAME" \
        -t "$CAPTURE_SECONDS" -ac 1 -ar 48000 -c:a pcm_s16le -y \
        "$OUTPUT_DIR/captured.wav" >"$OUTPUT_DIR/ffmpeg.log" 2>&1 &
    RECORDER_PID=$!

    sleep "$CAPTURE_LEAD_SECONDS"

    if ! kill -0 "$RECORDER_PID" 2>/dev/null; then
        echo "ERROR: ffmpeg is not recording from $MICROPHONE_NAME — nothing is being captured" >&2
        cat "$OUTPUT_DIR/ffmpeg.log" >&2
        exit 1
    fi
    grep -m1 "^Input #0" "$OUTPUT_DIR/ffmpeg.log" >>"$OUTPUT_DIR/capture_device.txt"
    afplay -t "$CAPTURE_SECONDS" "$OUTPUT_DIR/known_signal.wav"
    # Bounded twice over: ffmpeg stops itself at `-t`, and this stops waiting
    # for it well after that.
    for _ in $(seq $((CAPTURE_SECONDS * 4))); do
        kill -0 "$RECORDER_PID" 2>/dev/null || break
        sleep 0.5
    done
    kill -INT "$RECORDER_PID" 2>/dev/null
    wait "$RECORDER_PID" 2>/dev/null
    if ! [ -s "$OUTPUT_DIR/captured.wav" ]; then
        echo "ERROR: ffmpeg wrote no capture to measure" >&2
        cat "$OUTPUT_DIR/ffmpeg.log" >&2
        exit 1
    fi

    "$PYTHON" "$HERE/known_audio_signal.py" analyse \
        "$OUTPUT_DIR/captured.wav" "$OUTPUT_DIR/spectrogram.png" --path acoustic \
        | tee "$OUTPUT_DIR/report.json"
    VERDICT=${PIPESTATUS[0]}

    echo "artifacts: $OUTPUT_DIR" >&2
    exit "$VERDICT"
fi

if ! "$HERE/virtual_audio_device.sh" check >&2; then
    echo "SKIP: no virtual audio device available on this machine" >&2
    exit 77
fi

# Installed BEFORE the node is created, and idempotent — `stop` handles "not
# running". Installing it after `start` leaves a window in which a failure
# strands an Audio/Sink in the user's live session, and `object.linger` means
# it outlives this process; wireplumber can then promote it to the default
# sink and silence the machine.
trap '"$HERE/virtual_audio_device.sh" stop >&2' EXIT
# Without this the shell survives its interrupted children and runs on to the
# analysis, which can report PASS for a run the user aborted.
trap 'exit 130' INT TERM

SINK="$("$HERE/virtual_audio_device.sh" start)" || exit 1

# INJECT_BUG plays a deliberately broken signal, so a rig run can prove the
# gate is live rather than only ever having been observed green.
INJECT_BUG="${INJECT_BUG:-}"
if [ -n "$INJECT_BUG" ]; then
    echo "INJECTING FAULT: $INJECT_BUG — this run is expected to FAIL" >&2
    "$PYTHON" "$HERE/known_audio_signal.py" generate \
        "$OUTPUT_DIR/known_signal.wav" --inject "$INJECT_BUG" || exit 1
else
    "$PYTHON" "$HERE/known_audio_signal.py" generate \
        "$OUTPUT_DIR/known_signal.wav" || exit 1
fi

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

# A PipeWire --target is a hint: an unresolvable one links to the session
# default instead of failing, so the loopback would still close — through the
# default sink — and the analysis would pass having never touched the fixture
# node. Verified rather than trusted, and the evidence is kept.
pw-link -l 2>/dev/null | grep -A2 "^pw-record" > "$OUTPUT_DIR/capture_link.txt"
if ! [ -s "$OUTPUT_DIR/capture_link.txt" ]; then
    echo "ERROR: pw-record is not running — nothing is being captured at all" >&2
    exit 1
fi
if ! grep -q "$SINK" "$OUTPUT_DIR/capture_link.txt"; then
    echo "ERROR: the recorder attached to something other than $SINK" >&2
    kill "$RECORDER_PID" 2>/dev/null
    exit 1
fi
timeout "$CAPTURE_SECONDS" pw-play --target="$SINK" "$OUTPUT_DIR/known_signal.wav" >/dev/null 2>&1
sleep 0.5
kill "$RECORDER_PID" 2>/dev/null
wait "$RECORDER_PID" 2>/dev/null

"$PYTHON" "$HERE/known_audio_signal.py" analyse \
    "$OUTPUT_DIR/captured.wav" "$OUTPUT_DIR/spectrogram.png" \
    | tee "$OUTPUT_DIR/report.json"
VERDICT=${PIPESTATUS[0]}

echo "artifacts: $OUTPUT_DIR" >&2
exit "$VERDICT"

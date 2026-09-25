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
# macOS has no null sink, so `--path` picks what closes the loop there. Both
# ends stay StreamLib on every path:
#
#   tap-muted    (default) a private, muted Core Audio process tap of the
#                node's own output, which `MicrophoneSource` opens through a
#                private aggregate device. Digital and silent, and scored as
#                strictly as the null sink.
#   tap-audible  the same tap unmuted, so the signal also plays out of the
#                built-in speakers while it is scored strictly.
#   acoustic     the built-in speakers into the built-in microphone, through
#                the air, scored with the analyser's acoustic parameter set.
#
# The two audible paths run attended only: they refuse with 77 unless
# STREAMLIB_RUN_ATTENDED_AUDIBLE_TESTS=1 says someone is listening.
#
# Usage:
#   ./verify_audio_loopback.sh [--count N] [--port PORT]
#                              [--path tap-muted|tap-audible|acoustic]
#
# INJECT_BUG=silence|drop|gain publishes a deliberately broken signal, so a run
# can prove the gate is live rather than only ever having been observed green.
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
LOOPBACK_PATH=""
while [ $# -gt 0 ]; do
    case "$1" in
        --count) BAG_COUNT="$2"; shift 2 ;;
        --port) CONTROL_PORT="$2"; shift 2 ;;
        --path) LOOPBACK_PATH="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

PLATFORM="$(uname -s)"
if [ "$PLATFORM" = Darwin ]; then
    LOOPBACK_PATH="${LOOPBACK_PATH:-tap-muted}"
    case "$LOOPBACK_PATH" in
        tap-muted|tap-audible|acoustic) ;;
        *) echo "unknown --path: $LOOPBACK_PATH (tap-muted, tap-audible or acoustic)" >&2; exit 2 ;;
    esac
elif [ -n "$LOOPBACK_PATH" ]; then
    echo "--path picks a macOS loopback; on $PLATFORM the loop is the null sink's monitor" >&2
    exit 2
fi

if [ "$PLATFORM" = Darwin ] && [ "$LOOPBACK_PATH" != tap-muted ] \
    && [ "${STREAMLIB_RUN_ATTENDED_AUDIBLE_TESTS:-}" != 1 ]; then
    echo "SKIP: --path $LOOPBACK_PATH plays out loud, so it runs attended only —" >&2
    echo "      set STREAMLIB_RUN_ATTENDED_AUDIBLE_TESTS=1 with someone listening" >&2
    exit 77
fi

# Spelled in full rather than `mktemp -t`, which BSD reads as a prefix to
# suffix under $TMPDIR — so the directory is where the skill looks on both.
TEMPORARY_DIRECTORY="${TMPDIR:-/tmp}"
OUTPUT_DIR="$(mktemp -d "${TEMPORARY_DIRECTORY%/}/streamlib-audio-loopback-XXXXXX")"
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

ANALYSIS_PATH=digital
if [ "$PLATFORM" = Darwin ]; then
    # Pinned rather than the default output, so the tap reads a 48 kHz device
    # whatever the user has plugged in, and `acoustic` measures the laptop's own
    # speakers rather than headphones or a Bluetooth device.
    SPEAKER_DEVICE_ID="$("$PYTHON" "$HERE/coreaudio_process_tap.py" built-in-speaker-uid 2>/dev/null)"
    case "$LOOPBACK_PATH" in
        acoustic)
            CAPTURE_DEVICE_ID="$("$PYTHON" "$HERE/coreaudio_process_tap.py" built-in-microphone-uid 2>/dev/null)"
            if [ -z "$SPEAKER_DEVICE_ID" ] || [ -z "$CAPTURE_DEVICE_ID" ]; then
                echo "SKIP: --path acoustic needs the Mac's built-in speakers and microphone, and" >&2
                echo "      this one lacks one (or headphones are on the jack). What it has:" >&2
                "$PYTHON" "$HERE/coreaudio_process_tap.py" devices >&2
                exit 77
            fi
            PROCESS_TAP_MUTE_BEHAVIOUR=""
            ANALYSIS_PATH=acoustic
            ;;
        tap-muted | tap-audible)
            CAPTURE_DEVICE_ID="$("$HERE/virtual_audio_device.sh" start)" || exit 1
            PROCESS_TAP_MUTE_BEHAVIOUR=muted
            [ "$LOOPBACK_PATH" = tap-audible ] && PROCESS_TAP_MUTE_BEHAVIOUR=unmuted
            ;;
    esac
else
    SINK="$("$HERE/virtual_audio_device.sh" start)" || exit 1
fi

INJECT_BUG="${INJECT_BUG:-}"
if [ -n "$INJECT_BUG" ]; then
    echo "INJECTING FAULT: $INJECT_BUG — this run is expected to FAIL" >&2
fi

CAPTURED_WAVEFORM="$OUTPUT_DIR/captured.wav"
if [ "$PLATFORM" = Darwin ]; then
    echo "starting the loopback node ($LOOPBACK_PATH): speaker" \
        "${SPEAKER_DEVICE_ID:-<default output>}, microphone $CAPTURE_DEVICE_ID" >&2
    # `exec`, so NODE_PID is the node itself: a private tap lives exactly as
    # long as its process, and the EXIT trap has to reach that process.
    (
        cd "$HERE" || exit 1
        STREAMLIB_AUDIO_SINK="$SPEAKER_DEVICE_ID" \
            STREAMLIB_AUDIO_CAPTURE_DEVICE_ID="$CAPTURE_DEVICE_ID" \
            STREAMLIB_COREAUDIO_PROCESS_TAP_MUTE_BEHAVIOUR="$PROCESS_TAP_MUTE_BEHAVIOUR" \
            STREAMLIB_KNOWN_SIGNAL_INJECT="$INJECT_BUG" \
            CONTROL_PORT="$CONTROL_PORT" \
            STREAMLIB_CAPTURED_WAVEFORM="$CAPTURED_WAVEFORM" \
            exec "$PYTHON" audio_loopback_node.py
    ) >"$OUTPUT_DIR/node.log" 2>&1 &
else
    echo "starting the loopback node against $SINK" >&2
    (
        cd "$HERE" || exit 1
        STREAMLIB_AUDIO_SINK="$SINK" CONTROL_PORT="$CONTROL_PORT" \
            STREAMLIB_KNOWN_SIGNAL_INJECT="$INJECT_BUG" \
            STREAMLIB_CAPTURED_WAVEFORM="$CAPTURED_WAVEFORM" \
            "$PYTHON" audio_loopback_node.py
    ) >"$OUTPUT_DIR/node.log" 2>&1 &
fi
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

# The first line in node.log matching `pattern`, polled for while the node is
# up: the audio built-ins probe and open their devices in setup, which can land
# after the control plane is already answering.
first_node_log_line_matching() {
    local pattern="$1" line
    for _ in $(seq 60); do
        line="$(grep -m1 -e "$pattern" "$OUTPUT_DIR/node.log")"
        if [ -n "$line" ]; then
            printf '%s\n' "$line"
            return 0
        fi
        kill -0 "$NODE_PID" 2>/dev/null || return 1
        sleep 0.5
    done
    return 1
}

if [ "$PLATFORM" = Darwin ]; then
    # The node runs whatever `_engine.abi3.so` the interpreter imports, and one
    # predating the CoreAudio arm lands on the silent-null arm — which captures
    # silence and plays nothing, so a loopback over it measures nothing at all.
    COREAUDIO_ARM='audio_backend="?coreaudio("|[[:space:]]|$)'
    if ! PROBE_LINE="$(first_node_log_line_matching "audio device backend chain probed")"; then
        echo "ERROR: the node never probed an audio backend" >&2
        tail -40 "$OUTPUT_DIR/node.log" >&2
        exit 1
    fi
    if ! printf '%s' "$PROBE_LINE" | grep -Eq "$COREAUDIO_ARM"; then
        if grep "demoting to the next arm" "$OUTPUT_DIR/node.log" | grep -Eq "$COREAUDIO_ARM"; then
            echo "ERROR: this wheel carries the CoreAudio arm and it did not open:" >&2
            grep "demoting to the next arm" "$OUTPUT_DIR/node.log" >&2
            exit 1
        fi
        echo "SKIP: refusing to score — the engine probed another audio arm than coreaudio," >&2
        echo "      so this wheel predates the CoreAudio arm and would measure silence:" >&2
        echo "      $PROBE_LINE" >&2
        echo "      Rebuild it: (cd sdk/streamlib-python-wheel && maturin develop --release)" >&2
        exit 77
    fi
    echo "probed: $PROBE_LINE" >&2

    # The macOS analogue of the rig fixture's link check: a capture that opened
    # anything but the device named here — the real microphone, most likely —
    # would still close a loop and could pass.
    if ! MICROPHONE_OPENED="$(first_node_log_line_matching "MicrophoneSource: capture stream opened")"; then
        echo "ERROR: MicrophoneSource never opened $CAPTURE_DEVICE_ID" >&2
        tail -40 "$OUTPUT_DIR/node.log" >&2
        exit 1
    fi
    if ! printf '%s' "$MICROPHONE_OPENED" | grep -qF "$CAPTURE_DEVICE_ID"; then
        echo "ERROR: MicrophoneSource opened something other than $CAPTURE_DEVICE_ID:" >&2
        echo "      $MICROPHONE_OPENED" >&2
        exit 1
    fi
    if [ -n "$SPEAKER_DEVICE_ID" ]; then
        if ! SPEAKER_OPENED="$(first_node_log_line_matching "SpeakerSink: playback stream opened")" \
            || ! printf '%s' "$SPEAKER_OPENED" | grep -qF "$SPEAKER_DEVICE_ID"; then
            echo "ERROR: SpeakerSink did not open $SPEAKER_DEVICE_ID" >&2
            tail -40 "$OUTPUT_DIR/node.log" >&2
            exit 1
        fi
    fi
fi

# A bag the windowing stage could not read, by name rather than left to show up
# as silence — silence is also what a dead sink looks like and the two need
# different fixes. The speaker no longer refuses a format it cannot play: its
# port declares `audio_window = match_device`, so the stage converts. What it
# still refuses is a bag it cannot decode as an audio block at all.
if grep -q "cannot be read as an audio block" "$OUTPUT_DIR/node.log"; then
    echo "ERROR: the speaker's port refused the signal's bags — see the reason below" >&2
    grep "cannot be read as an audio block" "$OUTPUT_DIR/node.log" >&2
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

# A process tap with no System Audio Recording grant behind it reports no
# error: it delivers exact zeros. Told apart from an engine that played nothing
# by asking TCC, which answers without prompting.
if [ "$PLATFORM" = Darwin ] && [ "$ANALYSIS_PATH" = digital ] \
    && "$PYTHON" "$HERE/known_audio_signal.py" exact-digital-silence "$CAPTURED_WAVEFORM"; then
    AUTHORIZATION="$("$PYTHON" "$HERE/coreaudio_process_tap.py" system-audio-recording-authorization)"
    if [ "$AUTHORIZATION" = authorized ]; then
        echo "ERROR: the tap heard exact zeros with System Audio Recording authorized —" >&2
        echo "       SpeakerSink played nothing the tap could hear" >&2
        echo "artifacts: $OUTPUT_DIR" >&2
        exit 1
    fi
    echo "SKIP: the process tap delivered exact zeros, and TCC says System Audio Recording is" >&2
    echo "      $AUTHORIZATION for the app that launched this. Grant it in System Settings ›" >&2
    echo "      Privacy & Security › Screen & System Audio Recording › System Audio Recording" >&2
    echo "      Only (add Terminal, or whichever app launched this), then run again." >&2
    echo "artifacts: $OUTPUT_DIR" >&2
    exit 77
fi

"$PYTHON" "$HERE/known_audio_signal.py" analyse \
    "$CAPTURED_WAVEFORM" "$OUTPUT_DIR/spectrogram.png" --path "$ANALYSIS_PATH"
VERDICT=$?

echo "artifacts: $OUTPUT_DIR" >&2
exit "$VERDICT"

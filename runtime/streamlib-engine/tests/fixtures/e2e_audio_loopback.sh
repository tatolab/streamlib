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
# macOS has no null sink, so `--path` picks what closes the loop there, and
# each is the rig peer of `verify_audio_loopback.sh` on the same path:
#
#   tap-muted    (default) an IOProc plays into the built-in speakers and a
#                private, muted Core Audio process tap of this fixture's own
#                process carries it back through a private aggregate device —
#                the tap the engine loopback reads, made the same way
#                (`process_tap_loopback_without_the_engine.py`). Digital and
#                silent, and scored as strictly as the null sink.
#   tap-audible  the same tap unmuted, so the signal also plays out loud.
#   acoustic     `afplay` out of the built-in speakers and `ffmpeg` in off the
#                built-in microphone, pinned by name, through the air, scored
#                with the analyser's acoustic parameter set.
#
# The two audible paths run attended only: they refuse with 77 unless
# STREAMLIB_RUN_ATTENDED_AUDIBLE_TESTS=1 says someone is listening. The tap
# paths ask TCC for System Audio Recording before making a tap and fail closed,
# as `verify_audio_loopback.sh` does.
#
# Usage:
#   ./e2e_audio_loopback.sh [--path tap-muted|tap-audible|acoustic] [output_dir]
#
# Exit status is the verdict, and stdout is the report JSON and nothing else,
# so a caller can pipe it. Progress goes to stderr. Artifacts land in
# output_dir: the signal that was played, what came back, the JSON report, the
# link that was actually captured, and a spectrogram to read by eye.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PYTHON="${PYTHON:-python3}"
PLATFORM="$(uname -s)"

LOOPBACK_PATH=""
OUTPUT_DIR_ARGUMENT=""
while [ $# -gt 0 ]; do
    case "$1" in
        --path) LOOPBACK_PATH="$2"; shift 2 ;;
        -*) echo "unknown argument: $1" >&2; exit 2 ;;
        *) OUTPUT_DIR_ARGUMENT="$1"; shift ;;
    esac
done

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
OUTPUT_DIR="${OUTPUT_DIR_ARGUMENT:-$(mktemp -d "${TEMPORARY_DIRECTORY%/}/streamlib-audio-loopback-XXXXXX")}"

# Recording starts first and runs long, because a capture that opens after the
# signal begins loses the lead-in the analysis aligns on.
CAPTURE_LEAD_SECONDS=1.0
CAPTURE_SECONDS=8

mkdir -p "$OUTPUT_DIR"

if [ "$PLATFORM" = Darwin ]; then
    # Without this the shell survives its interrupted children and runs on to
    # the analysis, which can report PASS for a run the user aborted.
    trap 'exit 130' INT TERM
    if ! "$PYTHON" -c "import numpy" &>/dev/null; then
        echo "SKIP: $PYTHON cannot import numpy" >&2
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
fi

if [ "$PLATFORM" = Darwin ] && [ "$LOOPBACK_PATH" != acoustic ]; then
    # TCC is asked before any tap exists, and the answer fails closed: a
    # preflight that could not be asked is an error, and unattended only
    # `authorized` makes a tap. Re-checked here rather than trusted to the
    # helper's verdict.
    AUTHORIZATION_BEFORE_THE_RUN="$("$PYTHON" "$HERE/coreaudio_process_tap.py" authorize-a-tap)"
    AUTHORIZE_A_TAP_STATUS=$?
    [ "$AUTHORIZE_A_TAP_STATUS" -eq 77 ] && exit 77
    if [ "$AUTHORIZE_A_TAP_STATUS" -ne 0 ] || [ -z "$AUTHORIZATION_BEFORE_THE_RUN" ]; then
        echo "ERROR: could not ask TCC whether System Audio Recording is granted (the" >&2
        echo "       preflight exited $AUTHORIZE_A_TAP_STATUS and answered" \
            "'$AUTHORIZATION_BEFORE_THE_RUN'), so no tap is made" >&2
        exit 1
    fi
    if [ "$AUTHORIZATION_BEFORE_THE_RUN" != authorized ] \
        && [ "${STREAMLIB_RUN_ATTENDED_AUDIBLE_TESTS:-}" != 1 ]; then
        echo "ERROR: TCC answered '$AUTHORIZATION_BEFORE_THE_RUN' and this run is unattended," \
            "when only 'authorized' may make a tap unattended — no tap is made" >&2
        exit 1
    fi

    # The built-in speakers where there are any, as the engine loopback pins
    # them; headphones on the jack leave the default output.
    SPEAKER_UID="$("$PYTHON" "$HERE/coreaudio_process_tap.py" built-in-speaker-uid 2>/dev/null)"
    if [ -z "$SPEAKER_UID" ]; then
        SPEAKER_UID="$("$PYTHON" "$HERE/coreaudio_process_tap.py" default-output-uid 2>/dev/null)"
    fi
    if [ -z "$SPEAKER_UID" ]; then
        echo "SKIP: this Mac has no output device for the tap to hear" >&2
        exit 77
    fi
    PROCESS_TAP_MUTE_BEHAVIOUR=muted
    [ "$LOOPBACK_PATH" = tap-audible ] && PROCESS_TAP_MUTE_BEHAVIOUR=unmuted

    echo "playing into a $PROCESS_TAP_MUTE_BEHAVIOUR process tap of $SPEAKER_UID, no StreamLib" >&2
    "$PYTHON" "$HERE/process_tap_loopback_without_the_engine.py" \
        "$OUTPUT_DIR/known_signal.wav" "$OUTPUT_DIR/captured.wav" \
        "$SPEAKER_UID" "$PROCESS_TAP_MUTE_BEHAVIOUR" >"$OUTPUT_DIR/capture_device.txt"
    PROCESS_TAP_LOOP_STATUS=$?
    [ "$PROCESS_TAP_LOOP_STATUS" -eq 77 ] && exit 77
    if [ "$PROCESS_TAP_LOOP_STATUS" -ne 0 ] || ! [ -s "$OUTPUT_DIR/captured.wav" ]; then
        echo "ERROR: the process tap loop did not close (exit $PROCESS_TAP_LOOP_STATUS)" >&2
        echo "artifacts: $OUTPUT_DIR" >&2
        exit 1
    fi

    # A tap with no grant behind it reports no error: it delivers exact zeros.
    if "$PYTHON" "$HERE/known_audio_signal.py" exact-digital-silence "$OUTPUT_DIR/captured.wav"; then
        "$PYTHON" "$HERE/coreaudio_process_tap.py" explain-exact-zeros \
            "$AUTHORIZATION_BEFORE_THE_RUN"
        EXACT_ZEROS_STATUS=$?
        echo "artifacts: $OUTPUT_DIR" >&2
        [ "$EXACT_ZEROS_STATUS" -eq 77 ] && exit 77
        exit 1
    fi

    "$PYTHON" "$HERE/known_audio_signal.py" analyse \
        "$OUTPUT_DIR/captured.wav" "$OUTPUT_DIR/spectrogram.png" \
        | tee "$OUTPUT_DIR/report.json"
    VERDICT=${PIPESTATUS[0]}

    echo "artifacts: $OUTPUT_DIR" >&2
    # The prompt is answered while the signal plays, and the tap delivers zeros
    # until it is, so a failing run that started without the grant has measured
    # the prompt rather than the tap.
    if [ "$VERDICT" -ne 0 ] && [ "$AUTHORIZATION_BEFORE_THE_RUN" != authorized ]; then
        echo "SKIP: this run started with System Audio Recording $AUTHORIZATION_BEFORE_THE_RUN, so" >&2
        echo "      the capture may have a hole until the prompt was answered — not scored. Run again." >&2
        exit 77
    fi
    exit "$VERDICT"
fi

if [ "$PLATFORM" = Darwin ]; then
    for tool in afplay ffmpeg; do
        if ! command -v "$tool" &>/dev/null; then
            echo "SKIP: $tool not found" >&2
            exit 77
        fi
    done
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

    {
        echo "microphone: $MICROPHONE_NAME ($MICROPHONE_UID), recorded by ffmpeg avfoundation"
        echo "speaker: $SPEAKER_UID, the default output afplay plays to"
    } >"$OUTPUT_DIR/capture_device.txt"

    RECORDER_PID=""
    trap 'kill "$RECORDER_PID" 2>/dev/null' EXIT
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

#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# The microphone A/B: the built-in `MicrophoneSource` and this wheel's
# `MicrophoneProofSource`, each into the same `AudioBlockArrivalRecorder`, one
# after the other on the same device, each on a runtime this script starts and
# stops itself, under the same CPU stress. `audio_block_arrival_report.py`
# scores the two recordings against the ticket's bar.
#
# The built-in opens the audio backend's default source; the proof opens the
# ALSA PCM `PROOF_PCM_NAME` (default `default`, which on a PipeWire machine is
# PipeWire's default source through its ALSA plugin), so both capture the same
# device. Name a device for both with BUILT_IN_DEVICE_ID and PROOF_PCM_NAME.
#
# Usage:
#   packages/streamlib-microphone-proof/tests/ab/microphone_ab.sh [output_dir]
#
# Environment:
#   RUN_SECONDS               each arm's measured length (default 600)
#   CPU_STRESS_PROCESSES      busy processes beside both arms (default: nproc)
#   PROOF_GIL_CHURN_THREADS   pure-Python threads in the proof's interpreter (default 0)
#   PROOF_PCM_NAME            ALSA PCM the proof opens (default `default`)
#   BUILT_IN_DEVICE_ID        backend device id the built-in opens (default: the default source)
#   WARMUP_SECONDS            discarded at the start of each recording (default 2)
#
# Exit codes: 0 = the proof meets the bar, 1 = it misses or a run failed, 77 = cannot run.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
REPO_ROOT="$(cd "$PACKAGE_DIR/../.." && pwd)"
ENGINE_FIXTURES="$REPO_ROOT/runtime/streamlib-engine/tests/fixtures"

OUTPUT_DIR="${1:-/tmp/streamlib-microphone-ab-$(date +%s)}"
RUN_SECONDS="${RUN_SECONDS:-600}"
CPU_STRESS_PROCESSES="${CPU_STRESS_PROCESSES:-$(nproc)}"
PROOF_GIL_CHURN_THREADS="${PROOF_GIL_CHURN_THREADS:-0}"
PROOF_PCM_NAME="${PROOF_PCM_NAME:-default}"
BUILT_IN_DEVICE_ID="${BUILT_IN_DEVICE_ID:-}"
WARMUP_SECONDS="${WARMUP_SECONDS:-2}"
STREAM_FILE="tests/ab/microphone_ab_stream.py"

say() { echo "[microphone-ab] $*"; }
cannot_run() { echo "[microphone-ab] SKIP: $*" >&2; exit 77; }
fail() { echo "[microphone-ab] FAIL: $*" >&2; exit 1; }

# shellcheck source=../../../../runtime/streamlib-engine/tests/fixtures/fixture_runtime_unit.sh
. "$ENGINE_FIXTURES/fixture_runtime_unit.sh"
require_the_runtime_unit
require_the_fixture_venv
refuse_while_a_runtime_answers_at_the_local_api_socket \
    || cannot_run "a runtime already holds this machine; this run starts its own"

VENV_PYTHON="$PACKAGE_DIR/.venv/bin/python"
[ -x "$VENV_PYTHON" ] || cannot_run \
    "no venv at $PACKAGE_DIR/.venv — create it with tatolab-stream and \`maturin develop --release\` this wheel into it"
"$VENV_PYTHON" -c 'import tatolab.stream, tatolab.microphone_proof' 2>/dev/null \
    || cannot_run "the venv cannot import this wheel beside tatolab-stream; rebuild with \`maturin develop --release\`"

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

STRESS_PIDS=()
start_the_cpu_stress() {
    local _
    for _ in $(seq 1 "$CPU_STRESS_PROCESSES"); do
        yes >/dev/null &
        STRESS_PIDS+=("$!")
    done
}
stop_the_cpu_stress() {
    local stress_pid
    for stress_pid in "${STRESS_PIDS[@]}"; do
        kill "$stress_pid" 2>/dev/null || true
        wait "$stress_pid" 2>/dev/null || true
    done
    STRESS_PIDS=()
}

RUNTIME_STATE_HOME=""
stop_everything() {
    stop_the_fixture_stream_runs
    stop_the_fixture_runtime
    stop_the_cpu_stress
    [ -z "$RUNTIME_STATE_HOME" ] || rm -rf "$RUNTIME_STATE_HOME"
}
trap stop_everything EXIT

{
    echo "date: $(date -Iseconds)"
    echo "host: $(hostname)"
    echo "kernel: $(uname -r)"
    echo "cpus: $(nproc)"
    echo "commit: $(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null)"
    echo "run_seconds: $RUN_SECONDS"
    echo "cpu_stress_processes: $CPU_STRESS_PROCESSES"
    echo "proof_gil_churn_threads: $PROOF_GIL_CHURN_THREADS"
    echo "proof_pcm_name: $PROOF_PCM_NAME"
    echo "built_in_device_id: ${BUILT_IN_DEVICE_ID:-<backend default>}"
    if command -v pactl >/dev/null; then
        echo "default_source: $(pactl get-default-source 2>/dev/null)"
    fi
    if command -v pipewire >/dev/null; then
        echo "pipewire: $(pipewire --version 2>/dev/null | tail -1)"
    fi
} > "$OUTPUT_DIR/rig.txt"
cat "$OUTPUT_DIR/rig.txt"

run_one_arm() {
    local arm="$1" stream_function="$2"
    local recording="$OUTPUT_DIR/$arm.csv"
    RUNTIME_STATE_HOME="$(mktemp -d "${TMPDIR:-/tmp}/streamlib-microphone-ab-state-XXXXXX")"
    say "Arm $arm: starting the runtime"
    STREAMLIB_MICROPHONE_AB_RECORDING_PATH="$recording" \
    STREAMLIB_MICROPHONE_AB_BUILT_IN_DEVICE_ID="$BUILT_IN_DEVICE_ID" \
    STREAMLIB_MICROPHONE_AB_PROOF_PCM_NAME="$PROOF_PCM_NAME" \
    STREAMLIB_MICROPHONE_AB_PROOF_GIL_CHURN_THREADS="$PROOF_GIL_CHURN_THREADS" \
    RUST_LOG="${RUST_LOG:-warn,streamlib=info,streamlib_media_builtins=info}" \
        start_the_fixture_runtime "$OUTPUT_DIR/$arm.runtime.log" "$RUNTIME_STATE_HOME" \
        || fail "arm $arm: the runtime did not start"
    start_the_cpu_stress
    start_the_fixture_stream_run "$PACKAGE_DIR" "$STREAM_FILE:$stream_function" \
        "microphone_ab_$arm" "$OUTPUT_DIR/$arm.run.log" "$(( RUN_SECONDS + 120 ))" || {
        tail -40 "$OUTPUT_DIR/$arm.run.log" >&2
        tail -40 "$OUTPUT_DIR/$arm.runtime.log" >&2
        fail "arm $arm: the stream never ran"
    }
    say "Arm $arm: running for ${RUN_SECONDS}s under $CPU_STRESS_PROCESSES stress processes"
    sleep "$(( RUN_SECONDS + WARMUP_SECONDS ))"
    stop_the_fixture_stream_runs
    stop_the_cpu_stress
    stop_the_fixture_runtime 15
    rm -rf "$RUNTIME_STATE_HOME"
    RUNTIME_STATE_HOME=""
    [ -s "$recording" ] || fail "arm $arm recorded nothing"
    say "Arm $arm: $(( $(wc -l < "$recording") - 1 )) blocks recorded"
    grep -hE "MicrophoneSource: (capture stream opened|teardown)|MicrophoneProofSource: (opened|capturing|stopped)|audio device backend chain probed" \
        "$OUTPUT_DIR/$arm.runtime.log" "$OUTPUT_DIR/$arm.run.log" 2>/dev/null \
        | tee "$OUTPUT_DIR/$arm.source_report.txt"
}

run_one_arm built_in built_in_microphone_into_the_recorder
run_one_arm proof proof_microphone_into_the_recorder

"$VENV_PYTHON" "$SCRIPT_DIR/audio_block_arrival_report.py" \
    "$OUTPUT_DIR/built_in.csv" "$OUTPUT_DIR/proof.csv" \
    --warmup-seconds "$WARMUP_SECONDS" \
    --markdown "$OUTPUT_DIR/report.md" --json "$OUTPUT_DIR/report.json"
REPORT_EXIT=$?
say "Output dir: $OUTPUT_DIR"
exit "$REPORT_EXIT"

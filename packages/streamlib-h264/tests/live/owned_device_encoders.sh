#!/usr/bin/env bash
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
#
# The owned-device H.264 encoder on a real runtime, in two modes.
#
#   isolation   Two streams, each TestPatternSource → this wheel's
#               H264Encoder, on one runtime. Once both write, the first
#               stream's encoder helper is sent SIGKILL. The gate: the runtime
#               logs that helper's death, the second stream's encoder keeps
#               writing, the runtime keeps answering, no DEVICE_LOST reaches its
#               log — and the second stream's track, which it sends through the
#               `tatolab-moq` gateway nodes to a `moq-relay` this script
#               starts, read there and decoded by ffmpeg, holds the engine's H.264 test-pattern drift
#               lock (`cargo xtask psnr bar-colors` against the codec rig's
#               baseline), which proves the module's bitstream is the engine's.
#
#   cost N      N streams (1, 2 or 4), each with its own encoder helper and
#               owned device. Reports, per helper: GPU memory the driver
#               attributes to it (nvidia-smi), the owned device's creation time
#               and the first session's mint time (its own log lines), and the
#               frames its port wrote over a measured window — the GPU
#               timeslice cost shows as the rate each holds as N grows, beside
#               the encoder engine's utilisation.
#
# The streams load as a user's do: this script writes a project — a
# `pyproject.toml` and a `stream.py` defining one `@stream` per encoder over
# `owned_device_encoder_streams.py` — whose `.venv` is this package's own
# (`tatolab-stream` and a current `maturin develop` of this wheel), and queues
# its load with `tatolab start --dir` on a `tatolabd` this script starts and stops
# itself through the engine fixtures' `fixture_runtime_unit.sh`, so it refuses
# to run while another runtime holds the machine. The relay each stream's
# gateway dials is the fixture relay, which this script starts and stops too;
# its URL reaches the streams as `STREAMLIB_FIXTURE_RELAY_URL` in the runtime's
# environment.
#
# Usage:
#   packages/streamlib-h264/tests/live/owned_device_encoders.sh isolation [output_dir]
#   packages/streamlib-h264/tests/live/owned_device_encoders.sh cost <1|2|4> [output_dir]
#
# Environment:
#   RUN_SECONDS        budget of each stream run (default 120)
#   MEASURE_SECONDS    the cost mode's measured window (default 10)
#   OBJECT_COUNT       frames of the surviving track read and measured (default 20)
#   TOLERANCE          abs per-bar channel drift bound (default 0.05)
#
# Exit codes: 0 = pass, 1 = fail, 77 = cannot run.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
REPO_ROOT="$(cd "$PACKAGE_DIR/../.." && pwd)"
ENGINE_FIXTURES="$REPO_ROOT/runtime/streamlib-engine/tests/fixtures"
ENGINE_H264_BASELINE_TSV="$ENGINE_FIXTURES/psnr_test_pattern_bar_colors_baseline_h264.tsv"

MODE="${1:-}"
case "$MODE" in
    isolation)
        STREAM_COUNT=2
        OUTPUT_DIR="${2:-/tmp/streamlib-h264-isolation-$(date +%s)}"
        ;;
    cost)
        STREAM_COUNT="${2:-}"
        case "$STREAM_COUNT" in 1 | 2 | 4) ;; *)
            echo "[h264-owned-device] FAIL: cost takes 1, 2 or 4 streams" >&2
            exit 1
            ;;
        esac
        OUTPUT_DIR="${3:-/tmp/streamlib-h264-cost-$STREAM_COUNT-$(date +%s)}"
        ;;
    *)
        echo "usage: $0 isolation [output_dir] | cost <1|2|4> [output_dir]" >&2
        exit 1
        ;;
esac
RUN_SECONDS="${RUN_SECONDS:-120}"
MEASURE_SECONDS="${MEASURE_SECONDS:-10}"
OBJECT_COUNT="${OBJECT_COUNT:-20}"
TOLERANCE="${TOLERANCE:-0.05}"

say() { echo "[h264-owned-device] $*"; }
cannot_run() { echo "[h264-owned-device] SKIP: $*" >&2; exit 77; }
fail() { echo "[h264-owned-device] FAIL: $*" >&2; exit 1; }

for tool in cargo python3 ffmpeg; do
    command -v "$tool" >/dev/null || cannot_run "missing: $tool"
done
compgen -G "/dev/dri/renderD*" >/dev/null \
    || cannot_run "no DRM render node, so no GPU-backed runtime can start here"

# shellcheck source=../../../../runtime/streamlib-engine/tests/fixtures/fixture_runtime_unit.sh
. "$ENGINE_FIXTURES/fixture_runtime_unit.sh"
require_the_runtime_unit
require_the_fixture_venv
refuse_while_a_runtime_answers_at_the_local_api_socket \
    || cannot_run "a runtime already holds this machine; this run starts its own"

VENV_PYTHON="$PACKAGE_DIR/.venv/bin/python"
[ -x "$VENV_PYTHON" ] || cannot_run \
    "no venv at $PACKAGE_DIR/.venv — create it with tatolab-stream and tatolab-testpattern, and \`maturin develop\` this wheel into it"

# Each stream sends its encoder's port through the MoQ gateway module, built
# from this checkout into that venv when it cannot import it.
if ! "$VENV_PYTHON" -c 'import tatolab.moq' >/dev/null 2>&1; then
    say "building tatolab-moq into $PACKAGE_DIR/.venv"
    VIRTUAL_ENV="$PACKAGE_DIR/.venv" uv pip install --quiet "$REPO_ROOT/packages/tatolab-moq" >&2 \
        || cannot_run "the MoQ gateway module did not build into $PACKAGE_DIR/.venv"
fi
if ! IMPORT_FAILURE="$("$VENV_PYTHON" -c '
from tatolab.h264 import H264Encoder
from tatolab.moq import MoqGateway, MoqTrack, MoqTrackSend
from tatolab.testpattern import TestPatternSource
_ = (TestPatternSource, H264Encoder, MoqGateway, MoqTrack, MoqTrackSend)
' 2>&1)"; then
    say "$IMPORT_FAILURE" >&2
    cannot_run "the venv cannot import this wheel beside tatolab-stream. Rebuild with \`maturin develop --release\`."
fi

mkdir -p "$OUTPUT_DIR"
LOG_FILE="$OUTPUT_DIR/runtime.log"
RUNTIME_STATE_HOME="$(mktemp -d "${TMPDIR:-/tmp}/streamlib-h264-runtime-state-XXXXXX")"
stop_everything() {
    stop_the_fixture_runtime
    stop_the_fixture_relay
    rm -rf "$RUNTIME_STATE_HOME"
}
trap stop_everything EXIT

require_the_fixture_relay
start_the_fixture_relay "$OUTPUT_DIR/relay.log" "$RUNTIME_STATE_HOME/relay" \
    || fail "could not start the relay the streams send their encoders' tracks to"
say "Relay:             $FIXTURE_RELAY_URL"

if ! STREAMLIB_FIXTURE_RELAY_URL="$FIXTURE_RELAY_URL" \
    RUST_LOG="${RUST_LOG:-warn,streamlib=info,streamlib_media_builtins=info,_native=info,tatolab_module=info}" \
        start_the_fixture_runtime "$LOG_FILE" "$RUNTIME_STATE_HOME"; then
    fail "could not start the runtime the streams run on"
fi

PROJECT_DIR="$OUTPUT_DIR/owned-device-encoders"
mkdir -p "$PROJECT_DIR"
ln -sfn "$PACKAGE_DIR/.venv" "$PROJECT_DIR/.venv"
ln -sfn "$SCRIPT_DIR/owned_device_encoder_streams.py" "$PROJECT_DIR/owned_device_encoder_streams.py"
cat > "$PROJECT_DIR/pyproject.toml" <<'TOML'
[project]
name = "owned-device-encoders"
version = "0.0.0"
requires-python = ">=3.12"
TOML
{
    echo "from tatolab.stream import StreamBuilder, stream"
    echo "from owned_device_encoder_streams import test_pattern_through_an_owned_device_encoder"
    for stream_number in $(seq 1 "$STREAM_COUNT"); do
        echo ""
        echo ""
        echo "@stream"
        echo "def owned_device_encoder_$stream_number(stream_builder: StreamBuilder) -> None:"
        echo "    test_pattern_through_an_owned_device_encoder(stream_builder, $stream_number)"
    done
} > "$PROJECT_DIR/stream.py"

"$TATOLAB_EXECUTABLE" start --dir "$PROJECT_DIR" > "$OUTPUT_DIR/run.log" 2>&1 \
    || { cat "$OUTPUT_DIR/run.log" >&2; fail "tatolab start refused the project"; }

# The stream id `owned_device_encoder_<n>` of the project loads under,
# `<folder>-<function>-<hash>`: the folder and function names cast as the
# runtime casts them, and six hex digits of SHA-256 over
# `<resolved project directory>\n<module:qualname>`.
stream_id_of_encoder_stream() {
    "$VENV_PYTHON" -c '
import hashlib, pathlib, sys
from tatolab.stream._exposed_name_cast import cast_exposed_name_to_url_safe
project_directory = pathlib.Path(sys.argv[1]).resolve()
function_name = "owned_device_encoder_" + sys.argv[2]
location_hash = hashlib.sha256(
    f"{project_directory}\nstream:{function_name}".encode()
).hexdigest()[:6]
print("-".join([
    cast_exposed_name_to_url_safe(project_directory.name),
    cast_exposed_name_to_url_safe(function_name),
    location_hash,
]))
' "$PROJECT_DIR" "$1"
}

declare -A STREAM_ID_OF_ENCODER
for stream_number in $(seq 1 "$STREAM_COUNT"); do
    for _ in $(seq 1 360); do
        stream_id="$(stream_id_of_encoder_stream "$stream_number")" \
            || fail "could not compute the stream id of owned_device_encoder_$stream_number"
        if the_stream_is_running "$stream_id"; then
            STREAM_ID_OF_ENCODER[$stream_number]="$stream_id"
            break
        fi
        sleep 0.5
    done
    [ -n "${STREAM_ID_OF_ENCODER[$stream_number]:-}" ] || {
        "$TATOLAB_EXECUTABLE" graph > "$OUTPUT_DIR/graph_at_failure.json" 2>&1
        tail -60 "$LOG_FILE" >&2
        "$TATOLAB_EXECUTABLE" streams >> "$OUTPUT_DIR/graph_at_failure.json" 2>&1
        fail "the stream $stream_id never ran (graph and streams in $OUTPUT_DIR/graph_at_failure.json)"
    }
    say "Running:           ${STREAM_ID_OF_ENCODER[$stream_number]}"
done
stream_name_of() { echo "${STREAM_ID_OF_ENCODER[$1]}"; }

frames_written_by() {
    written_messages_of_the_output_port "$(stream_name_of "$1")" "encoder_$1" encoded_video
}

wait_until_every_encoder_writes() {
    local stream_number written
    for _ in $(seq 1 60); do
        local every_one_writes=1
        for stream_number in $(seq 1 "$STREAM_COUNT"); do
            written="$(frames_written_by "$stream_number")" || written=0
            [ "$written" -gt 0 ] 2>/dev/null || every_one_writes=0
        done
        [ "$every_one_writes" -eq 1 ] && return 0
        sleep 1
    done
    return 1
}
wait_until_every_encoder_writes || {
    tail -60 "$LOG_FILE" >&2
    fail "not every encoder wrote a frame within 60 s"
}

# The runtime's `helper process started: pid=<pid>` line names each encoder's
# helper beside its node.
helper_pid_of_encoder() {
    grep -F "encoder_$1" "$LOG_FILE" | grep -oE "helper process started: pid=[0-9]+" \
        | tail -1 | grep -oE '[0-9]+$'
}

count_device_lost_lines() { grep -cF "DEVICE_LOST" "$LOG_FILE" 2>/dev/null || true; }

if [ "$MODE" = "isolation" ]; then
    KILLED_PID="$(helper_pid_of_encoder 1)"
    SURVIVING_PID="$(helper_pid_of_encoder 2)"
    [ -n "$KILLED_PID" ] && [ -n "$SURVIVING_PID" ] \
        || fail "the runtime's log names no helper pid for each encoder"
    say "Encoder helpers:   encoder_1 pid=$KILLED_PID, encoder_2 pid=$SURVIVING_PID"

    kill -9 "$KILLED_PID" || fail "could not SIGKILL encoder_1's helper"
    say "SIGKILL sent to encoder_1's helper"
    for _ in $(seq 1 20); do
        grep -qF "its helper process (pid=$KILLED_PID) died" "$LOG_FILE" && break
        sleep 0.5
    done
    grep -qF "its helper process (pid=$KILLED_PID) died" "$LOG_FILE" \
        || { tail -40 "$LOG_FILE" >&2; fail "the runtime never noticed encoder_1's helper died"; }

    SURVIVING_BEFORE="$(frames_written_by 2)" || fail "the second stream's graph did not answer"
    KILLED_BEFORE="$(frames_written_by 1)" || KILLED_BEFORE="gone"
    sleep 5
    SURVIVING_AFTER="$(frames_written_by 2)" || fail "the second stream's graph did not answer after the kill"
    KILLED_AFTER="$(frames_written_by 1)" || KILLED_AFTER="gone"
    say "encoder_2 wrote:   $SURVIVING_BEFORE → $SURVIVING_AFTER over 5 s after the kill"
    say "encoder_1 wrote:   $KILLED_BEFORE → $KILLED_AFTER over the same 5 s"
    [ "$SURVIVING_AFTER" -gt "$SURVIVING_BEFORE" ] \
        || fail "the second stream's encoder stopped writing when the first's helper was killed"
    kill -0 "$SURVIVING_PID" 2>/dev/null || fail "the second stream's encoder helper died with the first"
    a_runtime_answers_at_the_local_api_socket || fail "the runtime stopped answering"
    [ "$(count_device_lost_lines)" -eq 0 ] || fail "DEVICE_LOST reached the runtime's log"

    cd "$REPO_ROOT" || fail "cannot enter $REPO_ROOT"
    require_tatolab_read_track
    cargo build --release --locked -p xtask > "$OUTPUT_DIR/build.log" 2>&1 \
        || { tail -40 "$OUTPUT_DIR/build.log" >&2; fail "xtask build failed"; }
    ENCODED_TRACK_DIR="$OUTPUT_DIR/encoded_track"
    read_the_track_at_the_fixture_relay "owned-device-encoder-2.hang" \
        "encoded_video" "$OBJECT_COUNT" "$ENCODED_TRACK_DIR" \
        2> "$OUTPUT_DIR/read_track.log" \
        || { cat "$OUTPUT_DIR/read_track.log" >&2; fail "the surviving encoder's track did not read"; }
    DECODED_PNG_COUNT="$(decode_the_encoded_track_dump_to_pngs "$ENCODED_TRACK_DIR" h264 \
            "$OUTPUT_DIR/encoded.h264" "$OUTPUT_DIR/measured" "$OUTPUT_DIR/ffmpeg.log")" \
        || { cat "$OUTPUT_DIR/ffmpeg.log" >&2; fail "ffmpeg could not decode the surviving track"; }
    [ "$DECODED_PNG_COUNT" -eq "$OBJECT_COUNT" ] \
        || fail "ffmpeg decoded $DECODED_PNG_COUNT of $OBJECT_COUNT frames"
    "$REPO_ROOT/target/release/xtask" psnr bar-colors \
        --images "$OUTPUT_DIR/measured" --baseline "$ENGINE_H264_BASELINE_TSV" \
        --tolerance "$TOLERANCE" --report "$OUTPUT_DIR/bar_colors.tsv" \
        || fail "the module's H.264 does not hold the engine's test-pattern drift lock"
    say "PASS: killing one encoder's helper ended only its stream; the other's bitstream holds the lock"
    exit 0
fi

# ── cost ─────────────────────────────────────────────────────────────
command -v nvidia-smi >/dev/null || say "nvidia-smi absent: GPU memory per helper is not reported"
sleep 3
declare -A WRITTEN_AT_START
for stream_number in $(seq 1 "$STREAM_COUNT"); do
    WRITTEN_AT_START[$stream_number]="$(frames_written_by "$stream_number")"
done
ENCODER_UTILISATION_SAMPLES="$OUTPUT_DIR/encoder_utilisation.csv"
if command -v nvidia-smi >/dev/null; then
    nvidia-smi --query-gpu=utilization.encoder,utilization.gpu --format=csv,noheader,nounits \
        -lms 500 > "$ENCODER_UTILISATION_SAMPLES" 2>/dev/null &
    UTILISATION_SAMPLER_PID=$!
fi
sleep "$MEASURE_SECONDS"
[ -n "${UTILISATION_SAMPLER_PID:-}" ] && kill "$UTILISATION_SAMPLER_PID" 2>/dev/null
nvidia-smi --query-compute-apps=pid,used_memory --format=csv,noheader,nounits \
    > "$OUTPUT_DIR/compute_apps.csv" 2>/dev/null || true

echo ""
printf '%-10s %-8s %-14s %-16s %-14s %-10s\n' encoder pid gpu_memory_mib device_created_ms session_minted_ms fps
for stream_number in $(seq 1 "$STREAM_COUNT"); do
    helper_pid="$(helper_pid_of_encoder "$stream_number")"
    written_at_end="$(frames_written_by "$stream_number")"
    fps="$(python3 -c "print(f'{($written_at_end - ${WRITTEN_AT_START[$stream_number]}) / $MEASURE_SECONDS:.1f}')")"
    gpu_memory="$(awk -F', ' -v pid="$helper_pid" '$1 == pid {print $2}' "$OUTPUT_DIR/compute_apps.csv" 2>/dev/null)"
    device_created_ms="$(grep -F "encoder_$stream_number" "$LOG_FILE" | grep -oE 'owned_device_created_ms=[0-9.]+' | tail -1 | cut -d= -f2)"
    session_minted_ms="$(grep -F "encoder_$stream_number" "$LOG_FILE" | grep -oE 'session_minted_ms=[0-9.]+' | tail -1 | cut -d= -f2)"
    printf '%-10s %-8s %-14s %-16s %-14s %-10s\n' "encoder_$stream_number" "$helper_pid" \
        "${gpu_memory:-?}" "${device_created_ms:-?}" "${session_minted_ms:-?}" "$fps"
done
if [ -s "$ENCODER_UTILISATION_SAMPLES" ]; then
    python3 - "$ENCODER_UTILISATION_SAMPLES" <<'PYTHON'
import sys
rows = [line.split(",") for line in open(sys.argv[1]) if line.strip()]
encoder = [float(row[0]) for row in rows]
gpu = [float(row[1]) for row in rows]
print(f"\nencoder engine utilisation: mean {sum(encoder)/len(encoder):.1f}% "
      f"(max {max(encoder):.0f}%), GPU {sum(gpu)/len(gpu):.1f}%, {len(rows)} samples")
PYTHON
fi
say "Runtime process GPU memory: $(awk -F', ' -v pid="$FIXTURE_RUNTIME_PID" '$1 == pid {print $2}' "$OUTPUT_DIR/compute_apps.csv" 2>/dev/null) MiB"
say "Log gates: DEVICE_LOST $(count_device_lost_lines), OUT_OF_DEVICE_MEMORY $(grep -cF OUT_OF_DEVICE_MEMORY "$LOG_FILE" || true)"
say "Output dir:        $OUTPUT_DIR"

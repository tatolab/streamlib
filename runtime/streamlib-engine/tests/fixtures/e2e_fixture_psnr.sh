#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Fixture-driven encode/decode PSNR harness (issues #305, #2085).
#
# Drives the engine-owned `codec_roundtrip_rig` — fixture source -> encoder ->
# decoder -> DisplayWindow, with the control plane hosted — and scores the
# decoded frames with `cargo xtask psnr score`.
#
# Scoring rides observation, not display side effects. The rig is tapped for
# bags on the decoded channel and each sampled surface id is exchanged for that
# frame's exact pixels over the control plane's bytes route; the graph is
# unchanged by being watched, and no window is in the measurement path.
#
# One rig run per reference, and that is the pairing contract. A decoded bag
# carries nothing to pair on — `sequence_index` is an encoded-frame field, and
# a decoded frame is an ordinary video frame — so pairing a decoded sample to
# the reference that produced it either needs a join this wire does not carry,
# or a best-match search that a channel swap would satisfy by re-pairing a
# swapped red onto `solid_blue.png`. Running one reference at a time makes the
# pairing exact by construction, and each run is a cold start, which is the
# shape #756 and #335 ask for anyway.
#
# Usage:
#   runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh [output_dir] [codec]
#
# Arguments:
#   output_dir — defaults to /tmp/streamlib-fixture-psnr-<timestamp>
#   codec      — h264 (default) or h265.
#
# This harness does NOT gate the H.265 CTU crop. `xtask psnr score` crops each
# decode to its reference extent before comparing, and the conformance window's
# origin is (0, 0), so a decoder publishing the padded 1088-tall picture scores
# byte-identically to one publishing the windowed 1080. What gates the crop is
# the extent assertion in
# `cargo test -p streamlib-media-builtins --test h265_decoder_completes_the_round_trip`,
# run on the rig. This harness gates colour and codec quality: Y against the
# 35/30 dB bands, and either chroma plane under 30 dB failing outright.
#
# Environment overrides:
#   SAMPLES_PER_REFERENCE — decoded frames exchanged per reference (default 2)
#   RUN_SECONDS           — per-reference rig budget (default 40)
#   REFERENCE_STEMS       — space-separated subset of reference names to run
#                            (default: every PNG in the checked-in set)
#   PSNR_INJECT_BUG       — post-decode bug injection; verifies the FAIL
#                            threshold trips for colour-management
#                            regressions. One of:
#                              swap-channels  — R<->B channel swap
#                              bt601-bt709    — matrix mis-interpretation
#                              range-swap     — PC<->TV range mis-interpretation
#                              swap-chroma    — Cb<->Cr transposition, the mode
#                                               the chroma floor exists for.
#                                               Only solid_red and solid_green
#                                               are luma-passing under it —
#                                               out-of-gamut clamping drags Y
#                                               down on complex_pattern and
#                                               solid_blue — so a whole-set run
#                                               would fail with the chroma floor
#                                               deleted. Narrow with
#                                               REFERENCE_STEMS="solid_red
#                                               solid_green" to exercise the
#                                               chroma floor specifically.
#                            Unknown values exit non-zero (no silent no-op);
#                            the list is `cargo xtask psnr score --help`.
#
# Exit codes: 0 = pass (every reference at or above the warn threshold),
#             1 = fail, 77 = skip (prerequisite missing).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
# shellcheck source=fixture_runtime_unit.sh
. "$SCRIPT_DIR/fixture_runtime_unit.sh"
REFERENCES_DIR="$SCRIPT_DIR/psnr"

OUTPUT_DIR="${1:-/tmp/streamlib-fixture-psnr-$(date +%s)}"
CODEC="${2:-h264}"

SAMPLES_PER_REFERENCE="${SAMPLES_PER_REFERENCE:-2}"
RUN_SECONDS="${RUN_SECONDS:-40}"
PSNR_INJECT_BUG="${PSNR_INJECT_BUG:-}"

# ── Prerequisites ────────────────────────────────────────────────────
need() { command -v "$1" >/dev/null || { echo "[psnr] missing: $1" >&2; exit 77; }; }
need cargo
need python3

case "$CODEC" in
    h264|h265) ;;
    *)
        echo "[psnr] FAIL: codec '$CODEC' is neither h264 nor h265" >&2
        exit 1
        ;;
esac

# The rig is read through the runtime unit's `tatolab` observation verbs (see
# fixture_runtime_unit.sh). It serves this user's local API socket itself, so
# each arm refuses to start while a runtime already answers there.
require_the_runtime_unit
require_the_fixture_venv

if [ ! -d "$REFERENCES_DIR" ]; then
    echo "[psnr] SKIP: reference set not found at $REFERENCES_DIR" >&2
    exit 77
fi

mapfile -t ALL_REFERENCE_PNGS < <(ls "$REFERENCES_DIR"/*.png 2>/dev/null | sort)
if [ "${#ALL_REFERENCE_PNGS[@]}" -eq 0 ]; then
    echo "[psnr] SKIP: no reference PNGs in $REFERENCES_DIR" >&2
    exit 77
fi

REFERENCE_PNGS=()
if [ -n "${REFERENCE_STEMS:-}" ]; then
    for stem in $REFERENCE_STEMS; do
        if [ ! -f "$REFERENCES_DIR/$stem.png" ]; then
            echo "[psnr] FAIL: REFERENCE_STEMS names $stem, which is not in $REFERENCES_DIR" >&2
            exit 1
        fi
        REFERENCE_PNGS+=("$REFERENCES_DIR/$stem.png")
    done
else
    REFERENCE_PNGS=("${ALL_REFERENCE_PNGS[@]}")
fi

DECODED_DIR="$OUTPUT_DIR/decoded"
ARMS_DIR="$OUTPUT_DIR/arms"
# The references this run actually drove, staged so the scorer's
# every-reference-was-sampled check covers the run rather than the whole
# checked-in set — a narrowed REFERENCE_STEMS must not read as seven
# regressions.
SCORED_REFERENCES_DIR="$OUTPUT_DIR/references"
mkdir -p "$DECODED_DIR" "$ARMS_DIR" "$SCORED_REFERENCES_DIR"

# A channel is the port's address, `<runtime_name>/<node>/<port>`, with this
# runtime's own top-level `runtime_name`. Read off the live graph rather than
# guessed.
decoded_channel_of_running_rig() {
    tatolab_observation_verb graph --stream "$STREAM_NAME" 2>/dev/null | python3 -c '
import json, sys
graph = json.load(sys.stdin)
decoder = next(
    (node for node in graph.get("nodes", []) if node.get("name") == "decoder"), None
)
if decoder is None:
    sys.exit("the running graph has no node named `decoder`")
print(graph["runtime_name"] + "/" + decoder["name"] + "/video")
'
}

echo "[psnr] Output dir:   $OUTPUT_DIR"
echo "[psnr] Codec:        $CODEC"
echo "[psnr] References:   ${#REFERENCE_PNGS[@]} (one cold rig run each, ${SAMPLES_PER_REFERENCE} sample(s) per run)"

# ── Build ────────────────────────────────────────────────────────────
cd "$REPO_ROOT"
echo "[psnr] Building codec_roundtrip_rig + xtask (release)..."
if ! cargo build --release --locked -p streamlib-engine --example codec_roundtrip_rig \
        > "$OUTPUT_DIR/build.log" 2>&1; then
    echo "[psnr] FAIL: rig build failed" >&2
    tail -40 "$OUTPUT_DIR/build.log" >&2
    exit 1
fi
if ! cargo build --release --locked -p xtask >> "$OUTPUT_DIR/build.log" 2>&1; then
    echo "[psnr] FAIL: xtask build failed" >&2
    tail -40 "$OUTPUT_DIR/build.log" >&2
    exit 1
fi
RIG_BINARY="$REPO_ROOT/target/release/examples/codec_roundtrip_rig"
XTASK_BINARY="$REPO_ROOT/target/release/xtask"

# Ask the tool about the injection mode before spending rig time. The reference
# set scored against itself is a perfect round trip, so an injected run of it
# must fail — which catches a mode that has gone vacuous as well as a typo. A
# mode the tool does not define is refused before it scores anything, and the
# absent report is how that is told apart from a mode that scored and failed.
if [ -n "$PSNR_INJECT_BUG" ]; then
    preflight_log="$OUTPUT_DIR/injection_preflight.log"
    preflight_report="$OUTPUT_DIR/injection_preflight.tsv"
    preflight_passed=0
    "$XTASK_BINARY" psnr score \
        --decoded "$REFERENCES_DIR" \
        --reference "$REFERENCES_DIR" \
        --inject "$PSNR_INJECT_BUG" \
        --report "$preflight_report" > "$preflight_log" 2>&1 || preflight_passed=1
    if [ ! -s "$preflight_report" ]; then
        echo "[psnr] FAIL: PSNR_INJECT_BUG=$PSNR_INJECT_BUG is not a mode the scorer defines" >&2
        cat "$preflight_log" >&2
        exit 1
    fi
    if [ "$preflight_passed" -eq 0 ]; then
        echo "[psnr] FAIL: PSNR_INJECT_BUG=$PSNR_INJECT_BUG left the reference set at or above" >&2
        echo "[psnr] the floor — the mode is vacuous and would pass a run carrying it" >&2
        exit 1
    fi
    echo "[psnr] Injection pre-flight: $PSNR_INJECT_BUG trips the gate on the reference set"
fi

RIG_PID=""
RIG_NEEDED_SIGKILL=0
stop_rig() {
    RIG_NEEDED_SIGKILL=0
    if [ -n "$RIG_PID" ] && kill -0 "$RIG_PID" 2>/dev/null; then
        # SIGTERM so the graph tears down the way a real stop does — a killed
        # rig would hide exactly the shutdown race #335 is about.
        kill -TERM "$RIG_PID" 2>/dev/null || true
        for _ in $(seq 1 50); do
            kill -0 "$RIG_PID" 2>/dev/null || break
            sleep 0.2
        done
        # A rig still alive after 10 s of SIGTERM is the #335 shutdown race, not
        # a slow exit. SIGKILL frees the GPU for the next arm; the caller is
        # told, so the kill can never launder a hung teardown into a PASS.
        if kill -0 "$RIG_PID" 2>/dev/null; then
            RIG_NEEDED_SIGKILL=1
            kill -9 "$RIG_PID" 2>/dev/null || true
        fi
        wait "$RIG_PID" 2>/dev/null || true
    fi
    RIG_PID=""
}
trap stop_rig EXIT

# Wait for the launched rig's stream to answer a graph round trip over the
# local API socket, which is the first moment a tap can attach. Sets
# STREAM_NAME to the name the rig loaded it under.
wait_for_the_launched_rig() {
    STREAM_NAME="$(name_of_the_stream_a_rig_serves_once_it_answers "$1" 30)" || {
        STREAM_NAME=""
        return 1
    }
}

# ── One cold run per reference ───────────────────────────────────────
for reference_png in "${REFERENCE_PNGS[@]}"; do
    stem="$(basename "$reference_png" .png)"
    arm_dir="$ARMS_DIR/$stem"
    mkdir -p "$arm_dir/fixtures"
    cp "$reference_png" "$arm_dir/fixtures/$stem.png"
    cp "$reference_png" "$SCORED_REFERENCES_DIR/$stem.png"
    pipeline_log="$arm_dir/pipeline.log"

    echo "[psnr] --- $stem ---"
    refuse_while_a_runtime_answers_at_the_local_api_socket || exit 1
    DISPLAY="${DISPLAY:-:0}" \
    RUST_LOG="${RUST_LOG:-warn,streamlib=info,streamlib_media_builtins=info}" \
        timeout --kill-after=5 "$RUN_SECONDS" "$RIG_BINARY" \
            --source fixture \
            --codec "$CODEC" \
            --fixtures "$arm_dir/fixtures" \
            > "$pipeline_log" 2>&1 &
    RIG_PID=$!

    if ! wait_for_the_launched_rig "$RIG_PID"; then
        echo "[psnr] FAIL: $stem — the rig never answered over its local API socket" >&2
        tail -30 "$pipeline_log" >&2
        stop_rig
        exit 1
    fi

    # The exchange itself is the readiness probe: it retries across tap rounds
    # while the encoder mints its session and the decoder waits for the first
    # sync point, and returns short (non-zero) if the channel never produced.
    decoded_channel="$(decoded_channel_of_running_rig)" || {
        echo "[psnr] FAIL: $stem — could not read the decoder channel off the live graph" >&2
        tail -30 "$pipeline_log" >&2
        stop_rig
        exit 1
    }
    echo "[psnr]     decoded channel: $decoded_channel"

    exchange_log="$arm_dir/exchange.log"
    if ! tatolab_observation_verb exchange \
            --channel "$decoded_channel" \
            --out "$arm_dir/exchanged" \
            --count "$SAMPLES_PER_REFERENCE" \
            --stream "$STREAM_NAME" \
            > "$arm_dir/exchanged_paths.txt" 2> "$exchange_log"; then
        echo "[psnr] FAIL: $stem — exchanged fewer frames than asked for" >&2
        cat "$exchange_log" >&2
        tail -30 "$pipeline_log" >&2
        stop_rig
        exit 1
    fi

    # Read the printed paths, not the directory: --out is not cleared, so a
    # listing can hand back an earlier arm's frames.
    sample_index=0
    while read -r exchanged_png; do
        [ -f "$exchanged_png" ] || continue
        cp "$exchanged_png" "$DECODED_DIR/${stem}__${sample_index}.png"
        sample_index=$(( sample_index + 1 ))
    done < "$arm_dir/exchanged_paths.txt"

    if [ "$sample_index" -ne "$SAMPLES_PER_REFERENCE" ]; then
        echo "[psnr] FAIL: $stem — copied $sample_index of $SAMPLES_PER_REFERENCE" \
             "exchanged frames; scoring fewer samples than the run asked for would" \
             "report a thinner gate as a full one" >&2
        cat "$exchange_log" >&2
        stop_rig
        exit 1
    fi
    echo "[psnr]     $sample_index frame(s) exchanged"
    cat "$exchange_log"

    stop_rig
    if [ "$RIG_NEEDED_SIGKILL" -eq 1 ]; then
        echo "[psnr] FAIL: $stem — the rig did not exit on SIGTERM and needed SIGKILL." \
             "A teardown that hangs is the #335 race class, not a slow exit." >&2
        tail -30 "$pipeline_log" >&2
        exit 1
    fi
    # The GPU has to release the encode/decode sessions before the next arm.
    sleep 2
done

# ── Score ────────────────────────────────────────────────────────────
SCORE_ARGUMENTS=(
    psnr score
    --decoded "$DECODED_DIR"
    --reference "$SCORED_REFERENCES_DIR"
    --report "$OUTPUT_DIR/psnr_report.tsv"
)
if [ -n "$PSNR_INJECT_BUG" ]; then
    SCORE_ARGUMENTS+=(--inject "$PSNR_INJECT_BUG")
fi

echo ""
if "$XTASK_BINARY" "${SCORE_ARGUMENTS[@]}"; then
    echo "[psnr] Output dir:   $OUTPUT_DIR"
    echo "[psnr] RESULT: PASS"
    exit 0
fi
echo "[psnr] Output dir:   $OUTPUT_DIR"
echo "[psnr] Report TSV:   $OUTPUT_DIR/psnr_report.tsv"
echo "[psnr] RESULT: FAIL"
exit 1

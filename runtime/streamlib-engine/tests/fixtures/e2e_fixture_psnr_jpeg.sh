#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Fixture-driven JPEG decode PSNR harness.
#
# Mirrors runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh so a
# reviewer can read both rigs with the same mental model. Differences
# (per the JPEG pipeline's shape):
#
#   - JPEG is self-contained per-frame and `JpegBytesSource` reads a
#     single file at setup (its schema's `file_path` is a required
#     scalar string, not an array). The rig therefore invokes the
#     `jpeg-psnr` binary once per reference rather than running a
#     single binary across a concatenated BGRA stream and pairing
#     decoded PNGs to references by `frame_index`. Per-invocation
#     overhead is small (~3s per reference at the configured FPS),
#     and the per-reference output subdirs match the rig's
#     middle-sample selection cleanly.
#   - ffmpeg performs the PNG → JPEG encode step (the "encoder" half of
#     the PSNR comparison); the streamlib pipeline owns the decode half.
#
# 1. For each reference PNG in runtime/streamlib-engine/tests/fixtures/psnr/:
#    a. Convert PNG → JPEG (rgba → yuvj420p, configurable quality).
#    b. Run the `jpeg-psnr` example (JpegBytesSource → JpegDecoder →
#       Display with PNG sampling).
# 2. Pick a middle-of-run decoded PNG and compute PSNR vs the reference.
# 3. Classify per the /verify-live skill: Y ≥ 35 dB pass, 30–35 dB warn,
#    < 30 dB fail.
#
# Usage:
#   runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_jpeg.sh [output_dir]
#
# Arguments:
#   output_dir — defaults to /tmp/streamlib-fixture-psnr-jpeg-<timestamp>
#
# Environment overrides:
#   JPEG_QUALITY       — encode quality 1–100 (default 70). Default is a
#                        known-good baseline that lands every reference
#                        well above the Y PSNR ≥ 35 dB pass bar; the rig
#                        runs cleanly at q=95 too. The historical q ≤ 70
#                        cap (msgpack-array wire expansion past iceoryx2's
#                        64 KiB per-slot default on `complex_pattern`) is
#                        no longer in effect — JTD codegen now emits
#                        `#[serde(with = "serde_bytes")]` on
#                        `EncodedJpegFrame.data` so the payload rides
#                        msgpack `bin` (1× wire footprint) instead of an
#                        array of integers.
#   FIXTURE_REPS       — frames per reference (default 10)
#   PNG_SAMPLE_EVERY   — displayed-frame sampling interval (default 2)
#   WIDTH / HEIGHT     — frame dimensions (default 1920x1080, must
#                        match the checked-in fixture PNGs)
#   FPS                — JpegBytesSource republish rate (default 10)
#   PSNR_INJECT_BUG    — post-decode bug injection; verifies the FAIL
#                        threshold trips for color-management regressions.
#                        One of:
#                          swap-channels  — R↔B channel swap (catches
#                                           plane-order regressions)
#                          bt601-bt709    — encode→YUV as bt601, decode→
#                                           RGB as bt709 (real matrix
#                                           mis-interpretation)
#                          range-swap     — encode→YUV as PC range, decode
#                                           as TV range (range expansion
#                                           mis-interpretation)
#                        Unknown values exit non-zero (no silent no-op).
#
# Exit codes: 0 = pass (all refs ≥ warn threshold), 1 = fail, 77 = skip.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
FIXTURES_DIR="$SCRIPT_DIR/psnr"

OUTPUT_DIR="${1:-/tmp/streamlib-fixture-psnr-jpeg-$(date +%s)}"

JPEG_QUALITY="${JPEG_QUALITY:-70}"
FIXTURE_REPS="${FIXTURE_REPS:-10}"
PNG_SAMPLE_EVERY="${PNG_SAMPLE_EVERY:-2}"
WIDTH="${WIDTH:-1920}"
HEIGHT="${HEIGHT:-1080}"
FPS="${FPS:-10}"
PSNR_INJECT_BUG="${PSNR_INJECT_BUG:-}"

# Thresholds (dB, Y channel).
PSNR_PASS_DB=35
PSNR_WARN_DB=30

# ── Prerequisites ────────────────────────────────────────────────────
need() { command -v "$1" >/dev/null || { echo "[psnr-jpeg] missing: $1" >&2; exit 77; }; }
need cargo
need ffmpeg
need awk

if [ ! -d "$FIXTURES_DIR" ]; then
    echo "[psnr-jpeg] SKIP: fixtures dir not found at $FIXTURES_DIR" >&2
    exit 77
fi

mapfile -t REF_PNGS < <(ls "$FIXTURES_DIR"/*.png 2>/dev/null | sort)
if [ "${#REF_PNGS[@]}" -eq 0 ]; then
    echo "[psnr-jpeg] SKIP: no fixture PNGs in $FIXTURES_DIR" >&2
    exit 77
fi

mkdir -p "$OUTPUT_DIR"
JPEG_DIR="$OUTPUT_DIR/encoded_jpegs"
PNG_DIR="$OUTPUT_DIR/png_samples"
REF_DIR="$OUTPUT_DIR/refs"
LOG_DIR="$OUTPUT_DIR/logs"
mkdir -p "$JPEG_DIR" "$PNG_DIR" "$REF_DIR" "$LOG_DIR"

echo "[psnr-jpeg] Output dir:    $OUTPUT_DIR"
echo "[psnr-jpeg] Fixtures:      ${#REF_PNGS[@]} references at ${WIDTH}x${HEIGHT} @ ${FPS}fps × ${FIXTURE_REPS} reps"
echo "[psnr-jpeg] JPEG quality:  $JPEG_QUALITY"
[ -n "$PSNR_INJECT_BUG" ] && echo "[psnr-jpeg] Bug injection: $PSNR_INJECT_BUG"

# Why q=70 as default
# -------------------
# A known-good baseline that clears the Y PSNR ≥ 35 dB pass bar on
# every reference (`complex_pattern` measured at Y=39 dB on initial
# rig validation, Y=42 dB at q=95). Bump `JPEG_QUALITY` higher for
# stricter regression detection; the wire path no longer caps the
# rig at q=70 (`EncodedJpegFrame.data` rides msgpack `bin` rather
# than an integer array, so iceoryx2's 64 KiB per-slot default
# easily accommodates `complex_pattern` up to q=100).
#
# ffmpeg's -q:v scale for libmjpeg is ~2..31 (lower = higher quality).
# The mapping below sends q=70 → -q:v 13, q=85 → -q:v 9, q=95 → -q:v 7.
QSCALE="$(awk -v q="$JPEG_QUALITY" 'BEGIN{
    if (q < 1) q = 1;
    if (q > 100) q = 100;
    v = 31 - int(q * 0.26);
    if (v < 2) v = 2;
    if (v > 31) v = 31;
    print v
}')"

# ── Build ────────────────────────────────────────────────────────────
cd "$REPO_ROOT"
echo "[psnr-jpeg] Building jpeg-psnr..."
if ! cargo build -p jpeg-psnr 2>"$LOG_DIR/build.log"; then
    echo "[psnr-jpeg] FAIL: build failed"
    tail -40 "$LOG_DIR/build.log"
    exit 1
fi
BINARY="$REPO_ROOT/target/debug/jpeg-psnr"

cleanup() { pkill -9 -f "$BINARY" 2>/dev/null || true; }
trap cleanup EXIT

# ── Per-reference: encode JPEG, run pipeline, capture PNG ─────────────
# Total samples per reference at FPS=10, REPS=10, every=2: ~5 samples.
# Run window per reference is `(REPS / FPS) + 3` seconds — matches the
# example binary's internal sleep envelope.
RUN_SECS=$(( FIXTURE_REPS / FPS + 3 ))

for i in "${!REF_PNGS[@]}"; do
    src_png="${REF_PNGS[$i]}"
    name="$(basename "$src_png" .png)"
    idx="$(printf "%02d" "$i")"

    # Reference at working dimensions / pixel format. Same shape as the
    # sibling rig — forces rgba so monochrome-encoded source PNGs don't
    # skew the YUV conversion vs the decoded RGBA-sourced samples.
    ref_png="$REF_DIR/${idx}_${name}.png"
    ffmpeg -y -hide_banner -loglevel error \
        -i "$src_png" -vf "scale=${WIDTH}:${HEIGHT},format=rgba" "$ref_png"

    # PNG → JPEG via ffmpeg's libmjpeg encoder. yuvj420p is the JFIF
    # baseline (full-range YCbCr 4:2:0) the decoder is built around;
    # using yuvj420p (rather than yuv420p) keeps the encoder honest
    # about JFIF's full-range convention.
    jpeg_path="$JPEG_DIR/${idx}_${name}.jpg"
    ffmpeg -y -hide_banner -loglevel error \
        -i "$ref_png" -vf "format=yuvj420p" -q:v "$QSCALE" "$jpeg_path"

    ref_png_dir="$PNG_DIR/${idx}_${name}"
    mkdir -p "$ref_png_dir"

    log_file="$LOG_DIR/${idx}_${name}.log"
    STREAMLIB_DISPLAY_PNG_SAMPLE_DIR="$ref_png_dir" \
    STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY="$PNG_SAMPLE_EVERY" \
    STREAMLIB_DISPLAY_FRAME_LIMIT="$FIXTURE_REPS" \
    DISPLAY="${DISPLAY:-:0}" \
    RUST_LOG=warn,streamlib=info \
    timeout --kill-after=3 "$RUN_SECS" "$BINARY" \
        "$jpeg_path" "$WIDTH" "$HEIGHT" "$FPS" "$FIXTURE_REPS" \
        > "$log_file" 2>&1 || true
done

# ── Optional bug injection ───────────────────────────────────────────
# Post-process every decoded sample with a specific color-management
# regression class before PSNR comparison. Each variant is expected to
# drop Y PSNR below the FAIL threshold on the references that carry
# the affected channels — proving the rig flags real quality
# regressions, not just smoke-test runs.
if [ -n "$PSNR_INJECT_BUG" ]; then
    case "$PSNR_INJECT_BUG" in
        swap-channels)
            echo "[psnr-jpeg] INJECTING BUG: swapping R↔B on decoded samples"
            inject_filter="colorchannelmixer=rr=0:rb=1:bb=0:br=1:gg=1"
            ;;
        bt601-bt709)
            echo "[psnr-jpeg] INJECTING BUG: BT.601→BT.709 matrix mis-interpretation"
            inject_filter="scale=out_color_matrix=bt601:flags=accurate_rnd,format=yuv420p,scale=in_color_matrix=bt709:flags=accurate_rnd,format=rgba"
            ;;
        range-swap)
            echo "[psnr-jpeg] INJECTING BUG: PC→TV range mis-interpretation"
            inject_filter="scale=out_range=pc:flags=accurate_rnd,format=yuv420p,scale=in_range=tv:flags=accurate_rnd,format=rgba"
            ;;
        *)
            echo "[psnr-jpeg] ERROR: unknown PSNR_INJECT_BUG=$PSNR_INJECT_BUG" >&2
            echo "[psnr-jpeg] valid: swap-channels | bt601-bt709 | range-swap" >&2
            exit 1
            ;;
    esac

    # Iterate per-reference subdir and glob each — matches the sibling
    # rig's whitespace-safe glob idiom (rather than the path-splitting
    # `for f in $(find ...)` form).
    for i in "${!REF_PNGS[@]}"; do
        name="$(basename "${REF_PNGS[$i]}" .png)"
        idx="$(printf "%02d" "$i")"
        for f in "$PNG_DIR/${idx}_${name}"/display_*_frame_*.png; do
            [ -f "$f" ] || continue
            ffmpeg -y -hide_banner -loglevel error -i "$f" \
                -vf "$inject_filter" \
                "${f}.injected.png"
            mv "${f}.injected.png" "$f"
        done
    done
fi

# ── PSNR per reference ───────────────────────────────────────────────
echo ""
echo "══════════════════════════════════════════════════════════════"
echo "  Fixture PSNR results (jpeg)"
echo "══════════════════════════════════════════════════════════════"
printf "  %-28s  %8s  %8s  %8s   %s\n" "reference" "Y(dB)" "U(dB)" "V(dB)" "verdict"
echo "  ───────────────────────────────────────────────────────────"

OVERALL_PASS=true
REPORT_TSV="$OUTPUT_DIR/psnr_report.tsv"
echo -e "reference\tinput_frame\ty_db\tu_db\tv_db\tverdict" > "$REPORT_TSV"

for i in "${!REF_PNGS[@]}"; do
    src_png="${REF_PNGS[$i]}"
    name="$(basename "$src_png" .png)"
    idx="$(printf "%02d" "$i")"
    ref_png="$REF_DIR/${idx}_${name}.png"
    ref_png_dir="$PNG_DIR/${idx}_${name}"

    # Pick a middle-of-run sample. Trim first and last to avoid display
    # mailbox boundary effects (the input-frame number embedded in the
    # filename rises as the source emits, so middle = stable steady
    # state).
    mapfile -t samples < <(ls "$ref_png_dir"/display_*_frame_*.png 2>/dev/null | sort)
    if [ "${#samples[@]}" -eq 0 ]; then
        printf "  %-28s  %8s  %8s  %8s   %s\n" "$name" "n/a" "n/a" "n/a" "NO-SAMPLE"
        echo -e "$name\t-\tn/a\tn/a\tn/a\tNO-SAMPLE" >> "$REPORT_TSV"
        OVERALL_PASS=false
        continue
    fi

    # Pick the middle sample, trimming endpoints when we have enough.
    if [ "${#samples[@]}" -ge 3 ]; then
        mid_index=$(( ${#samples[@]} / 2 ))
        decoded_png="${samples[$mid_index]}"
    else
        decoded_png="${samples[0]}"
    fi
    decoded_idx="$(basename "$decoded_png" .png | sed -E 's/.*_input_([0-9]+)$/\1/' | awk '{print int($0)}')"

    psnr_log="$OUTPUT_DIR/psnr_${name}.log"
    # Convert both sides to yuv420p (same matrix + range) before PSNR
    # so we report y/u/v values comparable across runs. `setparams`
    # pins color_range=pc so ffmpeg doesn't guess different defaults
    # per input. No `crop` step is needed for JPEG (no CTU padding).
    ffmpeg -y -hide_banner \
        -i "$decoded_png" -i "$ref_png" \
        -lavfi "[0:v]format=rgba,setparams=range=pc,format=yuv420p[a];[1:v]format=rgba,setparams=range=pc,format=yuv420p[b];[a][b]psnr=stats_file=$OUTPUT_DIR/psnr_${name}_stats.log" \
        -f null - > "$psnr_log" 2>&1 || true

    line="$(grep -oE 'PSNR y:[0-9.inf]+ u:[0-9.inf]+ v:[0-9.inf]+' "$psnr_log" | tail -1 || true)"
    y_db="$(echo "$line" | sed -nE 's/.*y:([0-9.]+|inf).*/\1/p')"
    u_db="$(echo "$line" | sed -nE 's/.*u:([0-9.]+|inf).*/\1/p')"
    v_db="$(echo "$line" | sed -nE 's/.*v:([0-9.]+|inf).*/\1/p')"
    [ -z "$y_db" ] && y_db="nan"
    [ -z "$u_db" ] && u_db="nan"
    [ -z "$v_db" ] && v_db="nan"

    verdict="FAIL"
    if [ "$y_db" = "inf" ]; then
        verdict="PASS"
    elif [ "$y_db" != "nan" ]; then
        ge_pass="$(awk -v a="$y_db" -v b="$PSNR_PASS_DB" 'BEGIN{print (a+0 >= b+0) ? 1 : 0}')"
        ge_warn="$(awk -v a="$y_db" -v b="$PSNR_WARN_DB" 'BEGIN{print (a+0 >= b+0) ? 1 : 0}')"
        if [ "$ge_pass" = "1" ]; then
            verdict="PASS"
        elif [ "$ge_warn" = "1" ]; then
            verdict="WARN"
        else
            verdict="FAIL"
        fi
    fi
    if [ "$verdict" = "FAIL" ] || [ "$verdict" = "NO-SAMPLE" ]; then
        OVERALL_PASS=false
    fi

    printf "  %-28s  %8s  %8s  %8s   %s\n" "$name" "$y_db" "$u_db" "$v_db" "$verdict"
    echo -e "$name\t$decoded_idx\t$y_db\t$u_db\t$v_db\t$verdict" >> "$REPORT_TSV"
done

echo "══════════════════════════════════════════════════════════════"
echo "  Output dir:        $OUTPUT_DIR"
echo "  Report TSV:        $REPORT_TSV"
echo "  Pipeline logs:     $LOG_DIR/"
echo "══════════════════════════════════════════════════════════════"

if [ "$OVERALL_PASS" = true ]; then
    echo "[psnr-jpeg] RESULT: PASS"
    exit 0
else
    echo "[psnr-jpeg] RESULT: FAIL"
    exit 1
fi

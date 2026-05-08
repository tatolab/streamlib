#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Fixture-driven encode/decode PSNR harness (issue #305).
#
# 1. Converts every reference PNG in libs/streamlib/tests/fixtures/psnr/
#    to raw BGRA, replicating each frame REPS_PER times so the encoder
#    crosses at least one GOP boundary per reference.
# 2. Runs the `vulkan-video-psnr` example (BgraFileSource → encoder →
#    decoder → display with PNG sampling).
# 3. Pairs each reference with the decoded PNG carrying the matching
#    input-frame index (threaded through the pipeline via
#    `Encodedvideoframe::frame_number` → `Videoframe::frame_index`).
# 4. Runs ffmpeg's psnr filter on each pair and classifies the result
#    per docs/testing.md: Y ≥ 35 dB pass, 30–35 dB warn, < 30 dB fail.
#
# Usage:
#   libs/streamlib/tests/fixtures/e2e_fixture_psnr.sh [output_dir] [codec]
#
# Arguments:
#   output_dir — defaults to /tmp/streamlib-fixture-psnr-<timestamp>
#   codec      — h264 (default) or h265
#
# Environment overrides:
#   FIXTURE_REPS       — frames per reference (default 15)
#   PNG_SAMPLE_EVERY   — displayed-frame sampling interval (default 3)
#   WIDTH / HEIGHT     — frame dimensions (default 1920x1080, must
#                         match the checked-in fixture PNGs)
#   FPS                — BgraFileSource playback fps (default 30)
#   PSNR_INJECT_BUG    — if set to "color-matrix", swaps BT.601↔BT.709
#                         at decode time to verify the FAIL threshold trips
#
# Exit codes: 0 = pass (all refs ≥ warn threshold), 1 = fail, 77 = skip.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
FIXTURES_DIR="$SCRIPT_DIR/psnr"

OUTPUT_DIR="${1:-/tmp/streamlib-fixture-psnr-$(date +%s)}"
CODEC="${2:-h264}"

# Defaults tuned so each reference is stable for ~1.5s while still
# producing enough PNG samples for the middle-of-range selection.
# Higher FPS (30) causes the display's skip_to_latest input mailbox to
# drop frames near reference boundaries; 10fps keeps the decoder ↔
# display step-locked.
FIXTURE_REPS="${FIXTURE_REPS:-15}"
PNG_SAMPLE_EVERY="${PNG_SAMPLE_EVERY:-1}"
WIDTH="${WIDTH:-1920}"
HEIGHT="${HEIGHT:-1080}"
FPS="${FPS:-10}"
PSNR_INJECT_BUG="${PSNR_INJECT_BUG:-}"

# Thresholds (dB, Y channel).
PSNR_PASS_DB=35
PSNR_WARN_DB=30

# ── Prerequisites ────────────────────────────────────────────────────
need() { command -v "$1" >/dev/null || { echo "[psnr] missing: $1" >&2; exit 77; }; }
need cargo
need ffmpeg
need awk

if [ ! -d "$FIXTURES_DIR" ]; then
    echo "[psnr] SKIP: fixtures dir not found at $FIXTURES_DIR" >&2
    exit 77
fi

mapfile -t REF_PNGS < <(ls "$FIXTURES_DIR"/*.png 2>/dev/null | sort)
if [ "${#REF_PNGS[@]}" -eq 0 ]; then
    echo "[psnr] SKIP: no fixture PNGs in $FIXTURES_DIR" >&2
    exit 77
fi

mkdir -p "$OUTPUT_DIR"
BGRA_FILE="$OUTPUT_DIR/fixtures.bgra"
PNG_DIR="$OUTPUT_DIR/png_samples"
REF_DIR="$OUTPUT_DIR/refs"
LOG_FILE="$OUTPUT_DIR/pipeline.log"
mkdir -p "$PNG_DIR" "$REF_DIR"

echo "[psnr] Output dir:  $OUTPUT_DIR"
echo "[psnr] Codec:       $CODEC"
echo "[psnr] Fixtures:    ${#REF_PNGS[@]} × $FIXTURE_REPS reps at ${WIDTH}x${HEIGHT} @ ${FPS}fps"

# ── Build BGRA file and keep a per-reference "decoded-looking" ref PNG ─
: > "$BGRA_FILE"
TOTAL_FRAMES=0
for i in "${!REF_PNGS[@]}"; do
    src_png="${REF_PNGS[$i]}"
    name="$(basename "$src_png" .png)"
    # Convert PNG → raw RGBA bytes at the working resolution. Despite
    # BgraFileSource's name, the downstream encoder reads the pixel
    # buffer as R8G8B8A8 (byte 0 = R), and the display PNG writer emits
    # RGBA ordering verbatim — so feeding RGBA in keeps channel order
    # consistent end-to-end.
    # Force rgba pix_fmt: some solid-color PNGs from ImageMagick are
    # encoded monochrome (pix_fmt=monob), which skews YUV conversion
    # vs the decoded RGBA-sourced PNGs.
    ref_png="$REF_DIR/$(printf "%02d" "$i")_${name}.png"
    ffmpeg -y -hide_banner -loglevel error \
        -i "$src_png" -vf "scale=${WIDTH}:${HEIGHT},format=rgba" "$ref_png"

    # Append FIXTURE_REPS copies of the raw RGBA payload. Stride is
    # width*4 with no padding for the rawvideo muxer, so simple
    # concatenation is safe.
    tmp_bgra="$OUTPUT_DIR/.tmp_${name}.bgra"
    ffmpeg -y -hide_banner -loglevel error \
        -i "$ref_png" -f rawvideo -pix_fmt rgba "$tmp_bgra"
    for _ in $(seq 1 "$FIXTURE_REPS"); do
        cat "$tmp_bgra" >> "$BGRA_FILE"
    done
    rm -f "$tmp_bgra"
    TOTAL_FRAMES=$(( TOTAL_FRAMES + FIXTURE_REPS ))
done
BGRA_SIZE="$(stat -c %s "$BGRA_FILE")"
EXPECTED_SIZE=$(( WIDTH * HEIGHT * 4 * TOTAL_FRAMES ))
if [ "$BGRA_SIZE" -ne "$EXPECTED_SIZE" ]; then
    echo "[psnr] FAIL: BGRA file size $BGRA_SIZE ≠ expected $EXPECTED_SIZE"
    exit 1
fi
echo "[psnr] BGRA fixture: $BGRA_FILE ($BGRA_SIZE bytes, $TOTAL_FRAMES frames)"

# ── Build the example ────────────────────────────────────────────────
cd "$REPO_ROOT"
echo "[psnr] Building vulkan-video-psnr..."
if ! cargo build -p vulkan-video-psnr 2>"$OUTPUT_DIR/build.log"; then
    echo "[psnr] FAIL: build failed"
    tail -40 "$OUTPUT_DIR/build.log"
    exit 1
fi
BINARY="$REPO_ROOT/target/debug/vulkan-video-psnr"

# ── Run ─────────────────────────────────────────────────────────────
cleanup() { pkill -9 -f vulkan-video-psnr 2>/dev/null || true; }
trap cleanup EXIT

RUN_SECS=$(( TOTAL_FRAMES / FPS + 8 ))
echo "[psnr] Running pipeline for ~${RUN_SECS}s (reps=$FIXTURE_REPS, every=$PNG_SAMPLE_EVERY)..."

STREAMLIB_DISPLAY_PNG_SAMPLE_DIR="$PNG_DIR" \
STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY="$PNG_SAMPLE_EVERY" \
STREAMLIB_DISPLAY_FRAME_LIMIT="$TOTAL_FRAMES" \
DISPLAY="${DISPLAY:-:0}" \
RUST_LOG=warn,streamlib=info,vulkan_video=info \
timeout --kill-after=3 "$RUN_SECS" "$BINARY" \
    "$CODEC" "$BGRA_FILE" "$WIDTH" "$HEIGHT" "$FPS" "$TOTAL_FRAMES" \
    > "$LOG_FILE" 2>&1 || true

SAMPLE_COUNT="$(ls -1 "$PNG_DIR"/display_*_frame_*_input_*.png 2>/dev/null | wc -l)"
echo "[psnr] PNG samples captured: $SAMPLE_COUNT"
if [ "$SAMPLE_COUNT" -eq 0 ]; then
    echo "[psnr] FAIL: no PNG samples produced"
    echo "[psnr] Last 30 lines of pipeline log:"
    tail -30 "$LOG_FILE"
    exit 1
fi

# Optional bug-injection: swap R↔B on every decoded sample before
# comparing. Simulates a BT.601 ↔ BT.709 style color-space regression
# at decode time and should drop Y PSNR below the FAIL threshold on
# every non-gray reference, proving the rig flags quality regressions.
if [ "$PSNR_INJECT_BUG" = "color-matrix" ]; then
    echo "[psnr] INJECTING BUG: swapping R↔B on decoded samples"
    for f in "$PNG_DIR"/display_*_frame_*_input_*.png; do
        ffmpeg -y -hide_banner -loglevel error -i "$f" \
            -vf "colorchannelmixer=rr=0:rb=1:bb=0:br=1:gg=1" \
            "${f}.swap.png"
        mv "${f}.swap.png" "$f"
    done
fi

# ── PSNR per reference ───────────────────────────────────────────────
echo ""
echo "══════════════════════════════════════════════════════════════"
echo "  Fixture PSNR results ($CODEC)"
echo "══════════════════════════════════════════════════════════════"
printf "  %-28s  %8s  %8s  %8s   %s\n" "reference" "Y(dB)" "U(dB)" "V(dB)" "verdict"
echo "  ───────────────────────────────────────────────────────────"

OVERALL_PASS=true
REPORT_TSV="$OUTPUT_DIR/psnr_report.tsv"
echo -e "reference\tinput_frame\ty_db\tu_db\tv_db\tverdict" > "$REPORT_TSV"

for i in "${!REF_PNGS[@]}"; do
    name="$(basename "${REF_PNGS[$i]}" .png)"
    # Trim the FIRST and LAST frame of each reference range — display
    # mailbox is `skip_to_latest` with a small buffer, so samples right
    # at a scene change can contain content from the adjacent
    # reference. Interior frames have unambiguous content and reliably
    # pair with the reference.
    start=$(( i * FIXTURE_REPS + 1 ))
    end=$(( (i + 1) * FIXTURE_REPS - 2 ))
    mid=$(( (start + end) / 2 ))

    # Find the sample whose input-frame index is nearest to the middle
    # of this reference's range.
    decoded_png=""
    decoded_idx=""
    best_distance=-1
    for f in $(ls "$PNG_DIR"/display_*_frame_*_input_*.png 2>/dev/null | sort); do
        idx="$(basename "$f" .png | sed -E 's/.*_input_([0-9]+)$/\1/' | awk '{print int($0)}')"
        if [ "$idx" -ge "$start" ] && [ "$idx" -le "$end" ]; then
            distance=$(( idx - mid ))
            if [ "$distance" -lt 0 ]; then distance=$(( -distance )); fi
            if [ "$best_distance" -eq -1 ] || [ "$distance" -lt "$best_distance" ]; then
                decoded_png="$f"
                decoded_idx="$idx"
                best_distance="$distance"
            fi
        fi
    done

    if [ -z "$decoded_png" ]; then
        printf "  %-28s  %8s  %8s  %8s   %s\n" "$name" "n/a" "n/a" "n/a" "NO-SAMPLE"
        echo -e "$name\t-\tn/a\tn/a\tn/a\tNO-SAMPLE" >> "$REPORT_TSV"
        OVERALL_PASS=false
        continue
    fi

    ref_png="$REF_DIR/$(printf "%02d" "$i")_${name}.png"
    psnr_log="$OUTPUT_DIR/psnr_${name}.log"
    # Convert both sides to yuv420p (same matrix + range) before PSNR
    # so we report y/u/v values comparable across runs. `setparams`
    # pins color_range=pc so ffmpeg doesn't guess different defaults
    # per input.
    # Crop the decoded side to WIDTH x HEIGHT — some H.265 encoders pad
    # height to CTU boundaries (e.g. 1080 → 1088). The padded rows
    # contain garbage that shouldn't influence PSNR.
    ffmpeg -y -hide_banner \
        -i "$decoded_png" -i "$ref_png" \
        -lavfi "[0:v]crop=${WIDTH}:${HEIGHT}:0:0,format=rgba,setparams=range=pc,format=yuv420p[a];[1:v]format=rgba,setparams=range=pc,format=yuv420p[b];[a][b]psnr=stats_file=$OUTPUT_DIR/psnr_${name}_stats.log" \
        -f null - > "$psnr_log" 2>&1 || true

    # ffmpeg logs "[Parsed_psnr_*] PSNR y:XX.YY u:YY.YY v:...".
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
echo "  Pipeline log:      $LOG_FILE"
echo "══════════════════════════════════════════════════════════════"

if [ "$OVERALL_PASS" = true ]; then
    echo "[psnr] RESULT: PASS"
    exit 0
else
    echo "[psnr] RESULT: FAIL"
    echo "[psnr] Last 30 lines of pipeline log:"
    tail -30 "$LOG_FILE"
    exit 1
fi

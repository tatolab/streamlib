#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Regenerate the reference PNG fixture set used by the encoder/decoder
# PSNR rig (issue #305). Idempotent — existing files are overwritten.
#
# Usage: libs/streamlib/tests/fixtures/psnr/generate_fixtures.sh

set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
W=1920
H=1080

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "Missing dependency: $1" >&2
        exit 1
    }
}
need convert
need ffmpeg

echo "Writing fixtures to $DIR"

# Solid colors — catch color-matrix (BT.601 vs BT.709) and full/limited-range
# bugs. Primaries are far-from-gray and stress chroma channels.
convert -size "${W}x${H}" "xc:#000000" "$DIR/solid_black.png"
convert -size "${W}x${H}" "xc:#ffffff" "$DIR/solid_white.png"
convert -size "${W}x${H}" "xc:#808080" "$DIR/solid_gray.png"
convert -size "${W}x${H}" "xc:#ff0000" "$DIR/solid_red.png"
convert -size "${W}x${H}" "xc:#00ff00" "$DIR/solid_green.png"
convert -size "${W}x${H}" "xc:#0000ff" "$DIR/solid_blue.png"

# Linear gradients — catch plane-stride, chroma-subsampling, and
# off-by-one plane-offset bugs. Smooth ramps amplify any quantization
# mismatch.
convert -size "${W}x${H}" gradient:black-white "$DIR/gradient_horizontal.png"
convert -size "${H}x${W}" gradient:black-white -rotate 90 "$DIR/gradient_vertical.png"

# Complex synthetic pattern — ffmpeg's testsrc2 (color bars, rainbow,
# text, frame counter, small geometry). Stand-in for a "natural
# photograph"; checked into git as a deterministic PNG so the rig
# doesn't drift as ffmpeg changes its generator.
ffmpeg -y -hide_banner -loglevel error \
    -f lavfi -i "testsrc2=size=${W}x${H}:rate=1:duration=1" \
    -frames:v 1 "$DIR/complex_pattern.png"

echo "Done. Fixture sizes:"
du -h "$DIR"/*.png

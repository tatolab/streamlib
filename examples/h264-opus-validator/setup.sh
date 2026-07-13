#!/usr/bin/env bash
# One-shot local dev setup for this standalone example.
#
# This validator depends only on public crates (webrtc, opus, hyper, ...) and
# shells out to `ffmpeg`/`ffprobe` — it does NOT depend on the streamlib SDK or
# any @tatolab/* package, so there is nothing to link and no processor package
# to install. `cargo run` builds and runs it directly.
set -euo pipefail
cd "$(dirname "$0")"

if ! command -v ffmpeg >/dev/null 2>&1; then
    echo "warning: ffmpeg not found on PATH — this validator shells out to ffmpeg/ffprobe." >&2
fi
if ! command -v ffprobe >/dev/null 2>&1; then
    echo "warning: ffprobe not found on PATH — this validator shells out to ffmpeg/ffprobe." >&2
fi

echo "No linking required — this example has no streamlib dependency."
echo "Run it with:"
echo "    cargo run"

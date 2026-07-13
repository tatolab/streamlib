#!/usr/bin/env bash
# One-shot local dev setup for this standalone polyglot streamlib example.
#
#   ./setup.sh                                  # link against the in-repo checkout (../..)
#   STREAMLIB_CHECKOUT=/path/to/streamlib ./setup.sh
#
# After it runs, `cargo run -- --runtime=python` (or `--runtime=deno`) builds
# against the local SDK and the runtime finds every processor package in
# ./streamlib_modules/. Reverse the SDK link with `streamlib unlink --engine`.
set -euo pipefail
cd "$(dirname "$0")"

CHECKOUT="${STREAMLIB_CHECKOUT:-../..}"
CHECKOUT="$(cd "$CHECKOUT" && pwd)"

STREAMLIB="${STREAMLIB_BIN:-streamlib}"
if ! command -v "$STREAMLIB" >/dev/null 2>&1; then
    STREAMLIB="$CHECKOUT/target/debug/streamlib"
    if [ ! -x "$STREAMLIB" ]; then
        echo "Building the streamlib CLI from $CHECKOUT ..."
        cargo build --manifest-path "$CHECKOUT/Cargo.toml" -p streamlib-cli
    fi
fi

echo "streamlib CLI: $STREAMLIB"
echo "checkout:      $CHECKOUT"

# 1. SDK (Rust + Python + Deno): point every streamlib SDK surface at the local
#    checkout via [patch.crates-io] / uv source / deno import-map overrides.
"$STREAMLIB" link --engine "$CHECKOUT"

# `link --engine` writes the cargo [patch.crates-io], but build-time schema
# codegen (build.rs JTD codegen + the runtime orchestrator's package builds)
# resolves schema deps via STREAMLIB_LINK_CHECKOUT — a separate pointer. Publish
# it via the .cargo [env] table so `./setup.sh && cargo run` is one step.
CARGO_CFG=.cargo/config.toml
if ! grep -q "STREAMLIB_LINK_CHECKOUT" "$CARGO_CFG" 2>/dev/null; then
    printf '\n[env]\nSTREAMLIB_LINK_CHECKOUT = { value = "%s", force = true }\n' "$CHECKOUT" >> "$CARGO_CFG"
fi

# 2. Packages into ./streamlib_modules/:
#    - the in-repo BgraFileSource trigger source, and
#    - this example's own Python + Deno polyglot processor packages.
"$STREAMLIB" link "$CHECKOUT/packages/debug-utilities"
"$STREAMLIB" link ./python
"$STREAMLIB" link ./deno

echo
echo "Setup complete. Run the example with:"
echo "    cargo run -- --runtime=python --output=/tmp/cpu-readback-blur-py.png"
echo "    cargo run -- --runtime=deno   --output=/tmp/cpu-readback-blur-deno.png"

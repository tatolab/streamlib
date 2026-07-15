#!/usr/bin/env bash
# One-shot local dev setup for this standalone streamlib example.
#
#   ./setup.sh                                  # link against the in-repo checkout (../..)
#   STREAMLIB_CHECKOUT=/path/to/streamlib ./setup.sh
#
# After it runs, `cargo run` builds against the local SDK and the runtime finds
# every processor package in ./streamlib_modules/. This is the npm equivalent of
# `npm install` for a linked workspace.
#
# Reverse it with: streamlib unlink --engine  (SDK) and  streamlib unlink @tatolab/<pkg>
set -euo pipefail
cd "$(dirname "$0")"

# A streamlib monorepo checkout that provides the SDK + the in-repo processor
# packages. Defaults to the repo this example ships inside.
CHECKOUT="${STREAMLIB_CHECKOUT:-../..}"
CHECKOUT="$(cd "$CHECKOUT" && pwd)"

# Resolve the `streamlib` CLI: prefer one on PATH, else the checkout's build.
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

# 1. SDK: point this app's `streamlib = "0.6"` dep at the local checkout via a
#    transient [patch.crates-io] (removed by `streamlib unlink --engine`). Once
#    the SDK publishes, drop this step and the bare version resolves directly.
"$STREAMLIB" link --engine "$CHECKOUT"

# `link --engine` writes the cargo [patch.crates-io], but the runtime
# orchestrator's build of each linked package resolves its schema deps via
# STREAMLIB_LINK_CHECKOUT — a separate build-time pointer. Publish it to every
# `cargo build`/`cargo run` in this dir through the .cargo [env] table so
# `./setup.sh && cargo run` is genuinely one step.
CARGO_CFG=.cargo/config.toml
if ! grep -q "STREAMLIB_LINK_CHECKOUT" "$CARGO_CFG" 2>/dev/null; then
    printf '\n[env]\nSTREAMLIB_LINK_CHECKOUT = { value = "%s", force = true }\n' "$CHECKOUT" >> "$CARGO_CFG"
fi

# 2. Packages: symlink the in-repo processor packages this example uses into
#    ./streamlib_modules/. Live edits in the checkout reflect on the next run.
"$STREAMLIB" link "$CHECKOUT/packages/camera"
"$STREAMLIB" link "$CHECKOUT/packages/display"

echo
echo "Setup complete. Run the example with:"
echo "    cargo run"

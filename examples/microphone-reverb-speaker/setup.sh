#!/usr/bin/env bash
# One-shot local dev setup for this standalone streamlib example.
#
#   ./setup.sh                                  # link against the in-repo checkout (../..)
#   STREAMLIB_CHECKOUT=/path/to/streamlib ./setup.sh
#
# This example is a deferred no-op (see README.md and src/main.rs), so it loads
# no processor packages — setup only links the SDK so `cargo run` builds against
# the local checkout. Reverse it with: streamlib unlink --engine
set -euo pipefail
cd "$(dirname "$0")"

# A streamlib monorepo checkout that provides the SDK. Defaults to the repo this
# example ships inside.
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

# SDK: point this app's `streamlib = "0.6"` dep at the local checkout via a
# transient [patch.crates-io] (removed by `streamlib unlink --engine`). Once the
# SDK publishes, drop this step and the bare version resolves directly.
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

echo
echo "Setup complete. Run the example with:"
echo "    cargo run"

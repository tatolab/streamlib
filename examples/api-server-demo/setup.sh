#!/usr/bin/env bash
# One-shot local dev setup for this standalone streamlib example.
#
#   ./setup.sh                                  # build/link against the in-repo checkout (../..)
#   STREAMLIB_CHECKOUT=/path/to/streamlib ./setup.sh
#
# The api-server is the runtime's control plane — statically linked into
# `streamlib-runtime`, not a loadable plugin — so this demo drives a real
# runtime subprocess over HTTP + WebSocket. After setup, `cargo run` spawns that
# runtime (its path is recorded below), waits for /health, and exercises the
# control plane. The runtime resolves `SimplePassthroughProcessor` from the
# linked `@tatolab/debug-utilities` in ./streamlib_modules/.
#
# Reverse it with: streamlib unlink @tatolab/debug-utilities
set -euo pipefail
cd "$(dirname "$0")"

# A streamlib monorepo checkout that provides the runtime binary + the in-repo
# processor packages. Defaults to the repo this example ships inside.
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

# 1. Runtime binary: this demo spawns `streamlib-runtime` as a subprocess.
echo "Building streamlib-runtime from $CHECKOUT ..."
cargo build --manifest-path "$CHECKOUT/Cargo.toml" -p streamlib-runtime
RUNTIME_BIN="$CHECKOUT/target/debug/streamlib-runtime"

# 2. Record the runtime binary path (and the checkout the runtime uses to build
#    linked packages from source) into this dir's .cargo [env] table so
#    `cargo run` finds them and passes them to the spawned runtime. Publishing
#    STREAMLIB_LINK_CHECKOUT lets the runtime resolve the linked package's
#    schema deps against the local checkout on first-load build.
CARGO_CFG=.cargo/config.toml
mkdir -p .cargo
if ! grep -q "STREAMLIB_RUNTIME_BIN" "$CARGO_CFG" 2>/dev/null; then
    {
        printf '\n[env]\n'
        printf 'STREAMLIB_RUNTIME_BIN = { value = "%s", force = true }\n' "$RUNTIME_BIN"
        printf 'STREAMLIB_LINK_CHECKOUT = { value = "%s", force = true }\n' "$CHECKOUT"
    } >> "$CARGO_CFG"
fi

# 3. Packages: symlink `@tatolab/debug-utilities` into ./streamlib_modules/ so
#    the runtime can resolve `SimplePassthroughProcessor` (created through the
#    control plane's dynamic-registry POST). Live edits reflect on the next run.
"$STREAMLIB" link "$CHECKOUT/packages/debug-utilities"

echo
echo "Setup complete. Run the example with:"
echo "    cargo run"

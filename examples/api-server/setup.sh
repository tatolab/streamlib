#!/usr/bin/env bash
# One-shot setup for this standalone streamlib example.
#
#   ./setup.sh                                  # build the runtime from ../..
#   STREAMLIB_CHECKOUT=/path/to/streamlib ./setup.sh
#
# The api-server is the runtime's HTTP + WebSocket control plane — statically
# linked into `streamlib-runtime` and served in-process, not a loadable module.
# This example is a dependency-free client for that control plane, so there is
# nothing to link: it builds on its own with `cargo build`. setup.sh only builds
# the `streamlib-runtime` binary you point the probe at.
set -euo pipefail
cd "$(dirname "$0")"

# A streamlib monorepo checkout that provides the runtime binary. Defaults to
# the repo this example ships inside.
CHECKOUT="${STREAMLIB_CHECKOUT:-../..}"
CHECKOUT="$(cd "$CHECKOUT" && pwd)"

echo "Building streamlib-runtime from $CHECKOUT ..."
cargo build --manifest-path "$CHECKOUT/Cargo.toml" -p streamlib-runtime

echo
echo "Setup complete. In one terminal start the runtime (serves the control"
echo "plane on http://127.0.0.1:9000):"
echo "    $CHECKOUT/target/debug/streamlib-runtime"
echo "then in this directory run the probe:"
echo "    cargo run"

#!/usr/bin/env bash
# Stage-1 (builder) orchestration for the multi-stage GPU Docker distribution.
#
# Runs inside the `builder` image (full toolchain, GPU-free). It:
#   1. builds the streamlib CLI + streamlib-runtime binaries in-place (the
#      vulkanalia fork is vendored at vendor/tatolab-vulkanalia* and resolves by
#      path — no package-source bootstrap of any kind),
#   2. emits the image-local static package-source tree (`.slpkg` generic store +
#      catalog + release manifest) via `cargo xtask static-package-source emit`,
#   3. assembles the /opt/streamlib app dir (binaries + package source),
#   4. writes the runtime npm config pointing at the image-local tree.
#
# There is NO daemon and NO cargo registry: engine / SDK crate deps in a package
# resolve to the local checkout via `streamlib link` ([patch.crates-io] path
# overrides), never a fetched registry. The RUNTIME `.slpkg` + pypi trees are
# plain files the entrypoint reads over `file://`; the entrypoint additionally
# re-serves the tree over a dumb `python3 -m http.server` mount for the npm
# (Deno SDK) face. The final stage COPYs /opt/streamlib (carrying the tree) +
# /root/.npmrc and docker/entrypoint.sh re-serves the mount at runtime.
# See docs/architecture/package-source.md.
#
# Configure-by-env (Dockerfile ARGs -> ENV): SRC, APP_DIR, PACKAGE_SOURCE_DIR,
# PACKAGE_SOURCE_PORT.
set -euo pipefail

SRC="${SRC:-/src}"
APP_DIR="${APP_DIR:-/opt/streamlib}"
PACKAGE_SOURCE_DIR="${PACKAGE_SOURCE_DIR:-${APP_DIR}/package-source}"
PACKAGE_SOURCE_PORT="${PACKAGE_SOURCE_PORT:-8799}"

log()  { printf '\n[stage1] %s\n' "$*"; }
fail() { printf '\n[stage1] ERROR: %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# 1. Build the streamlib CLI + runtime binaries in-place. The vulkanalia fork
#    is vendored at vendor/tatolab-vulkanalia* and resolves by path — no
#    package-source bootstrap or [source] replacement is needed for this build.
# ---------------------------------------------------------------------------
log "building streamlib CLI + runtime (release)"
( cd "$SRC" && cargo build --release -p streamlib-cli -p streamlib-runtime )
[ -x "$SRC/target/release/streamlib" ] || fail "streamlib CLI not built"
[ -x "$SRC/target/release/streamlib-runtime" ] || fail "streamlib-runtime not built"

# ---------------------------------------------------------------------------
# 2. Emit the image-local static package-source tree: the `.slpkg` generic
#    store + catalog + release manifest (written last, the atomicity flip).
# ---------------------------------------------------------------------------
log "emitting static package-source tree at $PACKAGE_SOURCE_DIR"
( cd "$SRC" && cargo run --release -q -p xtask -- static-package-source emit --out "$PACKAGE_SOURCE_DIR" )
[ -d "$PACKAGE_SOURCE_DIR/slpkg" ] || fail "static package-source emit did not produce a slpkg/ store"

# ---------------------------------------------------------------------------
# 3. Assemble the /opt/streamlib app dir (binaries + package source). packages/
#    source is required by the runtime's Path{IfStale} boot of api-server (the
#    staleness check reads the source) and lets in-process add_module resolve
#    any in-tree package by path. PACKAGE_SOURCE_DIR already lives under $APP_DIR.
# ---------------------------------------------------------------------------
log "assembling $APP_DIR"
mkdir -p "$APP_DIR/bin"
cp "$SRC/target/release/streamlib" "$SRC/target/release/streamlib-runtime" "$APP_DIR/bin/"
cp -a "$SRC/packages" "$APP_DIR/packages"

# ---------------------------------------------------------------------------
# 4. Runtime npm package-source config (COPY'd to the final image via /root).
#    The Deno SDK npm face resolves over the localhost static mount the
#    entrypoint serves (npm is HTTP-only by spec). pypi + `.slpkg` read the same
#    tree over `file://` with no server. See docs/architecture/package-source.md.
# ---------------------------------------------------------------------------
log "writing runtime npm package-source config"
printf '@tatolab:registry=http://127.0.0.1:%s/npm/\n' "$PACKAGE_SOURCE_PORT" > /root/.npmrc

log "stage 1 complete — static package-source tree emitted at $PACKAGE_SOURCE_DIR, binaries + cache assembled at $APP_DIR"

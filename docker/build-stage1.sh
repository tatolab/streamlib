#!/usr/bin/env bash
# Stage-1 (builder) orchestration for the multi-stage GPU Docker distribution.
#
# Runs inside the `builder` image (full toolchain, GPU-free). It:
#   1. bootstraps the tatolab/vulkanalia fork as a STATIC cargo sparse tree and
#      serves it on a throwaway localhost mount, so the workspace (xtask
#      included) resolves `vulkanalia = { registry = "tatolab" }` daemon-free,
#   2. builds the streamlib CLI + streamlib-runtime binaries in-place,
#   3. emits the full image-local static registry tree (cargo closure + fork +
#      pypi + npm + `.slpkg` generic store + catalog + release manifest) via
#      `cargo xtask static-registry emit --cargo-closure`,
#   4. assembles the /opt/streamlib app dir (binaries + package source),
#   5. writes the runtime cargo `[source]`-replacement + npm config pointing at
#      the image-local tree, and runs an api-server resolution preflight.
#
# There is NO registry daemon. The tree is plain files: cargo + npm resolve over
# a dumb `python3 -m http.server` mount (sparse + npm are HTTP-only by spec);
# pypi + `.slpkg` read the same tree straight off `file://`. The final stage
# COPYs /opt/streamlib (carrying the tree) + /usr/local/cargo (carrying the
# `[source]` config) and docker/entrypoint.sh re-serves the mount at runtime.
# See docs/architecture/static-registry.md.
#
# Configure-by-env (Dockerfile ARGs -> ENV): SRC, APP_DIR, REGISTRY_DIR,
# REGISTRY_PORT, CARGO_HOME, and the SKIP_* toggles.
set -euo pipefail

SRC="${SRC:-/src}"
APP_DIR="${APP_DIR:-/opt/streamlib}"
REGISTRY_DIR="${REGISTRY_DIR:-${APP_DIR}/registry}"
REGISTRY_PORT="${REGISTRY_PORT:-8799}"
REGISTRY_BASE_URL="http://127.0.0.1:${REGISTRY_PORT}"
BOOTSTRAP_PORT="${BOOTSTRAP_PORT:-8798}"
BOOTSTRAP_BASE_URL="http://127.0.0.1:${BOOTSTRAP_PORT}"
CARGO_HOME="${CARGO_HOME:?CARGO_HOME must be set}"

# Map the fast-iteration skip toggles to the emit's per-ecosystem skip flags.
SKIP_PYTHON_SDK="${SKIP_PYTHON_SDK:-0}"   # -> --no-pypi (python SDK sdist tree)
SKIP_DENO_SDK="${SKIP_DENO_SDK:-0}"       # -> --no-npm  (deno SDK npm tree)
SKIP_PACKAGES="${SKIP_PACKAGES:-0}"       # -> --no-slpkg (.slpkg store + manifest)

log()  { printf '\n[stage1] %s\n' "$*"; }
fail() { printf '\n[stage1] ERROR: %s\n' "$*" >&2; exit 1; }

BOOTSTRAP_DIR=""
BOOTSTRAP_PID=""
PREFLIGHT_PID=""
cleanup() {
  [ -n "$BOOTSTRAP_PID" ]  && kill "$BOOTSTRAP_PID"  2>/dev/null || true
  [ -n "$PREFLIGHT_PID" ]  && kill "$PREFLIGHT_PID"  2>/dev/null || true
  [ -n "$BOOTSTRAP_DIR" ]  && rm -rf "$BOOTSTRAP_DIR" || true
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# 0. Clone the vulkanalia fork (registry dep). VULKANALIA_DIR is reused by both
#    the bootstrap fork emit and the xtask emit's internal fork step (no
#    re-clone). The pinned rev is sourced from the workspace, not hardcoded.
# ---------------------------------------------------------------------------
VULKANALIA_REV="$(grep -oE 'rev = "[0-9a-f]{40}"' "$SRC/libs/streamlib-cross-rustc-fixture/Cargo.toml" | head -1 | grep -oE '[0-9a-f]{40}')"
[ -n "$VULKANALIA_REV" ] || fail "could not derive the vulkanalia rev from the workspace"
log "cloning tatolab/vulkanalia @ $VULKANALIA_REV"
rm -rf /tmp/vulkanalia
git clone --quiet https://github.com/tatolab/vulkanalia.git /tmp/vulkanalia
git -C /tmp/vulkanalia checkout --quiet "$VULKANALIA_REV"
git -C /tmp/vulkanalia submodule update --init --quiet \
  ext/vma/vendor/Vulkan-Headers ext/vma/vendor/VulkanMemoryAllocator
export VULKANALIA_DIR=/tmp/vulkanalia

# ---------------------------------------------------------------------------
# 1. Bootstrap: emit + serve the vulkanalia fork static cargo tree so the
#    workspace (and xtask itself) resolves the `tatolab` registry daemon-free.
#    The mount stays up for the whole build stage; cargo reaches it via the
#    CARGO_REGISTRIES_TATOLAB_INDEX override. On a DIFFERENT port than the final
#    tree's base URL so the xtask emit's own fork step can bind that port.
# ---------------------------------------------------------------------------
log "bootstrapping the vulkanalia fork static cargo tree ($BOOTSTRAP_BASE_URL)"
BOOTSTRAP_DIR="$(mktemp -d)"
python3 -m http.server "$BOOTSTRAP_PORT" --bind 127.0.0.1 --directory "$BOOTSTRAP_DIR" \
  >/tmp/bootstrap-httpd.log 2>&1 &
BOOTSTRAP_PID=$!
for i in $(seq 1 30); do
  curl -fsS "$BOOTSTRAP_BASE_URL/" >/dev/null 2>&1 && break
  kill -0 "$BOOTSTRAP_PID" 2>/dev/null || { cat /tmp/bootstrap-httpd.log; fail "bootstrap static server exited"; }
  [ "$i" = 30 ] && { cat /tmp/bootstrap-httpd.log; fail "bootstrap static server did not come up on $BOOTSTRAP_BASE_URL"; }
  sleep 1
done
export CARGO_REGISTRIES_TATOLAB_INDEX="sparse+${BOOTSTRAP_BASE_URL}/cargo/"
STATIC_FORK_URL="$BOOTSTRAP_BASE_URL" \
  "$SRC/scripts/registry/emit-static-fork.sh" "$BOOTSTRAP_DIR" --base-url "$BOOTSTRAP_BASE_URL"
curl -fsS "$BOOTSTRAP_BASE_URL/cargo/config.json" >/dev/null || fail "bootstrap fork tree not fetchable"

# ---------------------------------------------------------------------------
# 2. Build the streamlib CLI + runtime binaries in-place (resolves the fork
#    from the bootstrap mount).
# ---------------------------------------------------------------------------
log "building streamlib CLI + runtime (release)"
( cd "$SRC" && cargo build --release -p streamlib-cli -p streamlib-runtime )
[ -x "$SRC/target/release/streamlib" ] || fail "streamlib CLI not built"
[ -x "$SRC/target/release/streamlib-runtime" ] || fail "streamlib-runtime not built"

# ---------------------------------------------------------------------------
# 3. Emit the full image-local static registry tree. --cargo-closure packages
#    every workspace release-closure crate; the fork + pypi + npm + `.slpkg`
#    store + catalog + release manifest (the atomicity flip) ride the same emit.
#    The emit manages its own transient sub-servers (a throwaway fork server on
#    $REGISTRY_PORT, an ephemeral closure-packaging server on a random port);
#    the bootstrap mount on $BOOTSTRAP_PORT satisfies the outer xtask build.
# ---------------------------------------------------------------------------
EMIT_ARGS=(--out "$REGISTRY_DIR" --cargo-closure --base-url "$REGISTRY_BASE_URL")
[ "$SKIP_PYTHON_SDK" = 1 ] && EMIT_ARGS+=(--no-pypi)
[ "$SKIP_DENO_SDK" = 1 ]   && EMIT_ARGS+=(--no-npm)
[ "$SKIP_PACKAGES" = 1 ]   && EMIT_ARGS+=(--no-slpkg)
log "emitting static registry tree at $REGISTRY_DIR (base $REGISTRY_BASE_URL)"
( cd "$SRC" && cargo run --release -q -p xtask -- static-registry emit "${EMIT_ARGS[@]}" )
[ -f "$REGISTRY_DIR/cargo/config.json" ] || fail "static registry emit did not produce cargo/config.json"

# ---------------------------------------------------------------------------
# 4. Stop the bootstrap mount + drop its env override. Everything below resolves
#    the `tatolab` registry through the final $CARGO_HOME/config.toml source
#    replacement against the emitted tree served on $REGISTRY_PORT.
# ---------------------------------------------------------------------------
kill "$BOOTSTRAP_PID" 2>/dev/null || true
wait "$BOOTSTRAP_PID" 2>/dev/null || true
BOOTSTRAP_PID=""
unset CARGO_REGISTRIES_TATOLAB_INDEX

# ---------------------------------------------------------------------------
# 5. Assemble the /opt/streamlib app dir (binaries + package source). packages/
#    source is required by the runtime's Path{IfStale} boot of api-server (the
#    staleness check reads the source) and lets in-process add_module resolve
#    any in-tree package by path. REGISTRY_DIR already lives under $APP_DIR.
# ---------------------------------------------------------------------------
log "assembling $APP_DIR"
mkdir -p "$APP_DIR/bin" "$APP_DIR/.streamlib/cache/packages"
cp "$SRC/target/release/streamlib" "$SRC/target/release/streamlib-runtime" "$APP_DIR/bin/"
cp -a "$SRC/packages" "$APP_DIR/packages"

# ---------------------------------------------------------------------------
# 6. Runtime registry config (COPY'd to the final image via /usr/local/cargo +
#    /root). cargo resolves the canonical `tatolab` index through a [source]
#    replacement pointing at the localhost static mount the entrypoint serves —
#    source replacement keeps the canonical id in every Cargo.lock. npm reads the
#    same mount. See docs/architecture/static-registry.md.
# ---------------------------------------------------------------------------
log "writing runtime cargo + npm registry config"
mkdir -p "$CARGO_HOME"
cat > "$CARGO_HOME/config.toml" <<EOF
[registries.tatolab]
index = "sparse+https://registry.tatolab.com/cargo/"

[source.tatolab]
registry = "sparse+https://registry.tatolab.com/cargo/"
replace-with = "tatolab-local"

[source.tatolab-local]
registry = "sparse+http://127.0.0.1:${REGISTRY_PORT}/cargo/"
EOF
printf '@tatolab:registry=http://127.0.0.1:%s/npm/\n' "$REGISTRY_PORT" > /root/.npmrc

# ---------------------------------------------------------------------------
# 7. Resolution preflight for the api-server core module. The runtime builds
#    api-server from source on first boot (build-capable image, warm cargo cache
#    -> tens of seconds); this fetch verifies its dependency graph resolves
#    against the emitted tree now, so a resolution gap fails the image build
#    instead of first boot. Serve the tree on $REGISTRY_PORT (the port the
#    runtime cargo config points at) so `cargo fetch` resolves the closure + the
#    fork exactly as the runtime will. Run in a temp copy so it doesn't pollute
#    the shipped tree.
# ---------------------------------------------------------------------------
log "preflight: api-server dependency resolution against the emitted tree"
python3 -m http.server "$REGISTRY_PORT" --bind 127.0.0.1 --directory "$REGISTRY_DIR" \
  >/tmp/preflight-httpd.log 2>&1 &
PREFLIGHT_PID=$!
for i in $(seq 1 30); do
  curl -fsS "$REGISTRY_BASE_URL/cargo/config.json" >/dev/null 2>&1 && break
  kill -0 "$PREFLIGHT_PID" 2>/dev/null || { cat /tmp/preflight-httpd.log; fail "preflight static server exited"; }
  [ "$i" = 30 ] && { cat /tmp/preflight-httpd.log; fail "preflight static server did not come up"; }
  sleep 1
done
rm -rf /tmp/apisrv-diag && cp -a "$APP_DIR/packages/api-server" /tmp/apisrv-diag
( cd /tmp/apisrv-diag && cargo fetch 2>&1 ) \
  || fail "api-server dependency resolution failed — root cause printed above"
rm -rf /tmp/apisrv-diag
kill "$PREFLIGHT_PID" 2>/dev/null || true
wait "$PREFLIGHT_PID" 2>/dev/null || true
PREFLIGHT_PID=""

log "stage 1 complete — static registry tree emitted at $REGISTRY_DIR, binaries + cache assembled at $APP_DIR"

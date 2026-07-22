#!/usr/bin/env bash
# Stage-1 (builder) orchestration for the multi-stage GPU Docker distribution.
#
# Runs inside the `builder` image (full toolchain, GPU-free). It:
#   1. builds the streamlib CLI + streamlib-runtime binaries in-place (the
#      vulkanalia fork is vendored at vendor/tatolab-vulkanalia* and resolves by
#      path — no registry bootstrap of any kind),
#   2. emits the full image-local static registry tree (cargo closure — which
#      includes the vendored tatolab-vulkanalia* crates — + pypi + npm +
#      `.slpkg` generic store + catalog + release manifest) via
#      `cargo xtask static-registry emit --cargo-closure`,
#   3. assembles the /opt/streamlib app dir (binaries + package source),
#   4. writes the runtime cargo `[source]`-replacement + npm config pointing at
#      the image-local tree, and runs an api-server resolution preflight.
#
# There is NO registry daemon. The RUNTIME tree is plain files the entrypoint
# re-serves for cargo + npm over a dumb `python3 -m http.server` mount (sparse
# + npm are HTTP-only by spec), while pypi + `.slpkg` read it straight off
# `file://`. The final stage COPYs /opt/streamlib (carrying the tree) +
# /usr/local/cargo (carrying the runtime `[source]` config) and
# docker/entrypoint.sh re-serves the mount at runtime.
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
CARGO_HOME="${CARGO_HOME:?CARGO_HOME must be set}"

# Map the fast-iteration skip toggles to the emit's per-ecosystem skip flags.
SKIP_PYTHON_SDK="${SKIP_PYTHON_SDK:-0}"   # -> --no-pypi (python SDK sdist tree)
SKIP_DENO_SDK="${SKIP_DENO_SDK:-0}"       # -> --no-npm  (deno SDK npm tree)
SKIP_PACKAGES="${SKIP_PACKAGES:-0}"       # -> --no-slpkg (.slpkg store + manifest)

log()  { printf '\n[stage1] %s\n' "$*"; }
fail() { printf '\n[stage1] ERROR: %s\n' "$*" >&2; exit 1; }

PREFLIGHT_PID=""
cleanup() {
  if [ -n "$PREFLIGHT_PID" ]; then kill "$PREFLIGHT_PID" 2>/dev/null || true; fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# 1. Build the streamlib CLI + runtime binaries in-place. The vulkanalia fork
#    is vendored at vendor/tatolab-vulkanalia* and resolves by path — no
#    registry bootstrap or [source] replacement is needed for this build.
# ---------------------------------------------------------------------------
log "building streamlib CLI + runtime (release)"
( cd "$SRC" && cargo build --release -p streamlib-cli -p streamlib-runtime )
[ -x "$SRC/target/release/streamlib" ] || fail "streamlib CLI not built"
[ -x "$SRC/target/release/streamlib-runtime" ] || fail "streamlib-runtime not built"

# ---------------------------------------------------------------------------
# 2. Emit the full image-local static registry tree. --cargo-closure packages
#    every workspace release-closure crate (including the vendored
#    tatolab-vulkanalia* crates); pypi + npm + `.slpkg` store + catalog +
#    release manifest (the atomicity flip) ride the same emit. The emit
#    manages its OWN transient sub-servers internally (its inner
#    `cargo package` points CARGO_REGISTRIES_TATOLAB_INDEX at an ephemeral
#    staging server). Any lock dirtying from that override is confined to
#    /src, which is discarded after stage 1 (never COPYed into the final
#    image) — nothing downstream resolves in it, so no lock restore is needed
#    here, unlike the CI reference which runs a post-emit `cargo test
#    --locked` in the same workspace.
# ---------------------------------------------------------------------------
EMIT_ARGS=(--out "$REGISTRY_DIR" --cargo-closure --base-url "$REGISTRY_BASE_URL")
[ "$SKIP_PYTHON_SDK" = 1 ] && EMIT_ARGS+=(--no-pypi)
[ "$SKIP_DENO_SDK" = 1 ]   && EMIT_ARGS+=(--no-npm)
[ "$SKIP_PACKAGES" = 1 ]   && EMIT_ARGS+=(--no-slpkg)
# Belt-and-suspenders: force a fresh `cargo package` for every closure crate.
# The emitter already always repackages from source (target/package is cargo
# scratch, not a trusted content cache), so this is redundant on a fresh builder
# layer — kept for parity with the CI emit and to stay correct if a build cache
# mount is ever added.
rm -rf "$SRC/target/package"
log "emitting static registry tree at $REGISTRY_DIR (base $REGISTRY_BASE_URL)"
( cd "$SRC" && cargo run --release -q -p xtask -- static-registry emit "${EMIT_ARGS[@]}" )
[ -f "$REGISTRY_DIR/cargo/config.json" ] || fail "static registry emit did not produce cargo/config.json"

# ---------------------------------------------------------------------------
# 3. Assemble the /opt/streamlib app dir (binaries + package source). packages/
#    source is required by the runtime's Path{IfStale} boot of api-server (the
#    staleness check reads the source) and lets in-process add_module resolve
#    any in-tree package by path. REGISTRY_DIR already lives under $APP_DIR.
# ---------------------------------------------------------------------------
log "assembling $APP_DIR"
mkdir -p "$APP_DIR/bin"
cp "$SRC/target/release/streamlib" "$SRC/target/release/streamlib-runtime" "$APP_DIR/bin/"
cp -a "$SRC/packages" "$APP_DIR/packages"

# ---------------------------------------------------------------------------
# 4. Runtime registry config (COPY'd to the final image via /usr/local/cargo +
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
# 5. Resolution preflight for the api-server core module. The runtime builds
#    api-server from source on first boot (build-capable image, warm cargo cache
#    -> tens of seconds); this fetch verifies its dependency graph resolves
#    against the emitted tree now, so a resolution gap fails the image build
#    instead of first boot. Serve the tree on $REGISTRY_PORT (the port the
#    runtime cargo config points at) so `cargo fetch` resolves the closure
#    (vendored tatolab-vulkanalia* included) exactly as the runtime will. Run in a temp copy so it doesn't pollute
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

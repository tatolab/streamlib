#!/usr/bin/env bash
# Stage-1 (builder) orchestration for the multi-stage GPU Docker distribution.
#
# Runs inside the `builder` image (full toolchain, GPU-free). It:
#   1. brings up an EPHEMERAL Gitea (the `git` user — Gitea refuses root),
#   2. mints an admin + token and provisions the `tatolab` org,
#   3. clones + publishes the tatolab/vulkanalia fork (registry dep),
#   4. builds the streamlib CLI + streamlib-runtime binaries in-place,
#   5. publishes the whole closure (crates / python / deno / packages) so the
#      in-container registry "matches our local build",
#   6. assembles the /opt/streamlib app dir,
#   7. pre-materializes the api-server core module into the package cache so the
#      final image boots compiler-free,
#   8. leaves /var/lib/gitea (filled DB + packages) for the final stage to COPY.
#
# Everything that touches the running Gitea must happen in this single script
# because a Docker `RUN` can't carry a background process across layers.
#
# Configure-by-env (Dockerfile ARGs -> ENV): SRC, APP_DIR, GITEA_WORK_DIR,
# GITEA_URL, GITEA_ORG, GITEA_ADMIN_USER, and the SKIP_*/PREBUILD_* toggles.
set -euo pipefail

SRC="${SRC:-/src}"
APP_DIR="${APP_DIR:-/opt/streamlib}"
GITEA_WORK_DIR="${GITEA_WORK_DIR:-/var/lib/gitea}"
GITEA_CONF="${GITEA_WORK_DIR}/conf/app.ini"
GITEA_URL="${GITEA_URL:-http://localhost:3300}"
GITEA_ORG="${GITEA_ORG:-tatolab}"
GITEA_ADMIN_USER="${GITEA_ADMIN_USER:-tatolab-admin}"
GITEA_ADMIN_PASSWORD="${GITEA_ADMIN_PASSWORD:-streamlib-build-$(head -c8 /dev/urandom | od -An -tx1 | tr -d ' \n')}"
CARGO_REGISTRY="${CARGO_REGISTRY:-gitea}"

SKIP_PYTHON_SDK="${SKIP_PYTHON_SDK:-0}"
SKIP_DENO_SDK="${SKIP_DENO_SDK:-0}"
SKIP_PACKAGES="${SKIP_PACKAGES:-0}"
PREBUILD_API_SERVER="${PREBUILD_API_SERVER:-1}"

log()  { printf '\n[stage1] %s\n' "$*"; }
fail() { printf '\n[stage1] ERROR: %s\n' "$*" >&2; exit 1; }

# HOME must point at a git-writable dir: Gitea's startup RewriteAllPublicKeys
# writes ~/.ssh, and setpriv does NOT reset the inherited HOME=/root.
as_git() { setpriv --reuid=git --regid=git --init-groups \
  env HOME="$GITEA_WORK_DIR" GITEA_WORK_DIR="$GITEA_WORK_DIR" "$@"; }
gitea_cli() { as_git /usr/local/bin/gitea --config "$GITEA_CONF" "$@"; }

# ---------------------------------------------------------------------------
# 1. Gitea config: copy the committed template, append generated secrets.
# ---------------------------------------------------------------------------
log "preparing Gitea ($GITEA_URL, work dir $GITEA_WORK_DIR)"
mkdir -p "$GITEA_WORK_DIR/conf" "$GITEA_WORK_DIR/data" "$GITEA_WORK_DIR/git/repositories"
cp "$SRC/docker/gitea/app.ini" "$GITEA_CONF"
SECRET_KEY="$(/usr/local/bin/gitea generate secret SECRET_KEY)"
INTERNAL_TOKEN="$(/usr/local/bin/gitea generate secret INTERNAL_TOKEN)"
{
  printf 'SECRET_KEY = %s\n' "$SECRET_KEY"
  printf 'INTERNAL_TOKEN = %s\n' "$INTERNAL_TOKEN"
} >> "$GITEA_CONF"
chown -R git:git "$GITEA_WORK_DIR"

# ---------------------------------------------------------------------------
# 2. Migrate DB, create admin + token BEFORE `gitea web` holds the SQLite file.
# ---------------------------------------------------------------------------
log "initializing Gitea DB + admin user"
gitea_cli migrate >/dev/null
gitea_cli admin user create \
  --username "$GITEA_ADMIN_USER" --password "$GITEA_ADMIN_PASSWORD" \
  --email "${GITEA_ADMIN_USER}@tatolab.local" --admin --must-change-password=false >/dev/null \
  || fail "admin user create failed"
TOKEN="$(gitea_cli admin user generate-access-token \
  --username "$GITEA_ADMIN_USER" --token-name docker-build --scopes all \
  | grep -oE '[0-9a-f]{40}' | tail -1)"
[ -n "$TOKEN" ] || fail "could not mint admin token"

# ---------------------------------------------------------------------------
# 3. Start Gitea (background, as git) and wait until the API answers.
# ---------------------------------------------------------------------------
log "starting Gitea web (background)"
as_git /usr/local/bin/gitea --config "$GITEA_CONF" web >/tmp/gitea.log 2>&1 &
GITEA_PID=$!
stop_gitea() { kill "$GITEA_PID" 2>/dev/null || true; wait "$GITEA_PID" 2>/dev/null || true; }
trap stop_gitea EXIT

for i in $(seq 1 60); do
  curl -fsS "$GITEA_URL/api/v1/version" >/dev/null 2>&1 && break
  kill -0 "$GITEA_PID" 2>/dev/null || { tail -40 /tmp/gitea.log; fail "Gitea exited during startup"; }
  [ "$i" = 60 ] && { tail -40 /tmp/gitea.log; fail "Gitea did not become ready in 60s"; }
  sleep 1
done
log "Gitea ready: $(curl -fsS "$GITEA_URL/api/v1/version")"

# ---------------------------------------------------------------------------
# 4. Global cargo registry config so staged out-of-tree builds (api-server)
#    resolve `registry = "gitea"` deps. In-tree builds use $SRC/.cargo too.
# ---------------------------------------------------------------------------
mkdir -p "${CARGO_HOME:?CARGO_HOME must be set}"
cat > "${CARGO_HOME}/config.toml" <<EOF
[registries.gitea]
index = "sparse+${GITEA_URL}/api/packages/${GITEA_ORG}/cargo/"
EOF

# Shared publish env consumed by scripts/gitea/publish-*.sh.
export GITEA_URL GITEA_ORG CARGO_REGISTRY
export GITEA_ADMIN_TOKEN="$TOKEN"
export GITEA_PUBLISH_TOKEN="$TOKEN"
export GITEA_PUBLISH_USER="$GITEA_ADMIN_USER"
export CARGO_REGISTRIES_GITEA_TOKEN="Bearer ${TOKEN}"
export STREAMLIB_REGISTRY_URL="$GITEA_URL"
export STREAMLIB_REGISTRY_TOKEN="$TOKEN"
export PYTHON=python3

# ---------------------------------------------------------------------------
# 5. Provision the org + verify the four registries are reachable.
# ---------------------------------------------------------------------------
log "provisioning org '$GITEA_ORG'"
"$SRC/scripts/gitea/provision-registry.sh"

# ---------------------------------------------------------------------------
# 6. Publish the vulkanalia fork (registry dep — must precede the closure).
#    The pinned rev is sourced from the workspace, not hardcoded here.
# ---------------------------------------------------------------------------
VULKANALIA_REV="$(grep -oE 'rev = "[0-9a-f]{40}"' "$SRC/libs/streamlib-cross-rustc-fixture/Cargo.toml" | head -1 | grep -oE '[0-9a-f]{40}')"
[ -n "$VULKANALIA_REV" ] || fail "could not derive the vulkanalia rev from the workspace"
log "cloning tatolab/vulkanalia @ $VULKANALIA_REV"
git clone --quiet https://github.com/tatolab/vulkanalia.git /tmp/vulkanalia
git -C /tmp/vulkanalia checkout --quiet "$VULKANALIA_REV"
git -C /tmp/vulkanalia submodule update --init --quiet \
  ext/vma/vendor/Vulkan-Headers ext/vma/vendor/VulkanMemoryAllocator
export VULKANALIA_DIR=/tmp/vulkanalia
log "publishing vulkanalia fork"
( cd "$SRC" && ./scripts/gitea/publish-vulkanalia.sh )

# ---------------------------------------------------------------------------
# 7. Build the runtime + CLI binaries in-place (resolves vulkanalia from Gitea).
# ---------------------------------------------------------------------------
log "building streamlib CLI + runtime (release)"
( cd "$SRC" && cargo build --release -p streamlib-cli -p streamlib-runtime )
export STREAMLIB_BIN="$SRC/target/release/streamlib"
[ -x "$STREAMLIB_BIN" ] || fail "streamlib CLI not built"
[ -x "$SRC/target/release/streamlib-runtime" ] || fail "streamlib-runtime not built"

# ---------------------------------------------------------------------------
# 8. Publish the SDK closure so the in-container registry matches local build.
# ---------------------------------------------------------------------------
log "publishing streamlib crate closure"
( cd "$SRC" && ./scripts/gitea/publish-crates.sh )

# Package cargo crates consumed by OTHER packages must also land in the cargo
# registry. api-server's optional `moq` feature cargo-depends on streamlib-moq
# (packages/moq); cargo requires optional deps to be resolvable in the index
# even when the feature is off, so api-server can't build at all without it.
# Publish source-only (--no-verify): the index entry makes it resolvable; the
# crate is only compiled if a consumer enables the feature.
publish_package_crate() {
  local dir="$1" name; name="$(basename "$dir")"
  [ -f "$SRC/$dir/Cargo.toml" ] || return 0
  if [ -f "$SRC/$dir/streamlib.yaml" ] && grep -q '^patch:' "$SRC/$dir/streamlib.yaml"; then
    ( cd "$SRC" && cargo run -q -p xtask -- strip-publish-manifest --dir "$dir" ) >/dev/null 2>&1 || true
  fi
  log "publishing package cargo crate: $name"
  local out
  if out="$( cd "$SRC/$dir" && cargo publish --no-verify --allow-dirty --registry "$CARGO_REGISTRY" 2>&1 )"; then
    log "  ✓ $name published"
  elif printf '%s' "$out" | grep -qiE 'already exists|already uploaded|is already'; then
    log "  • $name already present — skipping"
  else
    printf '%s\n' "$out" >&2; fail "publish of package cargo crate $name failed"
  fi
}
publish_package_crate packages/moq
if [ "$SKIP_PYTHON_SDK" != 1 ]; then
  log "publishing python SDK"; ( cd "$SRC" && ./scripts/gitea/publish-python-sdk.sh )
else log "SKIP python SDK"; fi
if [ "$SKIP_DENO_SDK" != 1 ]; then
  log "publishing deno SDK"; ( cd "$SRC" && ./scripts/gitea/publish-deno-sdk.sh )
else log "SKIP deno SDK"; fi
if [ "$SKIP_PACKAGES" != 1 ]; then
  log "publishing packages (.slpkg)"; ( cd "$SRC" && ./scripts/gitea/publish-packages.sh )
else log "SKIP packages"; fi

# ---------------------------------------------------------------------------
# 9. Assemble the /opt/streamlib app dir (binaries + package source + registry
#    client config). packages/ source is required by the runtime's Path{IfStale}
#    boot of api-server (staleness check reads the source) and lets in-process
#    add_module resolve any in-tree package by path.
# ---------------------------------------------------------------------------
log "assembling $APP_DIR"
mkdir -p "$APP_DIR/bin" "$APP_DIR/.streamlib/cache/packages"
cp "$SRC/target/release/streamlib" "$SRC/target/release/streamlib-runtime" "$APP_DIR/bin/"
cp -a "$SRC/packages" "$APP_DIR/packages"
# npm scope -> in-container Gitea (read is anonymous; no token baked).
printf '@%s:registry=%s/api/packages/%s/npm/\n' "$GITEA_ORG" "$GITEA_URL" "$GITEA_ORG" > /root/.npmrc

# ---------------------------------------------------------------------------
# 10. Pre-materialize api-server into the package cache (compiler-free boot).
#     Runs the actual runtime so the cache slot is byte-identical to what the
#     final image's Strategy::Path{IfStale} boot expects (same key, same
#     inputs_hash, same release profile -> no rebuild on the host).
# ---------------------------------------------------------------------------
if [ "$PREBUILD_API_SERVER" = 1 ]; then
  log "pre-materializing api-server (this compiles the SDK closure once)"
  export STREAMLIB_HOME="$APP_DIR"
  export UV_INDEX="${GITEA_URL}/api/packages/${GITEA_ORG}/pypi/simple"
  # The runtime needs a writable XDG_RUNTIME_DIR for its surface-share socket.
  export XDG_RUNTIME_DIR=/run/user/0
  mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"

  # Resolution preflight: the orchestrator routes cargo stderr to the JSONL log,
  # so a standalone `cargo fetch` here surfaces any dependency-resolution error
  # (e.g. a missing registry crate) directly in the build output — fast, no
  # compile. Run in a temp copy so it doesn't pollute the shipped tree.
  log "preflight: api-server dependency resolution (cargo fetch)"
  rm -rf /tmp/apisrv-diag && cp -a "$APP_DIR/packages/api-server" /tmp/apisrv-diag
  ( cd /tmp/apisrv-diag && cargo fetch 2>&1 ) \
    || fail "api-server dependency resolution failed — root cause printed above"
  rm -rf /tmp/apisrv-diag

  slot="$APP_DIR/.streamlib/cache/packages"
  "$APP_DIR/bin/streamlib-runtime" --host 127.0.0.1 --port 9001 >/tmp/prematerialize.log 2>&1 &
  RT_PID=$!
  built=0
  for i in $(seq 1 3600); do
    if ls "$slot"/api-server-*/.streamlib-build.json >/dev/null 2>&1; then built=1; break; fi
    kill -0 "$RT_PID" 2>/dev/null || { tail -60 /tmp/prematerialize.log; fail "runtime exited before api-server built"; }
    sleep 1
  done
  sleep 2  # let the load settle past the build
  kill "$RT_PID" 2>/dev/null || true; wait "$RT_PID" 2>/dev/null || true
  [ "$built" = 1 ] || { tail -60 /tmp/prematerialize.log; fail "api-server not materialized within 60m"; }
  log "api-server materialized: $(ls -d "$slot"/api-server-* 2>/dev/null)"
else
  log "SKIP api-server pre-materialize (first boot will build it)"
fi

# Gitea data dir (/var/lib/gitea) is left in place for the final stage to COPY.
log "stage 1 complete — registry filled, binaries + cache assembled at $APP_DIR"

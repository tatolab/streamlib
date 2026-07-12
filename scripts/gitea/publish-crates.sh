#!/usr/bin/env bash
# Publish the `streamlib` SDK crate closure (the 14-crate chain `streamlib`
# transitively needs) to the Gitea cargo registry by version, in dependency
# (topological) order. This is the recurring dev-loop publish: a local engine
# change becomes a published 0.4.x-dev.N version a consumer bumps to — never a
# new path dep or [patch]. See docs/architecture/gitea-registry-distribution.md.
#
#   ./publish-crates.sh            # publish the base [workspace.package].version
#   ./publish-crates.sh --dev 3    # publish <base>-dev.3 (workspace + dep reqs
#                                  #   bumped in place, then restored)
#
# The closure + topo order is derived live from `cargo metadata`, so it stays
# correct as the dependency graph shifts — nothing is hard-coded.
#
# Two in-place rewrites are applied before publish and restored after (the tree
# is left exactly as found, clean or not):
#   * --dev N bumps [workspace.package].version and every internal
#     `version = "<base>"` dep requirement to "<base>-dev.N" (a plain "<base>"
#     requirement is ^<base> and does NOT match a prerelease, so the reqs must
#     move too).
#   * streamlib-engine's streamlib.yaml carries a dev `path:` patch for
#     @tatolab/escalate; `cargo publish` bundles streamlib.yaml verbatim, so we
#     strip the path patch (xtask strip-publish-manifest) before publishing,
#     leaving the consumer to resolve @tatolab/escalate from the registry.
#
# Configure-by-env (all optional except the token):
#   GITEA_URL                     default http://localhost:3300
#   GITEA_ORG                     default tatolab
#   CARGO_REGISTRY                default gitea
#   CARGO_REGISTRIES_GITEA_TOKEN  REQUIRED — must be "Bearer <token>"
set -euo pipefail

GITEA_URL="${GITEA_URL:-http://localhost:3300}"
GITEA_ORG="${GITEA_ORG:-tatolab}"
CARGO_REGISTRY="${CARGO_REGISTRY:-gitea}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PY="${PYTHON:-python3}"

DEV_N=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dev) DEV_N="${2:?--dev needs a number}"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

log()  { printf '[publish-crates] %s\n' "$*"; }
fail() { printf '[publish-crates] ERROR: %s\n' "$*" >&2; exit 1; }

[ -n "${CARGO_REGISTRIES_GITEA_TOKEN:-}" ] \
  || fail "set CARGO_REGISTRIES_GITEA_TOKEN='Bearer <token>' to publish"
upper="$(printf '%s' "$CARGO_REGISTRY" | tr '[:lower:]' '[:upper:]')"
eval "export CARGO_REGISTRIES_${upper}_TOKEN='${CARGO_REGISTRIES_GITEA_TOKEN}'"

cd "$ROOT"

base_version="$("$PY" - <<'PY'
import tomlkit
print(tomlkit.parse(open("Cargo.toml").read())["workspace"]["package"]["version"])
PY
)"
target_version="$base_version"
[ -n "$DEV_N" ] && target_version="${base_version}-dev.${DEV_N}"

# --- derive the streamlib release closure in topological order ---------------
# The closure is the SINGLE canonical set of crates a release publishes,
# defined once in streamlib-pack (`compute_release_closure`) and emitted by
# `cargo xtask release-closure --json`. There is deliberately NO "SDK-subset vs
# all-libs" switch: the easy-to-skip libs (streamlib-plugin-sdk, vulkan-jpeg)
# and the subprocess native hosts (streamlib-python-native / -deno-native) are
# all members by definition, in dependency (topological) publish order.
closure_json="$(cargo run -q -p xtask -- release-closure)" \
  || fail "could not compute the release closure (cargo xtask release-closure)"
mapfile -t ORDER < <(printf '%s' "$closure_json" | "$PY" -c '
import json, sys
for c in json.load(sys.stdin)["crates"]:
    print(c["name"])
')
[ "${#ORDER[@]}" -gt 0 ] || fail "could not derive the streamlib closure"
log "closure (${#ORDER[@]} crates): ${ORDER[*]}"
log "publishing version: $target_version"

# --- back up every file we rewrite, restore on exit --------------------------
backup_dir="$(mktemp -d)"
declare -a TOUCHED=()
# Restore verbatim on exit. Backups mirror the relative path under $backup_dir
# (not a flattened key) so no two snapshotted paths can ever collide.
trap 'for f in "${TOUCHED[@]}"; do cp -p "$backup_dir/$f" "$f"; done; rm -rf "$backup_dir"' EXIT
snapshot() {
  local f="$1"
  mkdir -p "$backup_dir/$(dirname "$f")"
  cp -p "$f" "$backup_dir/$f"
  TOUCHED+=("$f")
}

# --- optional dev-version bump (workspace version + internal dep reqs) --------
if [ -n "$DEV_N" ]; then
  log "bumping in place: $base_version -> $target_version (restored on exit)"
  # collect member manifests + root, snapshot, then rewrite with tomlkit.
  # Only libs/ + plugin/ carry the workspace-versioned engine crates the
  # closure publishes; packages/ own INDEPENDENT semver (.slpkg version, per
  # #1239) and are published by publish-packages.sh, so bumping their crate
  # version here would be semantically wrong (a transient bump/restore that
  # doesn't belong to the engine dev-version axis).
  while IFS= read -r m; do snapshot "$m"; done < <(
    { echo Cargo.toml; find libs plugin -name Cargo.toml -not -path '*/target/*'; }
  )
  BASE="$base_version" TARGET="$target_version" "$PY" - <<'PY'
import os, glob, tomlkit
base, target = os.environ["BASE"], os.environ["TARGET"]
def bump_workspace():
    doc = tomlkit.parse(open("Cargo.toml").read())
    doc["workspace"]["package"]["version"] = target
    open("Cargo.toml", "w").write(tomlkit.dumps(doc))
def bump_member(path):
    doc = tomlkit.parse(open(path).read())
    changed = False
    def bump_table(tbl):
        nonlocal changed
        for name in list(tbl.keys()):
            dep = tbl[name]
            # Any internal dep (a `path` dep with an explicit version req) must
            # move to the prerelease target. Matching on `== base` is too narrow:
            # internal reqs are floor-pinned (e.g. "0.4.30") while the workspace
            # version floats above, so an exact-base match misses them and the
            # prerelease crate then fails its own ^floor req (caret excludes
            # prereleases). Rewriting every path-dep's version is correct because
            # path deps are always in-workspace crates published at `target`.
            if isinstance(dep, dict) and "path" in dep and "version" in dep:
                dep["version"] = target
                changed = True
    for sec in ("dependencies", "build-dependencies", "dev-dependencies"):
        if sec in doc:
            bump_table(doc[sec])
    # [target.'cfg(...)'.dependencies] — the native hosts pin their adapter +
    # consumer-rhi deps here, so these must move to the prerelease too.
    for cfg_tbl in doc.get("target", {}).values():
        for sec in ("dependencies", "build-dependencies", "dev-dependencies"):
            if sec in cfg_tbl:
                bump_table(cfg_tbl[sec])
    if changed:
        open(path, "w").write(tomlkit.dumps(doc))
bump_workspace()
# libs/ + plugin/ only — packages/ own independent .slpkg semver (#1239) and
# are not part of the engine dev-version bump.
for m in (glob.glob("libs/**/Cargo.toml", recursive=True)
          + glob.glob("plugin/**/Cargo.toml", recursive=True)):
    if "/target/" in m: continue
    bump_member(m)
print("bumped")
PY
fi

# --- strip dev path patches from any bundled streamlib.yaml in the closure ----
for crate in "${ORDER[@]}"; do
  dir="libs/$crate"
  [ "$crate" = "streamlib" ] && dir="libs/streamlib-sdk"
  yaml="$dir/streamlib.yaml"
  if [ -f "$yaml" ] && grep -q '^patch:' "$yaml"; then
    log "stripping path patches from $yaml (restored on exit)"
    snapshot "$yaml"
    cargo run -q -p xtask -- strip-publish-manifest --dir "$dir" >/dev/null 2>&1 \
      || fail "strip-publish-manifest failed for $dir"
  fi
done

# --- publish in topological order --------------------------------------------
idx_path() {
  local n="$1"
  case ${#n} in
    1) printf '1/%s' "$n" ;; 2) printf '2/%s' "$n" ;;
    3) printf '3/%s/%s' "${n:0:1}" "$n" ;;
    *) printf '%s/%s/%s' "${n:0:2}" "${n:2:2}" "$n" ;;
  esac
}
wait_for_index() {
  local crate="$1" version="$2" i
  for i in $(seq 1 30); do
    curl -fsS "${GITEA_URL}/api/packages/${GITEA_ORG}/cargo/$(idx_path "$crate")" 2>/dev/null \
      | grep -q "\"vers\":\"${version}\"" && return 0
    sleep 1
  done
  log "WARN: ${crate}@${version} not visible in index after 30s (continuing)"
}
for crate in "${ORDER[@]}"; do
  log "publishing ${crate}@${target_version}"
  # --allow-dirty: the strip / --dev rewrites mutate the tree on purpose
  # (restored by the EXIT trap); publishing source with those edits is intended.
  if out="$(cargo publish --no-verify --allow-dirty --registry "$CARGO_REGISTRY" -p "$crate" 2>&1)"; then
    log "  ✓ ${crate} published"
  elif printf '%s' "$out" | grep -qiE 'already exists|already uploaded|is already'; then
    log "  • ${crate}@${target_version} already present — skipping"
  else
    printf '%s\n' "$out" >&2
    fail "publish of ${crate} failed"
  fi
  wait_for_index "$crate" "$target_version"
done

log "done — streamlib closure @ ${target_version} published to ${GITEA_ORG} on ${GITEA_URL}"

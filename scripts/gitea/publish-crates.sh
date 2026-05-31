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

# --- derive the streamlib closure in topological order -----------------------
mapfile -t ORDER < <("$PY" - <<'PY'
import json, subprocess
md = json.loads(subprocess.check_output(["cargo","metadata","--format-version","1"]))
pkgs = {p["id"]: p for p in md["packages"]}
members = set(md["workspace_members"])
resolve = {n["id"]: n for n in md["resolve"]["nodes"]}
name = lambda i: pkgs[i]["name"]
# Publish-closure roots: the `streamlib` SDK crate a consumer deps, PLUS the
# subprocess native hosts the build orchestrator fetches from the registry by
# exact version (streamlib-python-native / streamlib-deno-native). The hosts
# pull adapters + consumer-rhi the SDK closure alone doesn't, so the union of
# the three roots' closures (topo-ordered) is what a polyglot consumer needs.
root_names = ("streamlib", "streamlib-python-native", "streamlib-deno-native")
roots = [i for i in members if name(i) in root_names]
internal = lambda i: i in members and (name(i).startswith("streamlib") or name(i) == "vulkan-jpeg")
def deps(i):
    out = []
    for d in resolve[i]["deps"]:
        if {k["kind"] for k in d["dep_kinds"]} & {None, "build"}:
            out.append(d["pkg"])
    return out
seen, order = set(), []
def visit(i):
    if i in seen: return
    seen.add(i)
    for d in deps(i):
        if internal(d): visit(d)
    if internal(i): order.append(name(i))
for r in roots:
    visit(r)
print("\n".join(order))
PY
)
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
  # collect member manifests + root, snapshot, then rewrite with tomlkit
  while IFS= read -r m; do snapshot "$m"; done < <(
    { echo Cargo.toml; find libs packages -name Cargo.toml -not -path '*/target/*'; }
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
for m in glob.glob("libs/**/Cargo.toml", recursive=True) + glob.glob("packages/**/Cargo.toml", recursive=True):
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

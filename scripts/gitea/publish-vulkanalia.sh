#!/usr/bin/env bash
# Publish the tatolab/vulkanalia fork (vulkanalia-sys, vulkanalia,
# vulkanalia-vma) to the Gitea cargo registry, so the workspace resolves the
# fork by version with `registry = "gitea"` instead of a git `[patch]`.
#
# One-time bootstrap, re-runnable: cargo rejects republishing an existing
# version (HTTP 409 / "already exists"), which this script treats as success.
#
# Why a non-git scratch copy: vulkanalia-vma vendors VMA + Vulkan-Headers as
# git submodules, and cargo *excludes submodule contents when packaging inside
# a git repo*. Publishing from a plain (non-git) copy makes cargo bundle the
# vendored C++ sources so a consumer can compile vulkanalia-vma. The same copy
# is where we annotate the fork's inter-crate deps with `registry = "gitea"`
# (the fork's own manifests use bare `version`/`path`, which would resolve the
# fork's siblings from crates.io and silently pull upstream instead).
#
# Configure-by-env (all optional except the token):
#   GITEA_URL                     default http://localhost:3300
#   GITEA_ORG                     default tatolab
#   CARGO_REGISTRY                default gitea   (name in .cargo/config.toml)
#   VULKANALIA_DIR                default ~/Repositories/tatolab/vulkanalia
#   CARGO_REGISTRIES_GITEA_TOKEN  REQUIRED to publish — must be "Bearer <token>"
#                                 (cargo login stores it bare → 401).
#
# Secrets never live in this script — provide the token in the environment
# (e.g. via the gitignored scripts/gitea/*.local.sh wrapper).
set -euo pipefail

GITEA_URL="${GITEA_URL:-http://localhost:3300}"
GITEA_ORG="${GITEA_ORG:-tatolab}"
CARGO_REGISTRY="${CARGO_REGISTRY:-gitea}"
VULKANALIA_DIR="${VULKANALIA_DIR:-$HOME/Repositories/tatolab/vulkanalia}"
INDEX="sparse+${GITEA_URL}/api/packages/${GITEA_ORG}/cargo/"

log()  { printf '[publish-vulkanalia] %s\n' "$*"; }
fail() { printf '[publish-vulkanalia] ERROR: %s\n' "$*" >&2; exit 1; }

[ -n "${CARGO_REGISTRIES_GITEA_TOKEN:-}" ] \
  || fail "set CARGO_REGISTRIES_GITEA_TOKEN='Bearer <token>' to publish"
[ -d "$VULKANALIA_DIR" ] || fail "fork not found at VULKANALIA_DIR=$VULKANALIA_DIR"

# cargo reads the registry index + token from the environment, so the scratch
# copy (outside the monorepo) resolves `gitea` without a local .cargo/config.
export CARGO_REGISTRIES_GITEA_INDEX="$INDEX"
upper="$(printf '%s' "$CARGO_REGISTRY" | tr '[:lower:]' '[:upper:]')"
eval "export CARGO_REGISTRIES_${upper}_INDEX='$INDEX'"
eval "export CARGO_REGISTRIES_${upper}_TOKEN='${CARGO_REGISTRIES_GITEA_TOKEN}'"

log "ensuring vendored submodules are checked out"
git -C "$VULKANALIA_DIR" submodule update --init \
  ext/vma/vendor/Vulkan-Headers ext/vma/vendor/VulkanMemoryAllocator >/dev/null 2>&1 || true

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
log "copying fork to non-git scratch: $scratch"
rsync -a --exclude='.git' --exclude='target' "$VULKANALIA_DIR"/ "$scratch"/

log "annotating inter-crate deps with registry = \"$CARGO_REGISTRY\""
REGISTRY="$CARGO_REGISTRY" python3 - "$scratch" <<'PY'
import os, sys, tomlkit
scratch = sys.argv[1]
reg = os.environ["REGISTRY"]
def annotate(manifest, dep_names):
    doc = tomlkit.parse(open(manifest).read())
    deps = doc.get("dependencies", {})
    changed = False
    for name in dep_names:
        d = deps.get(name)
        if d is None:
            continue
        new = tomlkit.inline_table()
        # keep version, drop dev-only path, force the fork's registry
        if "version" in d:
            new["version"] = d["version"]
        else:
            new["version"] = "*"
        new["registry"] = reg
        for k, v in d.items():
            if k in ("version", "registry", "path"):
                continue
            new[k] = v
        deps[name] = new
        changed = True
    if changed:
        open(manifest, "w").write(tomlkit.dumps(doc))
        print(f"  annotated {dep_names} in {manifest}")
annotate(f"{scratch}/vulkanalia/Cargo.toml", ["vulkanalia-sys"])
annotate(f"{scratch}/ext/vma/Cargo.toml", ["vulkanalia"])
PY

wait_for_index() {
  local crate="$1" version="$2" i
  for i in $(seq 1 30); do
    if curl -fsS "${GITEA_URL}/api/packages/${GITEA_ORG}/cargo/$(idx_path "$crate")" 2>/dev/null \
         | grep -q "\"vers\":\"${version}\""; then
      return 0
    fi
    sleep 1
  done
  log "WARN: ${crate}@${version} not visible in index after 30s (continuing)"
}
# cargo sparse-index path: 1-char / 2-char / 3-char dirs for names >=4 chars
idx_path() {
  local n="$1"
  case ${#n} in
    1) printf '1/%s' "$n" ;;
    2) printf '2/%s' "$n" ;;
    3) printf '3/%s/%s' "${n:0:1}" "$n" ;;
    *) printf '%s/%s/%s' "${n:0:2}" "${n:2:2}" "$n" ;;
  esac
}

publish_one() {
  local crate="$1" version="$2" manifest="$3"
  log "publishing ${crate}@${version}"
  if out="$(cargo publish --no-verify --allow-dirty \
              --registry "$CARGO_REGISTRY" \
              --manifest-path "$manifest" 2>&1)"; then
    log "  ✓ ${crate}@${version} published"
  elif printf '%s' "$out" | grep -qiE 'already exists|already uploaded|crate version .* is already'; then
    log "  • ${crate}@${version} already present — skipping"
  else
    printf '%s\n' "$out" >&2
    fail "publish of ${crate} failed"
  fi
  wait_for_index "$crate" "$version"
}

publish_one vulkanalia-sys 0.35.0 "$scratch/vulkanalia-sys/Cargo.toml"
publish_one vulkanalia     0.35.0 "$scratch/vulkanalia/Cargo.toml"
publish_one vulkanalia-vma 0.9.0  "$scratch/ext/vma/Cargo.toml"

log "done — vulkanalia fork published to $GITEA_ORG on $GITEA_URL"

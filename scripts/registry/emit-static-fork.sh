#!/usr/bin/env bash
# Emit a STATIC, daemon-free cargo sparse-index tree for the tatolab/vulkanalia
# fork (vulkanalia-sys, vulkanalia, vulkanalia-vma) into <out>/cargo/. No registry daemon,
# no database, no token — just files a dumb static HTTP server can serve.
#
# Why this exists as a standalone shell step (not `cargo xtask`): the workspace
# declares `vulkanalia = { registry = "tatolab" }`, so cargo cannot resolve —
# and therefore cannot BUILD xtask — until the fork is fetchable. This script
# breaks that chicken-and-egg by packaging the fork from a standalone clone
# (the fork only depends on crates.io + itself, never the workspace or registry daemon),
# exactly like scripts/registry/publish-vulkanalia.sh does — but it writes a
# static file tree instead of PUTting to a registry daemon daemon. CI serves the tree
# with `python3 -m http.server` and points cargo at it via
# `CARGO_REGISTRIES_TATOLAB_INDEX=sparse+http://127.0.0.1:PORT/cargo/`.
#
#   ./emit-static-fork.sh <out_dir> [--base-url http://127.0.0.1:8000] \
#                                    [--fork-url http://127.0.0.1:8000]
#
# The cargo sparse `dl` template needs an absolute URL (sparse is HTTP-only by
# spec), so --base-url is baked into <out>/cargo/config.json — pass the exact
# scheme://host:port the tree will be served at. The .crate tarballs and index
# NDJSON are relocatable; only config.json carries the base URL.
#
# Sibling resolution during packaging: `cargo package` for vulkanalia needs to
# resolve vulkanalia-sys (and -vma needs vulkanalia) from the tatolab registry.
# Two modes:
#   * --fork-url URL (or env STATIC_FORK_URL): an EXTERNAL static server is
#     already serving a tree containing the fork (CI: the composite action
#     serves <out> itself while this script populates it). No server is
#     started here; cargo resolves via that URL. This removes the
#     bind-own-port coupling that silently resolves from a different tree.
#   * neither set: a throwaway `python3 -m http.server` is started on the
#     --base-url port serving <out>; a bind failure is FATAL (packaging the
#     later fork crates cannot succeed without sibling resolution).
#
# Configure-by-env (all optional):
#   VULKANALIA_DIR   a local checkout of the fork (skips the clone). When unset,
#                    the pinned rev is cloned from GitHub (rev sourced from the
#                    workspace, not hardcoded).
#   STATIC_FORK_URL  same as --fork-url (the flag wins when both are set).
#   STREAMLIB_REGISTRY_ORG        default tatolab (only affects the annotated `registry` name,
#                    which is cosmetic in a static tree — the index carries the
#                    resolution truth).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PY="${PYTHON:-python3}"

OUT=""
BASE_URL="http://127.0.0.1:8000"
FORK_URL="${STATIC_FORK_URL:-}"
while [ $# -gt 0 ]; do
  case "$1" in
    --base-url) BASE_URL="${2:?--base-url needs a URL}"; shift 2 ;;
    --fork-url) FORK_URL="${2:?--fork-url needs a URL}"; shift 2 ;;
    -*) echo "unknown arg: $1" >&2; exit 2 ;;
    *) if [ -z "$OUT" ]; then OUT="$1"; else echo "unexpected arg: $1" >&2; exit 2; fi; shift ;;
  esac
done
[ -n "$OUT" ] || { echo "usage: emit-static-fork.sh <out_dir> [--base-url URL] [--fork-url URL]" >&2; exit 2; }
BASE_URL="${BASE_URL%/}"
FORK_URL="${FORK_URL%/}"

log()  { printf '[emit-static-fork] %s\n' "$*"; }
fail() { printf '[emit-static-fork] ERROR: %s\n' "$*" >&2; exit 1; }

command -v cargo  >/dev/null || fail "cargo not found"
command -v rsync  >/dev/null || fail "rsync not found"
command -v git    >/dev/null || fail "git not found"
command -v tar    >/dev/null || fail "tar not found"

# --- obtain the fork checkout -------------------------------------------------
scratch_root="$(mktemp -d)"
trap 'rm -rf "$scratch_root"' EXIT

if [ -n "${VULKANALIA_DIR:-}" ]; then
  [ -d "$VULKANALIA_DIR" ] || fail "VULKANALIA_DIR=$VULKANALIA_DIR does not exist"
  src_fork="$VULKANALIA_DIR"
  log "using local fork checkout: $src_fork"
else
  rev="$(grep -oE 'rev = "[0-9a-f]{40}"' "$ROOT/libs/streamlib-cross-rustc-fixture/Cargo.toml" \
         | head -1 | grep -oE '[0-9a-f]{40}')"
  [ -n "$rev" ] || fail "could not derive the vulkanalia rev from the workspace"
  src_fork="$scratch_root/vulkanalia-src"
  log "cloning tatolab/vulkanalia @ $rev"
  git clone --quiet https://github.com/tatolab/vulkanalia.git "$src_fork"
  git -C "$src_fork" checkout --quiet "$rev"
fi
git -C "$src_fork" submodule update --init --quiet \
  ext/vma/vendor/Vulkan-Headers ext/vma/vendor/VulkanMemoryAllocator 2>/dev/null || true

# --- copy to a non-git scratch (so cargo bundles the vendored VMA/headers) ----
scratch="$scratch_root/fork"
mkdir -p "$scratch"
rsync -a --exclude='.git' --exclude='target' "$src_fork"/ "$scratch"/

# --- annotate inter-crate deps with registry = "tatolab" (fork siblings) --------
# The fork's own manifests use bare version/path for their siblings, which would
# resolve from crates.io (upstream). Force the fork's registry so the packaged
# Cargo.toml records the inter-fork dep as a tatolab-registry dep — the static
# index then renders it as a same-registry (null-registry) index dep.
REGISTRY="tatolab" "$PY" - "$scratch" <<'PY'
import os, sys, tomlkit
scratch = sys.argv[1]
reg = os.environ["REGISTRY"]
def annotate(manifest, dep_names):
    doc = tomlkit.parse(open(manifest).read())
    changed = False
    for sec in ("dependencies", "build-dependencies"):
        deps = doc.get(sec, {})
        for name in dep_names:
            d = deps.get(name)
            if d is None:
                continue
            new = tomlkit.inline_table()
            new["version"] = d["version"] if "version" in d else "*"
            new["registry"] = reg
            for k, v in d.items():
                if k in ("version", "registry", "path"):
                    continue
                new[k] = v
            deps[name] = new
            changed = True
    if changed:
        open(manifest, "w").write(tomlkit.dumps(doc))
annotate(f"{scratch}/vulkanalia/Cargo.toml", ["vulkanalia-sys"])
annotate(f"{scratch}/ext/vma/Cargo.toml", ["vulkanalia"])
PY

# shellcheck source=scripts/registry/cargo-idx-path.sh
. "$ROOT/scripts/registry/cargo-idx-path.sh"

# --- package each fork crate into a .crate (no tatolab, no workspace) -----------
# `cargo package` resolves the crate's deps to write the normalized (published)
# Cargo.toml. The fork's only registry dep is its own siblings; we serve those
# incrementally from the tree we are building so a later crate resolves an
# earlier one from the static index — no registry daemon needed at any point.
CARGO_DIR="$OUT/cargo"
mkdir -p "$CARGO_DIR/crates"

# Sibling resolution: cargo resolves the earlier-emitted fork sibling from a
# sparse index over HTTP (sparse is HTTP-only — no file:// form). Either an
# external server is already serving a fork-bearing tree (--fork-url), or a
# throwaway server on BASE_URL's port serves the tree we are populating.
if [ -n "$FORK_URL" ]; then
  export CARGO_REGISTRIES_TATOLAB_INDEX="sparse+${FORK_URL}/cargo/"
else
  export CARGO_REGISTRIES_TATOLAB_INDEX="sparse+${BASE_URL}/cargo/"
fi

# Write config.json up front so the throwaway server (if used) serves it.
"$PY" - "$CARGO_DIR/config.json" "$BASE_URL" <<'PY'
import sys
out, base = sys.argv[1], sys.argv[2].rstrip("/")
# Templated `dl` → clean, browsable .crate filenames; `api` unused for reads.
cfg = '{"dl":"%s/cargo/crates/{crate}/{crate}-{version}.crate","api":"%s/cargo"}\n' % (base, base)
open(out, "w").write(cfg)
PY

# Serve the (growing) tree so cargo can resolve fork siblings during packaging
# — unless an external server already does (--fork-url / STATIC_FORK_URL).
srv_pid=""
stop_server() { if [ -n "$srv_pid" ]; then kill "$srv_pid" 2>/dev/null || true; fi; srv_pid=""; }
trap 'stop_server; rm -rf "$scratch_root"' EXIT
if [ -z "$FORK_URL" ]; then
  srv_port="${BASE_URL##*:}"
  case "$srv_port" in *[!0-9]*) srv_port="8000" ;; esac
  ( cd "$OUT" && exec "$PY" -m http.server "$srv_port" --bind 127.0.0.1 ) >/dev/null 2>&1 &
  srv_pid=$!
  server_up=""
  for _ in $(seq 1 25); do
    if curl -fsS "${BASE_URL}/cargo/config.json" >/dev/null 2>&1; then server_up=1; break; fi
    sleep 0.2
  done
  # FATAL: without sibling resolution, packaging vulkanalia / -vma fails, or
  # worse — a stale server on this port would resolve from a DIFFERENT tree.
  [ -n "$server_up" ] || fail "throwaway static server did not come up on ${BASE_URL} (port busy? pass --fork-url to use an external server)"
fi

emit_one() {
  local name="$1" version="$2" manifest="$3"
  log "packaging ${name}@${version}"
  local pkg_root crate_file
  pkg_root="$(dirname "$manifest")"
  cargo package --no-verify --allow-dirty --manifest-path "$manifest" >/dev/null 2>&1 \
    || fail "cargo package failed for ${name} (manifest: ${manifest})"
  # `.crate` lands under the package's target dir. Search both the package-local
  # and any parent workspace target for robustness.
  crate_file="$(find "$pkg_root" "$scratch" -type f -name "${name}-${version}.crate" 2>/dev/null | head -1)"
  [ -n "$crate_file" ] || fail "could not locate ${name}-${version}.crate after packaging"

  local dest_dir="$CARGO_DIR/crates/${name}"
  mkdir -p "$dest_dir"
  cp "$crate_file" "$dest_dir/${name}-${version}.crate"

  local cksum
  cksum="$(sha256sum "$crate_file" | awk '{print $1}')"

  # Render the sparse index NDJSON line from the packaged Cargo.toml deps.
  local idx_rel idx_abs
  idx_rel="$(cargo_idx_path "$name")"
  idx_abs="$CARGO_DIR/$idx_rel"
  mkdir -p "$(dirname "$idx_abs")"
  NAME="$name" VERSION="$version" CKSUM="$cksum" CRATE="$crate_file" \
    "$PY" "$ROOT/scripts/registry/render_cargo_index_line.py" >> "$idx_abs"
}

emit_one vulkanalia-sys 0.35.0 "$scratch/vulkanalia-sys/Cargo.toml"
emit_one vulkanalia     0.35.0 "$scratch/vulkanalia/Cargo.toml"
emit_one vulkanalia-vma 0.9.0  "$scratch/ext/vma/Cargo.toml"

stop_server
log "done — vulkanalia fork static cargo tree at $CARGO_DIR (served base: $BASE_URL)"

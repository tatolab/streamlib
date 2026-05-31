#!/usr/bin/env bash
# Publish every streamlib package under packages/* to the Gitea **generic**
# registry as a source-only `.slpkg`, by version. The package twin of
# publish-crates.sh / publish-python-sdk.sh: each package's `streamlib.yaml`
# version becomes a published `.slpkg` a cross-repo host resolves via
# `Strategy::Registry` (`add_module`) — never a relative path or git patch.
# See docs/architecture/gitea-registry-distribution.md.
#
# Each publish repacks a fresh source-only `.slpkg` (no prebuilt cdylib) and,
# as part of `streamlib pkg publish`, (re)writes the package's anonymous
# cargo-sparse-shaped version index so the read path lists versions tokenless.
#
#   ./publish-packages.sh                 # publish all packages/* (skip test fixtures)
#   ./publish-packages.sh camera display  # publish only the named packages
#
# Configure-by-env (all optional except the token):
#   GITEA_URL              default http://localhost:3300
#   GITEA_ORG              default tatolab
#   GITEA_PUBLISH_TOKEN    REQUIRED — a Gitea token with write:package
#   STREAMLIB_PKG_SKIP     space-separated package names to skip
#                          (default: the test-fixture packages)
#   STREAMLIB_BIN          path to a prebuilt `streamlib` binary (skips the
#                          in-repo build when set)
set -euo pipefail

GITEA_URL="${GITEA_URL:-http://localhost:3300}"
GITEA_ORG="${GITEA_ORG:-tatolab}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PKG_DIR="$ROOT/packages"
SKIP="${STREAMLIB_PKG_SKIP:-test-fixtures test-fixtures-abi-mismatch}"

log()  { printf '[publish-packages] %s\n' "$*"; }
fail() { printf '[publish-packages] ERROR: %s\n' "$*" >&2; exit 1; }

[ -n "${GITEA_PUBLISH_TOKEN:-}" ] || fail "set GITEA_PUBLISH_TOKEN (write:package)"

# `streamlib pkg publish` reads the registry endpoint + credential from the
# resolver's env channel (RegistryConfig::from_env). Map the publish token
# onto it; the read path is anonymous, so the token is publish-only.
export STREAMLIB_REGISTRY_URL="${STREAMLIB_REGISTRY_URL:-$GITEA_URL}"
export STREAMLIB_REGISTRY_TOKEN="$GITEA_PUBLISH_TOKEN"

# Resolve the `streamlib` CLI: a caller-provided binary, else build it once.
if [ -n "${STREAMLIB_BIN:-}" ]; then
  BIN="$STREAMLIB_BIN"
else
  log "building the streamlib CLI (release)…"
  cargo build --release -p streamlib-cli --bin streamlib --manifest-path "$ROOT/Cargo.toml" >&2
  BIN="$ROOT/target/release/streamlib"
fi
[ -x "$BIN" ] || fail "streamlib binary not found/executable: $BIN"

# Pick the package set: explicit args, else every packages/* with a manifest.
if [ "$#" -gt 0 ]; then
  names=("$@")
else
  names=()
  for d in "$PKG_DIR"/*/; do
    [ -f "$d/streamlib.yaml" ] || continue
    names+=("$(basename "$d")")
  done
fi
# Guard the empty set explicitly: `"${names[@]}"` on an empty array trips
# `set -u` ("unbound variable") on bash < 4.4 (e.g. macOS's default 3.2).
[ "${#names[@]}" -gt 0 ] || fail "no packages to publish under $PKG_DIR"

published=0 skipped=0 failed=0
for name in "${names[@]}"; do
  case " $SKIP " in *" $name "*) log "skip $name (in STREAMLIB_PKG_SKIP)"; skipped=$((skipped+1)); continue ;; esac
  dir="$PKG_DIR/$name"
  [ -f "$dir/streamlib.yaml" ] || { log "skip $name (no streamlib.yaml)"; skipped=$((skipped+1)); continue; }
  log "publishing $name → $STREAMLIB_REGISTRY_URL"
  if ( cd "$dir" && "$BIN" pkg publish ); then
    published=$((published+1))
  else
    log "FAILED to publish $name"
    failed=$((failed+1))
  fi
done

log "done: $published published, $skipped skipped, $failed failed"
[ "$failed" -eq 0 ] || exit 1

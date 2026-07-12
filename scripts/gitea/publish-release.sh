#!/usr/bin/env bash
# Publish a CONSISTENT, ATOMIC release of the streamlib version surface to the
# Gitea registry: the full crate closure, the polyglot SDKs, and the packages
# — then the release manifest, written LAST as the atomicity flip.
#
# The manifest at `streamlib-release/<V>/manifest.json` is the completion
# marker: it is written only after every other artifact lands, so a consumer
# that finds it knows the release is complete, and a consumer resolving against
# a mid-publish registry (no manifest yet, or a manifest that doesn't list a
# pinned crate) fails fast with an actionable "incomplete release" error
# instead of a cryptic cargo version-unification failure.
#
#   ./publish-release.sh            # publish the base [workspace.package].version
#   ./publish-release.sh --dev 3    # publish <base>-dev.3
#
# Honors the same SKIP_* guards as docker/build-stage1.sh so a partial release
# (e.g. crates-only in a fast CI lane) records exactly what was published:
#   SKIP_PYTHON_SDK=1   skip the Python SDK publish (manifest omits `python`)
#   SKIP_DENO_SDK=1     skip the Deno SDK publish   (manifest omits `deno`)
#   SKIP_PACKAGES=1     skip the package publish    (manifest omits `packages`)
#
# Registry config is by-env (shared with the other publish scripts):
#   GITEA_URL / STREAMLIB_REGISTRY_URL   registry base URL
#   GITEA_ORG                            org (default tatolab)
#   CARGO_REGISTRIES_GITEA_TOKEN         cargo publish token ("Bearer <token>")
#   STREAMLIB_REGISTRY_TOKEN             manifest-upload token (raw token)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"

DEV_N=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dev) DEV_N="${2:?--dev needs a number}"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

log()  { printf '[publish-release] %s\n' "$*"; }
fail() { printf '[publish-release] ERROR: %s\n' "$*" >&2; exit 1; }

dev_args=()
[ -n "$DEV_N" ] && dev_args=(--dev "$DEV_N")

manifest_args=()
[ -n "$DEV_N" ] && manifest_args+=(--dev "$DEV_N")

cd "$ROOT"

# 1. Crate closure (the single canonical release closure — no ALL_LIBS flag).
log "publishing crate closure"
"$HERE/publish-crates.sh" "${dev_args[@]}"

# 2. Polyglot SDKs — same --dev train as the crates so the manifest's
#    recorded SDK versions match what was actually published (Python spells
#    the same dev train `<base>.devN` per PEP 440; the manifest records the
#    canonical semver form).
if [ "${SKIP_PYTHON_SDK:-0}" != 1 ]; then
  log "publishing python SDK"; "$HERE/publish-python-sdk.sh" "${dev_args[@]}"
else
  log "SKIP python SDK"; manifest_args+=(--skip-python)
fi
if [ "${SKIP_DENO_SDK:-0}" != 1 ]; then
  log "publishing deno SDK"; "$HERE/publish-deno-sdk.sh" "${dev_args[@]}"
else
  log "SKIP deno SDK"; manifest_args+=(--skip-deno)
fi

# 3. Packages (.slpkg).
if [ "${SKIP_PACKAGES:-0}" != 1 ]; then
  log "publishing packages (.slpkg)"; "$HERE/publish-packages.sh"
else
  log "SKIP packages"; manifest_args+=(--skip-packages)
fi

# 4. Release manifest — LAST. This is the atomicity flip: the release is not
#    consumable-as-complete until this lands.
log "publishing release manifest (marks the release complete)"
cargo run -q -p xtask -- release-manifest-publish "${manifest_args[@]}" \
  || fail "release manifest publish failed — the release is published but NOT marked complete"

log "done — consistent release published"

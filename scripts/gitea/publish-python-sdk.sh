#!/usr/bin/env bash
# Publish the `streamlib` Python SDK (sdist + pure-Python wheel) to the Gitea
# pypi registry by version. The Python twin of publish-crates.sh: a local engine
# change becomes a published version a Python consumer resolves from Gitea by
# version — never a PYTHONPATH inject or path/editable install.
# See docs/architecture/gitea-registry-distribution.md.
#
#   ./publish-python-sdk.sh            # publish the base [workspace.package].version
#   ./publish-python-sdk.sh --dev 3    # publish <base>.dev3
#
# Version is derived from the Rust workspace's [workspace.package].version
# (single source of truth) so the Python SDK rides the same train as the cargo
# closure. NOTE the grammar difference: cargo prereleases are `<base>-dev.N`
# (semver); PEP 440 spells the same thing `<base>.devN`. This script emits the
# PEP 440 form so pip/uv accept it; the two are semantically the same dev train.
# pyproject's committed `version` is rewritten in place from the workspace
# version and restored on exit (mirrors publish-crates.sh's in-place rewrite).
#
# Configure-by-env (all optional except the token + user):
#   GITEA_URL              default http://localhost:3300
#   GITEA_ORG              default tatolab
#   GITEA_PUBLISH_TOKEN    REQUIRED — a Gitea token with write:package
#   GITEA_PUBLISH_USER     REQUIRED — the Gitea username the token belongs to
#                          (Gitea pypi upload is basic-auth: user + token-as-password)
set -euo pipefail

GITEA_URL="${GITEA_URL:-http://localhost:3300}"
GITEA_ORG="${GITEA_ORG:-tatolab}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROJECT="$ROOT/libs/streamlib-python"
PY="${PYTHON:-python3}"

DEV_N=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dev) DEV_N="${2:?--dev needs a number}"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

log()  { printf '[publish-python-sdk] %s\n' "$*"; }
fail() { printf '[publish-python-sdk] ERROR: %s\n' "$*" >&2; exit 1; }

[ -n "${GITEA_PUBLISH_TOKEN:-}" ] || fail "set GITEA_PUBLISH_TOKEN (write:package)"
[ -n "${GITEA_PUBLISH_USER:-}" ]  || fail "set GITEA_PUBLISH_USER (the token's owner)"
command -v uv >/dev/null || fail "uv not found (the SDK's build + publish toolchain)"

# --- derive the target version from the workspace (single source of truth) ----
base_version="$("$PY" - "$ROOT/Cargo.toml" <<'PY'
import sys, tomllib
with open(sys.argv[1], "rb") as f:
    print(tomllib.load(f)["workspace"]["package"]["version"])
PY
)"
target_version="$base_version"
[ -n "$DEV_N" ] && target_version="${base_version}.dev${DEV_N}"
log "publishing streamlib==$target_version to $GITEA_ORG on $GITEA_URL"

# --- snapshot the files we rewrite; restore all on exit -----------------------
# Two in-place rewrites (both restored): pyproject version, and stripping the
# dev `path:` patches from streamlib.yaml so a registry consumer resolves the
# SDK's schema deps (@tatolab/core, @tatolab/escalate) from the registry rather
# than a dangling ../../packages path. The stripped manifest is what `build_py`
# stages into the installed package, so the runtime-layer codegen resolves from
# the registry. `out` is filled in below.
pyproject="$PROJECT/pyproject.toml"
manifest="$PROJECT/streamlib.yaml"
pyproject_bak="$(mktemp)"; cp -p "$pyproject" "$pyproject_bak"
manifest_bak="$(mktemp)"; cp -p "$manifest" "$manifest_bak"
out=""
restore() {
  cp -p "$pyproject_bak" "$pyproject"; cp -p "$manifest_bak" "$manifest"
  rm -f "$pyproject_bak" "$manifest_bak"; [ -n "$out" ] && rm -rf "$out"
}
trap restore EXIT

log "stripping dev path: patches from streamlib.yaml"
( cd "$ROOT" && cargo run -q -p xtask -- strip-publish-manifest --dir "$PROJECT" ) \
  >/dev/null 2>&1 || fail "strip-publish-manifest failed"

TARGET="$target_version" "$PY" - "$pyproject" <<'PY'
import os, re, sys
path = sys.argv[1]
target = os.environ["TARGET"]
src = open(path).read()
# Replace the first `version = "..."` under [project] only. The file is small
# and the project version is the first such key; a targeted regex avoids a
# tomlkit dependency on the publish host.
new, n = re.subn(r'(?m)^version = "[^"]*"', f'version = "{target}"', src, count=1)
if n != 1:
    sys.exit("could not rewrite [project].version in pyproject.toml")
open(path, "w").write(new)
PY

# --- build the sdist (source only) --------------------------------------------
# Source distribution only: the registry stores a packaged artifact of the
# SOURCE (no `_generated_/` — that's a build artifact the runtime layer
# regenerates after pull-down; see MANIFEST.in). A wheel is deliberately not
# published — building one in the monorepo would bake the locally-generated
# `_generated_/` into it, defeating the source-only contract.
out="$(mktemp -d)"
log "building sdist (source only)"
( cd "$PROJECT" && uv build --sdist --out-dir "$out" ) >/dev/null 2>&1 \
  || fail "uv build --sdist failed"
log "built: $(cd "$out" && echo *)"

# --- publish to Gitea's pypi upload endpoint ----------------------------------
# Gitea pypi upload is basic-auth (user + token-as-password); a duplicate
# version returns HTTP 409 which uv surfaces as an error — treat as already
# published and continue (idempotent re-runs, mirrors publish-crates.sh).
publish_url="${GITEA_URL}/api/packages/${GITEA_ORG}/pypi"
if out_log="$(cd "$out" && uv publish \
      --publish-url "$publish_url" \
      --username "$GITEA_PUBLISH_USER" \
      --password "$GITEA_PUBLISH_TOKEN" \
      ./* 2>&1)"; then
  log "  ✓ streamlib==$target_version published"
elif printf '%s' "$out_log" | grep -qiE 'already exist|conflict|409|duplicate'; then
  log "  • streamlib==$target_version already present — skipping"
else
  printf '%s\n' "$out_log" >&2
  fail "uv publish failed"
fi

log "done — streamlib==$target_version on ${GITEA_ORG} at ${GITEA_URL}"
log "consumers resolve it via: UV_INDEX=${GITEA_URL}/api/packages/${GITEA_ORG}/pypi/simple"

#!/bin/bash
# Smoke-test a Gitea registry namespace: publish a throwaway crate, resolve it
# by version, then remove it — plus a generic-registry round-trip (the generic
# registry is where `.slpkg` packages live).
#
# Self-contained and self-cleaning: uses a temp CARGO_HOME so it never touches
# the operator's cargo config, and deletes everything it publishes. Safe to run
# repeatedly against a live registry.
#
# Env:
#   GITEA_ORG          REQUIRED — the org namespace to test
#   GITEA_URL          default http://localhost:3000
#   GITEA_ADMIN_TOKEN  required — a token that can publish to the org
#
# Exit codes: 0 = pass, 1 = fail, 77 = skip (cargo unavailable).

set -euo pipefail

GITEA_ORG="${GITEA_ORG:-}"
GITEA_URL="${GITEA_URL:-http://localhost:3000}"
GITEA_ADMIN_TOKEN="${GITEA_ADMIN_TOKEN:-}"

CRATE_NAME="smoke-test-$$"
CRATE_VERSION="0.0.1"

log()  { printf '[smoke-registry] %s\n' "$*"; }
fail() { printf '[smoke-registry] ERROR: %s\n' "$*" >&2; exit 1; }

[ -n "$GITEA_ORG" ] || fail "set GITEA_ORG to the org namespace to test"
[ -n "$GITEA_ADMIN_TOKEN" ] || fail "GITEA_ADMIN_TOKEN is required"
command -v cargo >/dev/null 2>&1 || { log "cargo not available — skipping"; exit 77; }

WORK="$(mktemp -d /tmp/gitea-smoke-XXXXXX)"
cleanup() {
  # best-effort: remove the published crate version and the temp dir.
  # NB: package *deletion* is the management API (/api/v1/packages/...), NOT the
  # registry-download namespace (/api/packages/...) used for publish/resolve.
  curl -s -o /dev/null -X DELETE -H "Authorization: token $GITEA_ADMIN_TOKEN" \
    "$GITEA_URL/api/v1/packages/$GITEA_ORG/cargo/$CRATE_NAME/$CRATE_VERSION" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

export CARGO_HOME="$WORK/cargohome"
mkdir -p "$CARGO_HOME"
cat > "$CARGO_HOME/config.toml" <<EOF
[registries.$GITEA_ORG]
index = "sparse+$GITEA_URL/api/packages/$GITEA_ORG/cargo/"
EOF
# The token MUST be stored as "Bearer <token>"; a bare token yields HTTP 401.
cat > "$CARGO_HOME/credentials.toml" <<EOF
[registries.$GITEA_ORG]
token = "Bearer $GITEA_ADMIN_TOKEN"
EOF

# --- generic registry round-trip (the .slpkg home) -------------------------
# NB: Gitea's generic upload needs a raw body (--upload-file); a urlencoded
# body (curl --data) is rejected with HTTP 500.
log "generic: PUT → GET → DELETE round-trip"
GEN="smoke-$$/0.0.1/probe.txt"
printf 'smoke' > "$WORK/gen-probe.txt"
code="$(curl -s -o /dev/null -w '%{http_code}' -X PUT \
  -H "Authorization: token $GITEA_ADMIN_TOKEN" --upload-file "$WORK/gen-probe.txt" \
  "$GITEA_URL/api/packages/$GITEA_ORG/generic/$GEN")"
[ "$code" = "201" ] || fail "generic PUT returned HTTP $code"
body="$(curl -s "$GITEA_URL/api/packages/$GITEA_ORG/generic/$GEN")"
[ "$body" = "smoke" ] || fail "generic GET returned unexpected body: '$body'"
curl -s -o /dev/null -X DELETE -H "Authorization: token $GITEA_ADMIN_TOKEN" \
  "$GITEA_URL/api/packages/$GITEA_ORG/generic/$GEN"
log "generic OK"

# --- cargo publish → resolve-by-version → remove ---------------------------
cd "$WORK"
cargo new --lib --vcs none "$CRATE_NAME" -q
cd "$CRATE_NAME"
cat > Cargo.toml <<EOF
[package]
name = "$CRATE_NAME"
version = "$CRATE_VERSION"
edition = "2021"
description = "Throwaway smoke-test crate for a StreamLib Gitea registry namespace."
license = "BUSL-1.1"

[dependencies]
EOF

log "cargo: publishing $CRATE_NAME v$CRATE_VERSION to '$GITEA_ORG'"
cargo publish --registry "$GITEA_ORG" --no-verify --allow-dirty >/dev/null 2>&1 \
  || fail "cargo publish failed"

# resolve by version: sparse-index metadata entry + .crate download.
# Sparse index sharding has length-based cases (1/2/3/4+ char names); CRATE_NAME
# is always "smoke-test-<pid>" (4+ chars), which shards as <ab>/<cd>/<name>.
# This only implements that branch — correct for the fixed prefix above.
p1="${CRATE_NAME:0:2}"; p2="${CRATE_NAME:2:2}"
meta="$(curl -s "$GITEA_URL/api/packages/$GITEA_ORG/cargo/$p1/$p2/$CRATE_NAME")"
printf '%s' "$meta" | grep -q "\"vers\":\"$CRATE_VERSION\"" \
  || fail "sparse index entry missing version $CRATE_VERSION: $meta"
code="$(curl -s -o "$WORK/dl.crate" -w '%{http_code}' \
  "$GITEA_URL/api/packages/$GITEA_ORG/cargo/api/v1/crates/$CRATE_NAME/$CRATE_VERSION/download")"
[ "$code" = "200" ] || fail "crate download returned HTTP $code"
tar tzf "$WORK/dl.crate" >/dev/null 2>&1 || fail "downloaded .crate is not a valid tarball"
log "cargo resolve-by-version OK (index entry + .crate download)"

# remove it (also handled by the EXIT trap, but assert success here).
# Deletion is the management API (/api/v1/packages/...), distinct from the
# /api/packages/... registry namespace used to publish and resolve above.
code="$(curl -s -o /dev/null -w '%{http_code}' -X DELETE \
  -H "Authorization: token $GITEA_ADMIN_TOKEN" \
  "$GITEA_URL/api/v1/packages/$GITEA_ORG/cargo/$CRATE_NAME/$CRATE_VERSION")"
[ "$code" = "204" ] || fail "crate delete returned HTTP $code"
log "cargo remove OK"

log "PASS — registry publish/resolve/remove smoke test green"

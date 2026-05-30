#!/bin/bash
# Provision a Gitea registry namespace for a self-hosted StreamLib package
# registry — your own admin owner + org with all four package registries
# (cargo / pypi / npm / generic) reachable under it.
#
# Point it at any Gitea instance you control and pass your own org / admin via
# the environment; the script bakes in no specific namespace. Standing up a
# StreamLib registry for your team is the same one command whether it runs
# against a local dev container or a hosted Gitea.
#
# Stands up (idempotently):
#   - a surviving admin/owner user (GITEA_ADMIN_USER)
#   - the GITEA_ORG org that owns all four package registries
#   - verifies cargo / pypi / npm / generic registries are reachable under it
#
# The four registries are reachable as soon as the org exists and Gitea's
# package feature is enabled. The cargo registry uses the SPARSE protocol
# (`sparse+<url>/api/packages/<org>/cargo/`), which Gitea serves from the
# package DB — there is NO `_cargo-index` repo to create and NO web-session
# "initialize" step. The first `cargo publish` populates the DB-backed index.
#
# Two modes:
#   API mode (preferred, works against any Gitea including a hosted one):
#     export GITEA_ADMIN_TOKEN=<token for an existing admin>
#     GITEA_ORG=<your-org> ./provision-registry.sh
#   Bootstrap mode (local dev container, no token yet):
#     creates the admin user + token via the container CLI, then proceeds.
#     Requires docker access to $GITEA_CONTAINER. On a host where your shell
#     is not yet in the docker group, run under: sg docker -c '...'.
#
# Env:
#   GITEA_ORG            REQUIRED — the org namespace to stand up
#   GITEA_URL            default http://localhost:3000
#   GITEA_ADMIN_USER     default registry-admin
#   GITEA_ADMIN_TOKEN    if set, API mode; else bootstrap mode
#   GITEA_ADMIN_EMAIL    default ${GITEA_ADMIN_USER}@example.com
#   GITEA_ADMIN_PASSWORD bootstrap mode only; required to create the admin user
#   GITEA_CONTAINER      default gitea (bootstrap mode only)
#   RUN_SMOKE            if "1", run smoke-test-registry.sh at the end
#
# Exit codes: 0 = provisioned & verified, 1 = failure.

set -euo pipefail

GITEA_ORG="${GITEA_ORG:-}"
GITEA_URL="${GITEA_URL:-http://localhost:3000}"
GITEA_ADMIN_USER="${GITEA_ADMIN_USER:-registry-admin}"
GITEA_ADMIN_TOKEN="${GITEA_ADMIN_TOKEN:-}"
GITEA_ADMIN_EMAIL="${GITEA_ADMIN_EMAIL:-${GITEA_ADMIN_USER}@example.com}"
GITEA_ADMIN_PASSWORD="${GITEA_ADMIN_PASSWORD:-}"
GITEA_CONTAINER="${GITEA_CONTAINER:-gitea}"
RUN_SMOKE="${RUN_SMOKE:-0}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

log()  { printf '[provision-registry] %s\n' "$*"; }
fail() { printf '[provision-registry] ERROR: %s\n' "$*" >&2; exit 1; }

[ -n "$GITEA_ORG" ] || fail "set GITEA_ORG to the org namespace you want to stand up"

http_code() {
  # http_code METHOD URL [auth]  -> prints the status code
  local method="$1" url="$2" auth="${3:-}"
  if [ -n "$auth" ]; then
    curl -s -o /dev/null -w '%{http_code}' -X "$method" \
      -H "Authorization: token $GITEA_ADMIN_TOKEN" "$url"
  else
    curl -s -o /dev/null -w '%{http_code}' -X "$method" "$url"
  fi
}

gitea_cli() {
  docker exec -u git "$GITEA_CONTAINER" gitea "$@"
}

bootstrap_admin() {
  command -v docker >/dev/null 2>&1 \
    || fail "no GITEA_ADMIN_TOKEN set and 'docker' unavailable for bootstrap; set GITEA_ADMIN_TOKEN instead"
  [ -n "$GITEA_ADMIN_PASSWORD" ] \
    || fail "bootstrap mode needs GITEA_ADMIN_PASSWORD to create the '$GITEA_ADMIN_USER' admin user"

  if gitea_cli admin user list 2>/dev/null | awk '{print $2}' | grep -qx "$GITEA_ADMIN_USER"; then
    log "Admin user '$GITEA_ADMIN_USER' already exists"
  else
    log "Creating admin user '$GITEA_ADMIN_USER'"
    gitea_cli admin user create \
      --username "$GITEA_ADMIN_USER" \
      --password "$GITEA_ADMIN_PASSWORD" \
      --email "$GITEA_ADMIN_EMAIL" \
      --admin --must-change-password=false >/dev/null \
      || fail "failed to create admin user"
  fi

  log "Generating an access token for '$GITEA_ADMIN_USER'"
  local out
  out="$(gitea_cli admin user generate-access-token \
    --username "$GITEA_ADMIN_USER" \
    --token-name "provision-$(date +%s)" \
    --scopes all 2>&1)" || fail "failed to generate token: $out"
  GITEA_ADMIN_TOKEN="$(printf '%s\n' "$out" | grep -oE '[0-9a-f]{40}' | tail -1)"
  [ -n "$GITEA_ADMIN_TOKEN" ] || fail "could not parse generated token from: $out"
}

ensure_org() {
  local code
  code="$(http_code GET "$GITEA_URL/api/v1/orgs/$GITEA_ORG")"
  if [ "$code" = "200" ]; then
    log "Org '$GITEA_ORG' already exists"
    return
  fi
  log "Creating org '$GITEA_ORG'"
  code="$(curl -s -o /dev/null -w '%{http_code}' -X POST \
    -H "Authorization: token $GITEA_ADMIN_TOKEN" -H 'Content-Type: application/json' \
    "$GITEA_URL/api/v1/orgs" \
    -d "{\"username\":\"$GITEA_ORG\",\"visibility\":\"public\"}")"
  [ "$code" = "201" ] || fail "org creation returned HTTP $code"
}

verify_registries() {
  local ok=1
  # cargo: the computed sparse-index config endpoint must serve this org's URLs
  local cfg
  cfg="$(curl -s "$GITEA_URL/api/packages/$GITEA_ORG/cargo/config.json")"
  if printf '%s' "$cfg" | grep -q "/api/packages/$GITEA_ORG/cargo"; then
    log "cargo    reachable (sparse config.json OK)"
  else
    log "cargo    NOT reachable: $cfg"; ok=0
  fi
  # generic: a definitive write→read→delete round-trip.
  # NB: Gitea's generic upload needs a raw body (--upload-file); a urlencoded
  # body (curl --data) is rejected with HTTP 500.
  local probe="provision-probe/0.0.0/probe.txt" code tmpf
  tmpf="$(mktemp)"; printf 'probe' > "$tmpf"
  code="$(curl -s -o /dev/null -w '%{http_code}' -X PUT \
    -H "Authorization: token $GITEA_ADMIN_TOKEN" \
    --upload-file "$tmpf" "$GITEA_URL/api/packages/$GITEA_ORG/generic/$probe")"
  rm -f "$tmpf"
  if [ "$code" = "201" ]; then
    curl -s -o /dev/null -X DELETE -H "Authorization: token $GITEA_ADMIN_TOKEN" \
      "$GITEA_URL/api/packages/$GITEA_ORG/generic/$probe" || true
    log "generic  reachable (PUT/DELETE round-trip OK)"
  else
    log "generic  NOT reachable: PUT HTTP $code"; ok=0
  fi
  # npm: a present registry returns npm-shaped JSON ({"error":...}) for a
  # missing package; a route-missing path returns Gitea's generic 404 page.
  # Inspect the body, not just the status, so this assertion can actually fail.
  local npm_body
  npm_body="$(curl -s "$GITEA_URL/api/packages/$GITEA_ORG/npm/@$GITEA_ORG%2Fprobe")"
  if printf '%s' "$npm_body" | grep -q '"error"'; then
    log "npm      reachable (npm-shaped empty-index response)"
  else
    log "npm      NOT reachable: ${npm_body:0:80}"; ok=0
  fi
  # pypi: an authed empty upload to a present route is rejected with a 4xx
  # (400 on Gitea 1.22); a missing route returns 404. Accept any non-404 4xx as
  # "route present" so this isn't pinned to one Gitea version's exact code.
  code="$(http_code POST "$GITEA_URL/api/packages/$GITEA_ORG/pypi" auth)"
  if [ "$code" != "404" ] && [ "$code" -ge 400 ] 2>/dev/null && [ "$code" -lt 500 ]; then
    log "pypi     reachable (upload route responds: HTTP $code)"
  else
    log "pypi     NOT reachable: HTTP $code"; ok=0
  fi
  [ "$ok" = "1" ] || fail "one or more registries are not reachable under '$GITEA_ORG'"
}

main() {
  log "Target Gitea: $GITEA_URL  org: $GITEA_ORG  admin: $GITEA_ADMIN_USER"
  # -f fails on HTTP >=400 so an up-but-broken Gitea fails fast here, not later.
  curl -fsS -o /dev/null "$GITEA_URL/api/v1/version" \
    || fail "cannot reach Gitea at $GITEA_URL (no response or error status)"

  if [ -z "$GITEA_ADMIN_TOKEN" ]; then
    log "No GITEA_ADMIN_TOKEN — entering bootstrap mode via container '$GITEA_CONTAINER'"
    bootstrap_admin
  else
    log "Using provided GITEA_ADMIN_TOKEN (API mode)"
  fi

  ensure_org
  verify_registries
  log "Provisioned: org '$GITEA_ORG' with cargo/pypi/npm/generic reachable."

  if [ "$RUN_SMOKE" = "1" ]; then
    log "Running smoke test…"
    GITEA_URL="$GITEA_URL" GITEA_ORG="$GITEA_ORG" GITEA_ADMIN_TOKEN="$GITEA_ADMIN_TOKEN" \
      "$SCRIPT_DIR/smoke-test-registry.sh"
  fi
  log "Done."
}

main "$@"

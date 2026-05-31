#!/usr/bin/env bash
# Publish the `@tatolab/streamlib-deno` SDK to the Gitea npm registry by
# version. The Deno twin of publish-python-sdk.sh / publish-crates.sh: a local
# engine change becomes a published version a Deno consumer resolves from Gitea
# by version (a bare `import "streamlib"` mapped to
# `npm:@tatolab/streamlib-deno`) — never a relative `../../../libs/...` import.
# See docs/architecture/gitea-registry-distribution.md.
#
#   ./publish-deno-sdk.sh            # publish the base [workspace.package].version
#   ./publish-deno-sdk.sh --dev 3    # publish <base>-dev.3
#
# Version is derived from the Rust workspace's [workspace.package].version
# (single source of truth) so the Deno SDK rides the same train as the cargo
# closure. Deno/npm use semver, so the `-dev.N` prerelease grammar matches
# cargo's exactly (unlike Python's PEP 440 `.devN` translation).
#
# Why a BUILT artifact (not source like cargo/pypi): Deno cannot consume a
# `.ts` package through the `npm:` protocol (node_modules → no type-stripping;
# `jsr:` deps → unsupported scheme), so the npm channel ships transpiled JS +
# `.d.ts`. `deno pack` (Deno 2.8+) is the purpose-built tool: it transpiles,
# emits declarations, rewrites import specifiers, and synthesizes package.json
# from the SDK's `deno.json` `exports`. The SDK's `_generated_` escalate
# wire-vocabulary is regenerated fresh and baked into the artifact (it is
# protocol-locked to the SDK version; there is no post-install codegen hook
# for an npm consumer the way Python regenerates into its venv).
#
# Configure-by-env (all optional except the token + user):
#   GITEA_URL              default http://localhost:3300
#   GITEA_ORG              default tatolab
#   GITEA_PUBLISH_TOKEN    REQUIRED — a Gitea token with write:package
#   GITEA_PUBLISH_USER     REQUIRED — the Gitea username the token belongs to
set -euo pipefail

GITEA_URL="${GITEA_URL:-http://localhost:3300}"
GITEA_ORG="${GITEA_ORG:-tatolab}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROJECT="$ROOT/libs/streamlib-deno"
PY="${PYTHON:-python3}"

DEV_N=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dev) DEV_N="${2:?--dev needs a number}"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

log()  { printf '[publish-deno-sdk] %s\n' "$*"; }
fail() { printf '[publish-deno-sdk] ERROR: %s\n' "$*" >&2; exit 1; }

[ -n "${GITEA_PUBLISH_TOKEN:-}" ] || fail "set GITEA_PUBLISH_TOKEN (write:package)"
[ -n "${GITEA_PUBLISH_USER:-}" ]  || fail "set GITEA_PUBLISH_USER (the token's owner)"
command -v deno >/dev/null || fail "deno not found (the SDK's build + pack toolchain)"
command -v npm  >/dev/null || fail "npm not found (publishes the packed tarball)"
# `deno pack` landed in Deno 2.8. Fail loud rather than emit a cryptic
# "Module not found 'pack'" on an older toolchain.
deno_ver="$(deno --version | head -1 | awk '{print $2}')"
case "$deno_ver" in
  2.[0-7].*|1.*|0.*) fail "deno $deno_ver too old — \`deno pack\` needs >= 2.8 (run: deno upgrade)" ;;
esac

# --- derive the target version from the workspace (single source of truth) ----
base_version="$("$PY" - "$ROOT/Cargo.toml" <<'PY'
import sys, tomllib
with open(sys.argv[1], "rb") as f:
    print(tomllib.load(f)["workspace"]["package"]["version"])
PY
)"
target_version="$base_version"
[ -n "$DEV_N" ] && target_version="${base_version}-dev.${DEV_N}"
log "publishing @tatolab/streamlib-deno@$target_version to $GITEA_ORG on $GITEA_URL"

# --- regenerate the escalate wire vocabulary so the artifact is current -------
# `_generated_/` is a build artifact (gitignored); the published JS bakes it in.
# `deno task setup` resolves the SDK's schema deps from the local path patches.
log "regenerating _generated_ (deno task setup)"
( cd "$PROJECT" && deno task setup ) >/dev/null 2>&1 \
  || fail "deno task setup (codegen) failed"

# --- pack the SDK into an npm tarball (built JS + .d.ts + package.json) --------
out="$(mktemp -d)"
tgz="$out/streamlib-deno.tgz"
cleanup() { rm -rf "$out"; }
trap cleanup EXIT
log "packing (deno pack --set-version $target_version)"
( cd "$PROJECT" && deno pack --set-version "$target_version" --allow-dirty -o "$tgz" ) \
  >/dev/null 2>&1 || fail "deno pack failed"
log "packed: $(cd "$out" && echo *.tgz)"

# --- publish to Gitea's npm registry ------------------------------------------
# Gitea npm upload is token auth via a scoped .npmrc. A duplicate version
# returns HTTP 409, which npm surfaces as EPUBLISHCONFLICT — treat as already
# published and continue (idempotent re-runs, mirrors publish-crates.sh).
npm_registry="${GITEA_URL}/api/packages/${GITEA_ORG}/npm/"
# Auth key is the registry URL minus scheme, with a trailing slash.
auth_host="$(printf '%s' "$npm_registry" | sed -E 's#^https?:##')"
npmrc="$out/.npmrc"
cat > "$npmrc" <<EOF
@${GITEA_ORG}:registry=${npm_registry}
${auth_host}:_authToken=${GITEA_PUBLISH_TOKEN}
EOF

# npm refuses to publish a prerelease (`-dev.N`) under the default `latest`
# dist-tag — route dev publishes to a `dev` tag so `latest` keeps pointing at
# the newest stable release.
publish_tag=(--tag latest)
case "$target_version" in *-*) publish_tag=(--tag dev) ;; esac

if out_log="$(npm publish "$tgz" --userconfig "$npmrc" --registry "$npm_registry" "${publish_tag[@]}" 2>&1)"; then
  log "  ✓ @tatolab/streamlib-deno@$target_version published"
elif printf '%s' "$out_log" | grep -qiE 'already exist|conflict|409|EPUBLISHCONFLICT|cannot publish over'; then
  log "  • @tatolab/streamlib-deno@$target_version already present — skipping"
else
  printf '%s\n' "$out_log" >&2
  fail "npm publish failed"
fi

log "done — @tatolab/streamlib-deno@$target_version on ${GITEA_ORG} at ${GITEA_URL}"
log "consumers resolve it via deno.json: \"streamlib\": \"npm:@tatolab/streamlib-deno@^${base_version%%-*}\""
log "  + .npmrc: @${GITEA_ORG}:registry=${npm_registry}"

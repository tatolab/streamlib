#!/usr/bin/env bash
# Smoke-test the Deno SDK npm round-trip: publish the SDK to the Gitea npm
# registry, resolve a bare `import "streamlib"` (+ a subpath) from Gitea in a
# throwaway consumer, run it, and assert it loaded SDK code from the registry.
# Then remove the published version.
#
# This is the executable form of #1118's exit criterion: "a throwaway Deno
# module resolves a bare `import "streamlib"` from Gitea and runs." It needs a
# live Gitea + a Deno toolchain, so it is a local gate (no GPU/registry CI),
# the Deno sibling of smoke-test-registry.sh.
#
# Self-cleaning: publishes a unique throwaway dev version and deletes it.
#
# Env:
#   GITEA_URL              default http://localhost:3300
#   GITEA_ORG              default tatolab
#   GITEA_PUBLISH_TOKEN    REQUIRED — a token with write:package (+ delete)
#   GITEA_PUBLISH_USER     REQUIRED — the token's owner
#
# Exit codes: 0 = pass, 1 = fail, 77 = skip (deno/npm unavailable).
set -euo pipefail

GITEA_URL="${GITEA_URL:-http://localhost:3300}"
GITEA_ORG="${GITEA_ORG:-tatolab}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

log()  { printf '[smoke-deno-sdk] %s\n' "$*"; }
fail() { printf '[smoke-deno-sdk] ERROR: %s\n' "$*" >&2; exit 1; }

[ -n "${GITEA_PUBLISH_TOKEN:-}" ] || fail "set GITEA_PUBLISH_TOKEN (write:package)"
[ -n "${GITEA_PUBLISH_USER:-}" ]  || fail "set GITEA_PUBLISH_USER (the token's owner)"
command -v deno >/dev/null 2>&1 || { log "deno not available — skipping"; exit 77; }
command -v npm  >/dev/null 2>&1 || { log "npm not available — skipping"; exit 77; }

base_version="$(python3 -c "import tomllib;print(tomllib.load(open('$ROOT/Cargo.toml','rb'))['workspace']['package']['version'])")"
# A throwaway dev tier unlikely to collide with real dev publishes.
dev_n="99$$"; dev_n="${dev_n:0:6}"
version="${base_version}-dev.${dev_n}"
npm_registry="${GITEA_URL}/api/packages/${GITEA_ORG}/npm/"
auth_host="$(printf '%s' "$npm_registry" | sed -E 's#^https?:##')"

WORK="$(mktemp -d /tmp/deno-sdk-smoke-XXXXXX)"
cleanup() {
  curl -s -o /dev/null -X DELETE -H "Authorization: token $GITEA_PUBLISH_TOKEN" \
    "$GITEA_URL/api/v1/packages/$GITEA_ORG/npm/@${GITEA_ORG}%2Fstreamlib-deno/$version" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

log "publishing @${GITEA_ORG}/streamlib-deno@$version"
"$ROOT/scripts/gitea/publish-deno-sdk.sh" --dev "$dev_n" >/dev/null 2>&1 \
  || fail "publish-deno-sdk.sh failed"

log "resolving a bare \`import \"streamlib\"\` (+ subpath) from Gitea"
cat > "$WORK/.npmrc" <<EOF
@${GITEA_ORG}:registry=${npm_registry}
${auth_host}:_authToken=${GITEA_PUBLISH_TOKEN}
EOF
cat > "$WORK/deno.json" <<EOF
{ "imports": {
  "streamlib": "npm:@${GITEA_ORG}/streamlib-deno@${version}",
  "streamlib/": "npm:/@${GITEA_ORG}/streamlib-deno@${version}/"
} }
EOF
cat > "$WORK/use.ts" <<'EOF'
import { processor, SchemaIdent } from "streamlib";
import { VulkanContext } from "streamlib/adapters/vulkan.ts";
// Exercise a pure SDK code path to prove the Gitea-resolved module runs.
const id = new SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0");
if (id.toString() !== "@tatolab/core/VideoFrame@1.0.0") throw new Error("SchemaIdent mismatch");
if (typeof processor !== "function") throw new Error("processor not a function");
if (typeof VulkanContext !== "function") throw new Error("VulkanContext not resolved");
console.log("STREAMLIB-FROM-GITEA-OK");
EOF

out="$(cd "$WORK" && deno run --allow-all --reload use.ts 2>&1)" || {
  printf '%s\n' "$out" >&2; fail "consumer failed to resolve/run streamlib from Gitea";
}
printf '%s' "$out" | grep -q "STREAMLIB-FROM-GITEA-OK" \
  || { printf '%s\n' "$out" >&2; fail "expected STREAMLIB-FROM-GITEA-OK marker"; }

log "PASS — bare \`import \"streamlib\"\` resolved from Gitea and ran"

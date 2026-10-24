#!/usr/bin/env bash
# Unit tests for .claude/scripts/attach-artifact.
#
# What must hold: an upload lands under the key the evidence belongs to, with the type it is
# served as; it is reported only once the public URL serves every byte; the token reaches wrangler
# only inside `infisical run` and never the output; a file that would run as a page on our domain,
# a malformed key or a missing file is refused before anything is uploaded.
#
# No network: `infisical`, `npx` (wrangler) and `curl` are stubs on PATH. The `npx` stub records
# each call and keeps "uploaded" bytes in a scratch bucket the `curl` stub serves from.
set -uo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/attach-artifact"
[ -x "$script" ] || { echo "attach-artifact not found or not executable at $script" >&2; exit 2; }

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
stubs="$scratch/stubs"
bucket="$scratch/bucket"
calls="$scratch/calls.log"
mkdir -p "$stubs" "$bucket"

cat >"$stubs/infisical" <<'STUB'
#!/usr/bin/env bash
# `infisical run --env prod --silent -- <cmd>...` with the Tatolab secrets injected.
[ "$1 $2 $3 $4 $5" = "run --env prod --silent --" ] || { echo "unexpected infisical call: $*" >&2; exit 9; }
shift 5
CLOUDFLARE_DEPLOY_API_TOKEN="the-secret-token-value" exec "$@"
STUB
cat >"$stubs/npx" <<'STUB'
#!/usr/bin/env bash
# `npx -y wrangler@4 r2 object put|delete <bucket>/<key> [--file f --content-type t] --remote`.
echo "token=${CLOUDFLARE_API_TOKEN:-unset} $*" >>"$ATTACH_ARTIFACT_TEST_CALLS"
shift 2
[ "$1 $2" = "r2 object" ] || exit 9
verb="$3" object="$4"
key="${object#github-artifacts/}"
case "$verb" in
  put)
    file="" ; while [ "$#" -gt 0 ]; do [ "$1" = "--file" ] && file="$2"; shift; done
    mkdir -p "$ATTACH_ARTIFACT_TEST_BUCKET/$(dirname "$key")"
    if [ -n "${ATTACH_ARTIFACT_TEST_TRUNCATE:-}" ]; then head -c 3 "$file" >"$ATTACH_ARTIFACT_TEST_BUCKET/$key"; else cp "$file" "$ATTACH_ARTIFACT_TEST_BUCKET/$key"; fi
    ;;
  delete) rm -f "$ATTACH_ARTIFACT_TEST_BUCKET/$key" ;;
esac
STUB
cat >"$stubs/curl" <<'STUB'
#!/usr/bin/env bash
# Serves the scratch bucket at https://gh-artifact.tatolab.com: `curl -sI <url>` prints headers,
# `curl -s -o /dev/null -w '%{http_code}' -I <url>` prints the status.
url="${!#}"
key="${url#https://gh-artifact.tatolab.com/}"
object="$ATTACH_ARTIFACT_TEST_BUCKET/$key"
if [ "$*" != "${*/-w/}" ]; then [ -f "$object" ] && printf 200 || printf 404; exit 0; fi
if [ -f "$object" ]; then printf 'HTTP/2 200\r\ncontent-length: %s\r\n\r\n' "$(wc -c <"$object" | tr -d ' ')"; else printf 'HTTP/2 404\r\n\r\n'; fi
STUB
chmod +x "$stubs"/*
echo '{"workspaceId":"test"}' >"$scratch/.infisical.json"
cd "$scratch"
export PATH="$stubs:$PATH" ATTACH_ARTIFACT_TEST_BUCKET="$bucket" ATTACH_ARTIFACT_TEST_CALLS="$calls"

passed=0
failed=0
expect() {
  local name="$1" condition="$2"
  if eval "$condition"; then passed=$((passed + 1)); else failed=$((failed + 1)); echo "FAIL: $name ($condition)" >&2; fi
}

printf 'frame' >"$scratch/frame-0001.png"
printf 'a log line\n' >"$scratch/node.log"
printf '<script></script>' >"$scratch/page.html"

# An image and a log land under the prefix, typed, and are reported with their URL and markdown.
: >"$calls"
out="$("$script" streamlib/pr-2920/verify-live "$scratch/frame-0001.png" "$scratch/node.log" 2>&1)"; status=$?
expect "upload succeeds" '[ "$status" -eq 0 ]'
expect "image url printed" 'grep -qx "https://gh-artifact.tatolab.com/streamlib/pr-2920/verify-live/frame-0001.png" <<<"$out"'
expect "image markdown inlines it" 'grep -qF "![frame-0001.png](https://gh-artifact.tatolab.com/streamlib/pr-2920/verify-live/frame-0001.png)" <<<"$out"'
expect "log markdown links it" 'grep -qF "[node.log](https://gh-artifact.tatolab.com/streamlib/pr-2920/verify-live/node.log)" <<<"$out"'
expect "image typed image/png" 'grep -q "put github-artifacts/streamlib/pr-2920/verify-live/frame-0001.png .*--content-type image/png --remote" "$calls"'
expect "log typed as utf-8 text" 'grep -q "node.log .*--content-type text/plain; charset=utf-8" "$calls"'
expect "the token reaches wrangler" 'grep -q "^token=the-secret-token-value " "$calls"'
expect "the token never reaches the output" '! grep -q "the-secret-token-value" <<<"$out"'
expect "the bytes are served" 'cmp -s "$scratch/frame-0001.png" "$bucket/streamlib/pr-2920/verify-live/frame-0001.png"'

# A public URL that serves fewer bytes than the file is a failure, not a success.
out="$(ATTACH_ARTIFACT_TEST_TRUNCATE=1 "$script" streamlib/pr-2920/short "$scratch/node.log" 2>&1)"; status=$?
expect "a short served object fails" '[ "$status" -ne 0 ] && grep -q "serves 3 bytes, not the 11" <<<"$out"'

# Refusals happen before anything is uploaded.
: >"$calls"
out="$("$script" streamlib/pr-2920/x "$scratch/page.html" 2>&1)"; status=$?
expect "an html page is refused" '[ "$status" -ne 0 ] && grep -q "would run as a page or script" <<<"$out"'
out="$("$script" Streamlib/PR-1 "$scratch/node.log" 2>&1)"; status=$?
expect "an uppercase prefix is refused" '[ "$status" -ne 0 ] && grep -q "is not <repo>/<pr-N|issue-N>/<topic>" <<<"$out"'
out="$("$script" streamlib "$scratch/node.log" 2>&1)"; status=$?
expect "a one-segment prefix is refused" '[ "$status" -ne 0 ]'
out="$("$script" streamlib/pr-1 "$scratch/node.log" "$scratch/missing.png" 2>&1)"; status=$?
expect "a missing file refuses the whole batch" '[ "$status" -ne 0 ] && grep -q "missing.png is not a file" <<<"$out"'
expect "no refused call uploaded anything" '[ ! -s "$calls" ]'

# Removing deletes the object and checks the URL no longer answers.
out="$("$script" --remove streamlib/pr-2920/verify-live/frame-0001.png 2>&1)"; status=$?
expect "remove succeeds" '[ "$status" -eq 0 ] && grep -q "^removed https://gh-artifact.tatolab.com/streamlib/pr-2920/verify-live/frame-0001.png$" <<<"$out"'
expect "the object is gone" '[ ! -e "$bucket/streamlib/pr-2920/verify-live/frame-0001.png" ]'

printf 'video' >"$scratch/Decision1-TakenName.mp4"
"$script" tatolab-app/pr-57/decision-videos "$scratch/Decision1-TakenName.mp4" >/dev/null 2>&1
out="$("$script" --remove tatolab-app/pr-57/decision-videos/Decision1-TakenName.mp4 2>&1)"; status=$?
expect "a key with an uppercase file name is removed" '[ "$status" -eq 0 ] && [ ! -e "$bucket/tatolab-app/pr-57/decision-videos/Decision1-TakenName.mp4" ]'

# Outside a checkout linked to Infisical it says how to link one, and uploads nothing.
mkdir -p /tmp/attach-artifact-unlinked-$$ && printf 'x' >/tmp/attach-artifact-unlinked-$$/a.txt
out="$(cd /tmp/attach-artifact-unlinked-$$ && "$script" streamlib/pr-1 a.txt 2>&1)"; status=$?
rm -rf /tmp/attach-artifact-unlinked-$$
expect "an unlinked checkout is told to run infisical init" '[ "$status" -ne 0 ] && grep -q "infisical init" <<<"$out"'

expect "--url prints the public URL" '[ "$("$script" --url streamlib/pr-1/a.png)" = "https://gh-artifact.tatolab.com/streamlib/pr-1/a.png" ]'

echo "attach-artifact: $passed passed, $failed failed"
[ "$failed" -eq 0 ]

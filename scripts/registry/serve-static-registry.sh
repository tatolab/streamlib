#!/usr/bin/env bash
# Serve an emitted static registry tree with a dumb static HTTP server
# (`python3 -m http.server`) and print the consumer configuration.
# No custom server is built — cargo/npm just need a directory-index HTTP mount;
# slpkg/pypi read straight off `file://`.
#
#   ./serve-static-registry.sh <tree_dir> [--port 8799]
#
# The tree_dir is what `cargo xtask static-registry emit --out <dir>` produced.
# The --port MUST match the emit's --base-url port (the cargo config.json + npm
# packument bake in that absolute URL).
#
# For the ergonomic path, prefer `streamlib registry use <tree_dir>` — it writes
# the cargo `[source]` replacement + `.npmrc` into the consumer's config and
# auto-serves npm on localhost. This script is the manual equivalent.
set -euo pipefail

DIR=""
PORT="8799"
while [ $# -gt 0 ]; do
  case "$1" in
    --port) PORT="${2:?--port needs a number}"; shift 2 ;;
    -*) echo "unknown arg: $1" >&2; exit 2 ;;
    *) if [ -z "$DIR" ]; then DIR="$1"; else echo "unexpected arg: $1" >&2; exit 2; fi; shift ;;
  esac
done
[ -n "$DIR" ] || { echo "usage: serve-static-registry.sh <tree_dir> [--port N]" >&2; exit 2; }
[ -d "$DIR" ] || { echo "no such tree dir: $DIR" >&2; exit 1; }
DIR="$(cd "$DIR" && pwd)"

log() { printf '[serve-static-registry] %s\n' "$*"; }

log "serving $DIR at http://127.0.0.1:$PORT  (Ctrl-C to stop)"
cat <<EOF

# ── consumer configuration ────────────────────────────────────────────────────
# .slpkg generic store + in-process schema codegen (file://, tree root):
export STREAMLIB_REGISTRY_URL="file://$DIR"

# pypi (file://, PEP-503 simple):
export UV_INDEX="file://$DIR/pypi/simple"

# cargo — a source replacement keeps the canonical source id in Cargo.lock while
# resolving from this local mount. Add to the consumer's .cargo/config.toml:
#   [source.tatolab]
#   registry = "sparse+https://registry.tatolab.com/cargo/"
#   replace-with = "tatolab-local"
#   [source.tatolab-local]
#   registry = "sparse+http://127.0.0.1:$PORT/cargo/"

# npm (static HTTP mount) — add to .npmrc:
#   @tatolab:registry=http://127.0.0.1:$PORT/npm/
# ──────────────────────────────────────────────────────────────────────────────

EOF

exec python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$DIR"

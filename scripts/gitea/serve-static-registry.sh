#!/usr/bin/env bash
# Serve an emitted static registry tree with a dumb static HTTP server
# (`python3 -m http.server`) and print the consumer env for all four ecosystems.
# No custom server is built — cargo/npm just need a directory-index HTTP mount;
# slpkg/pypi read straight off `file://`.
#
#   ./serve-static-registry.sh <tree_dir> [--port 8799]
#
# The tree_dir is what `cargo xtask static-registry emit --out <dir>` produced.
# The --port MUST match the emit's --base-url port (the cargo config.json + npm
# packument bake in that absolute URL).
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

# ── consumer env (both the in-venv codegen channel AND the ecosystem clients) ──
# .slpkg generic store + in-process schema codegen  (file://):
export STREAMLIB_REGISTRY_URL="file://$DIR/slpkg"
export STREAMLIB_REGISTRY_TOKEN=""            # reads are tokenless

# pypi (file://, PEP-503 simple):
export UV_INDEX="file://$DIR/pypi/simple"

# cargo (static HTTP mount — sparse is HTTP-only):
export CARGO_REGISTRIES_GITEA_INDEX="sparse+http://127.0.0.1:$PORT/cargo/"

# npm (static HTTP mount) — add to .npmrc:
#   @tatolab:registry=http://127.0.0.1:$PORT/npm/
# ──────────────────────────────────────────────────────────────────────────────

EOF

exec python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$DIR"

#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Reshape a cargo SPARSE subtree (the one the xtask `static-registry emit`
# generator produces under <sparse>/cargo/) into a cargo
# LOCAL-REGISTRY tree — the serverless, `file://`-resolvable source cargo can
# use as a `[source]` replacement with NO running HTTP server.
#
#   ./emit-cargo-local-registry.sh <sparse-tree-root> <lr-out>
#
#   <sparse-tree-root>  the dir that CONTAINS `cargo/` (e.g. what `--out`
#                       pointed at). The sparse index is <root>/cargo/<shard>/<name>
#                       and the tarballs are <root>/cargo/crates/<name>/<name>-<ver>.crate.
#   <lr-out>            the local-registry dir to (re)populate.
#
# The two on-disk shapes differ only in layout, not content:
#
#   SPARSE:  cargo/config.json                       (dl/api template — dropped)
#            cargo/<shard>/<name>                     (NDJSON index, carries cksum)
#            cargo/crates/<name>/<name>-<ver>.crate   (tarballs, nested by crate)
#
#   LOCAL:   index/<shard>/<name>                     (SAME NDJSON index lines)
#            <name>-<ver>.crate                       (SAME tarballs, FLAT at root)
#
# A cargo sparse index NDJSON line is byte-identical to a local-registry index
# line (both carry `cksum`), so the reshape is a pure copy/flatten: no
# config.json, no git index, no per-crate `.cargo-checksum.json`. cargo verifies
# each `.crate` against the index-line `cksum` at extraction time.
#
# Idempotent: safe to re-run after a subsequent closure emit adds more crates to
# the same sparse tree (the reshape re-mirrors the whole tree). No server, no
# cargo invocation — deterministic file copies only.
set -euo pipefail

SPARSE_ROOT="${1:?usage: emit-cargo-local-registry.sh <sparse-tree-root> <lr-out>}"
LR_OUT="${2:?usage: emit-cargo-local-registry.sh <sparse-tree-root> <lr-out>}"

log()  { printf '[emit-cargo-local-registry] %s\n' "$*"; }
fail() { printf '[emit-cargo-local-registry] ERROR: %s\n' "$*" >&2; exit 1; }

CARGO_DIR="$SPARSE_ROOT/cargo"
[ -d "$CARGO_DIR" ] || fail "no cargo/ subtree under $SPARSE_ROOT (expected $CARGO_DIR)"

mkdir -p "$LR_OUT/index"

# --- index: every sparse-index file EXCEPT config.json and the crates/ subtree
# copies verbatim to <lr>/index/<same-relpath>. The shard path grammar
# (se/rd/serde, 3/x/xxx, ...) is preserved as-is — local-registry reads the same
# grammar.
index_count=0
while IFS= read -r -d '' idx_file; do
  rel="${idx_file#"$CARGO_DIR"/}"
  dest="$LR_OUT/index/$rel"
  mkdir -p "$(dirname "$dest")"
  cp -f "$idx_file" "$dest"
  index_count=$((index_count + 1))
done < <(find "$CARGO_DIR" -type f \
            -not -name config.json \
            -not -path "$CARGO_DIR/crates/*" -print0)

# --- tarballs: every crates/<name>/<name>-<ver>.crate copies FLAT to <lr>/<basename>.
crate_count=0
if [ -d "$CARGO_DIR/crates" ]; then
  while IFS= read -r -d '' crate_file; do
    cp -f "$crate_file" "$LR_OUT/$(basename "$crate_file")"
    crate_count=$((crate_count + 1))
  done < <(find "$CARGO_DIR/crates" -type f -name '*.crate' -print0)
fi

[ "$index_count" -gt 0 ] || fail "no index files found under $CARGO_DIR — sparse tree empty?"
[ "$crate_count" -gt 0 ] || fail "no .crate tarballs found under $CARGO_DIR/crates"

log "reshaped $index_count index file(s) + $crate_count crate tarball(s) → $LR_OUT (local-registry, serverless)"

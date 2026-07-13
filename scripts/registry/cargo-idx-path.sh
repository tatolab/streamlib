#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Cargo sparse-index path grammar — the single bash implementation, sourceable
# by registry shell tooling and probeable standalone for the
# cross-implementation golden test against Rust's
# `streamlib_pack::static_registry::cargo_index_path`:
#
#   ./cargo-idx-path.sh <crate-name>     # prints e.g. `vu/lk/vulkanalia`
#
# Grammar: 1-char name → `1/<n>`, 2-char → `2/<n>`, 3-char → `3/<n[0]>/<n>`,
# 4+ chars → `<n[0..2]>/<n[2..4]>/<n>` (lowercased).
set -euo pipefail

cargo_idx_path() {
  local n
  n="$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
  case ${#n} in
    1) printf '1/%s' "$n" ;;
    2) printf '2/%s' "$n" ;;
    3) printf '3/%s/%s' "${n:0:1}" "$n" ;;
    *) printf '%s/%s/%s' "${n:0:2}" "${n:2:2}" "$n" ;;
  esac
}

# Standalone invocation prints the path for $1; sourcing only defines the fn.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  [ $# -eq 1 ] || { echo "usage: cargo-idx-path.sh <crate-name>" >&2; exit 2; }
  cargo_idx_path "$1"
  printf '\n'
fi

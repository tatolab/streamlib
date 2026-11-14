#!/usr/bin/env bash
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
#
# Regenerates a project's THIRD-PARTY-NOTICES.md: its hand-kept preamble, then
# `cargo about generate` over its Rust closure, then its hand-kept appendix of
# projects cargo-about cannot see (vendored C/C++ sources, prebuilt libraries a
# wheel carries, sources a build script fetches), when it has one.
#
# Needs cargo-about (`cargo install --locked cargo-about --features cli`) and
# the network for crates that ship no licence file of their own. Not a CI gate:
# the licence check on every PR is `cargo deny check licenses`.
#
# Usage:
#   scripts/generate-third-party-notices.sh                     # the engine workspace
#   scripts/generate-third-party-notices.sh packages/<package>  # an extension wheel
set -euo pipefail

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
project_directory="${1:-.}"
cd "$workspace_root"

notices_file="$project_directory/THIRD-PARTY-NOTICES.md"
preamble_file="$project_directory/THIRD-PARTY-NOTICES.preamble.md"
appendix_file="$project_directory/THIRD-PARTY-NOTICES.appendix.md"

# Every project shares the workspace's about.toml and about.hbs, so one repo
# takes one licensing posture. --all-features reads the graph deny.toml checks.
generated_licence_texts="$(
  cargo about generate --locked --all-features \
    --config about.toml \
    --manifest-path "$project_directory/Cargo.toml" \
    about.hbs
)"

{
  cat "$preamble_file"
  printf '%s\n' "$generated_licence_texts"
  if [[ -f "$appendix_file" ]]; then
    printf '\n\n'
    cat "$appendix_file"
  fi
} > "$notices_file.partial"
mv "$notices_file.partial" "$notices_file"

echo "wrote $notices_file"

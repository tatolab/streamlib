#!/usr/bin/env bash
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
#
# Prints one digest of everything a runtime unit is built from, for `ci.yml` to
# key the unit's cache on. A pull request that changes none of it — an extension,
# the integration suite, the stream package's tests — runs on the unit `main`
# already built instead of compiling the engine again.
#
# The digest reads the git index (`git ls-files -s`: each file's blob id), so it
# names content, not timestamps. It is built by exclusion: a tree is left out
# only when nothing in a runtime unit is compiled or packed from it, so a new
# directory lands in the digest by default. `rustc -Vv` names the compiler,
# beside `rust-toolchain.toml`'s pin.
#
# Usage: runtime_unit_input_digest.sh <extra words folded into the digest>...

set -euo pipefail

# macOS ships `shasum`; GNU systems ship `sha256sum`.
sha256_of_standard_input() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum
  else
    shasum -a 256
  fi
}

{
  git ls-files -s -- . \
    ':(exclude)docs/**' \
    ':(exclude).claude/**' \
    ':(exclude)examples/**' \
    ':(exclude)docker/**' \
    ':(exclude)installer/**' \
    ':(exclude)tests/**' \
    ':(exclude)packages/**' \
    ':(exclude)sdk/tatolab-pypi-placeholder/**' \
    ':(exclude)sdk/tatolab-stream/**' \
    ':(exclude).github/ISSUE_TEMPLATE/**' \
    ':(exclude)*.md'
  # The runtime compiles the scaffold `tatolab new` writes, the wheel ships the
  # notices, and the unit carries the stream package and its core modules as
  # wheels and the Core Nodes project as a folder; each sits under an exclusion
  # above.
  git ls-files -s -- 'sdk/tatolab-stream/**' ':(exclude)sdk/tatolab-stream/tests/**' 'THIRD-PARTY-NOTICES.md' 'sdk/streamlib-python-wheel/THIRD-PARTY-NOTICES.md' 'packages/tatolab-testpattern/**' 'packages/tatolab-compositor/**' 'packages/tatolab-core-nodes/**' ':(exclude)packages/tatolab-core-nodes/tests/**'
  rustc -Vv
  printf '%s\n' "$@"
} | sha256_of_standard_input | cut -d ' ' -f 1

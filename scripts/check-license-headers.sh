#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Every Rust and Python source file carries the BUSL header. Run from the
# workspace root, by `.github/workflows/repo-gates.yml` and by
# `cargo xtask run-local-ci-gates` alike — one implementation, so a green local
# run means a green CI run.
#
# File discovery is `git ls-files`, not `find`. CI walks a clean checkout, so
# "the files in the repo" is the semantics the gate has always meant; a `find`
# reproduces it on the runner and then reports thousands of false positives on a
# developer machine, where venvs, uv caches and build output sit in the tree.
# `--others --exclude-standard` adds new-but-unstaged files, so a header missing
# from a file you just created fails here rather than on the PR.

set -uo pipefail

list_repo_files() {
  git ls-files -z --cached --others --exclude-standard -- "$@"
}

# A `#!` interpreter line may legitimately occupy line 1; the copyright header
# then sits on line 2.
#
# Both lines are checked, and as whole lines. Checking only the copyright line —
# which is what this gate did for its whole life before 2026-08-12 — passes a
# file that carries the copyright but no SPDX identifier, and SPDX is the half a
# licence scanner actually reads.
report_files_missing_header() {
  local expected_header="$1"
  local expected_spdx="${expected_header%%Copyright*}SPDX-License-Identifier: BUSL-1.1"
  local language="$2"
  shift 2

  local missing_files=()
  local file
  local header_line_number
  while IFS= read -r -d '' file; do
    if head -1 "$file" | grep -q '^#!'; then
      header_line_number=2
    else
      header_line_number=1
    fi
    if ! sed -n "${header_line_number}p" "$file" | grep -qxF "$expected_header" ||
      ! sed -n "$((header_line_number + 1))p" "$file" | grep -qxF "$expected_spdx"; then
      missing_files+=("$file")
    fi
  done < <(list_repo_files "$@")

  if [ ${#missing_files[@]} -ne 0 ]; then
    echo "❌ ${#missing_files[@]} $language file(s) are missing the required copyright header:"
    echo ""
    printf '  - %s\n' "${missing_files[@]}"
    echo ""
    echo "Required header (lines 1-2, or lines 2-3 after a shebang):"
    echo "  $expected_header"
    echo "  $expected_spdx"
    return 1
  fi

  echo "✅ All $language files carry the required copyright header"
  return 0
}

failed_language_checks=()

# Every Rust file in the repo, not an enumerated set of zone dirs. The old list
# named runtime/ sdk/ adapters/ vendor/ examples/, which silently exempted
# `xtask/` and `tools/` — and `xtask/src/check_no_inventory_submit.rs` had been
# sitting there with a `2026` copyright line the rule does not permit.
#
# The vendored trees are verbatim third-party copies under their own licences
# and deliberately carry NO BUSL headers — the vulkanalia fork is Apache-2.0
# (docs/architecture/vendored-vulkanalia.md), the MoQ wheel's moq-transport is
# MIT OR Apache-2.0 under Cloudflare's SPDX headers. One exception, listed by
# path; see CLAUDE.md's licensing section.
#
# Exact-dir exclusions, so a sibling one path segment away is still covered: a
# future vendor/tatolab-vulkanalia-extras/, or a second crate vendored beside
# moq-transport, would NOT be excluded. Rust only — neither tree ships a `.py`,
# so the Python check below buys its lack of exemptions for free.
report_files_missing_header \
  "// Copyright (c) 2025 Jonathan Fontanez" Rust \
  '*.rs' \
  ':(exclude)vendor/tatolab-vulkanalia/*' \
  ':(exclude)vendor/tatolab-vulkanalia-sys/*' \
  ':(exclude)vendor/tatolab-vulkanalia-vma/*' \
  ':(exclude)packages/streamlib-moq/vendor/moq-transport/*' ||
  failed_language_checks+=("Rust")

report_files_missing_header \
  "# Copyright (c) 2025 Jonathan Fontanez" Python \
  '*.py' ||
  failed_language_checks+=("Python")

if [ ${#failed_language_checks[@]} -ne 0 ]; then
  echo ""
  echo "❌ License header check failed for: ${failed_language_checks[*]}"
  exit 1
fi

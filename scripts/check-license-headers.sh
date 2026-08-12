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
report_files_missing_header() {
  local expected_header="$1"
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
    if ! sed -n "${header_line_number}p" "$file" | grep -qF "$expected_header"; then
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
    echo "  ${expected_header%%Copyright*}SPDX-License-Identifier: BUSL-1.1"
    return 1
  fi

  echo "✅ All $language files carry the required copyright header"
  return 0
}

failed_language_checks=()

# The three vendored vulkanalia fork dirs are Apache-2.0 verbatim copies and
# deliberately carry NO BUSL headers — see
# docs/architecture/vendored-vulkanalia.md and CLAUDE.md's licensing exception.
# Exact-dir exclusions (a future vendor/tatolab-vulkanalia-extras/ crate would
# NOT be excluded).
report_files_missing_header \
  "// Copyright (c) 2025 Jonathan Fontanez" Rust \
  'runtime/*.rs' 'sdk/*.rs' 'adapters/*.rs' 'vendor/*.rs' 'examples/*.rs' \
  ':(exclude)vendor/tatolab-vulkanalia/*' \
  ':(exclude)vendor/tatolab-vulkanalia-sys/*' \
  ':(exclude)vendor/tatolab-vulkanalia-vma/*' ||
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

#!/usr/bin/env bash
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
#
# The structure lints from tatolab/lint-config, pinned at LINT_CONFIG_COMMIT: its
# ast-grep rules (through sgconfig.yml), the 1,000-line file-size limit, the
# module-file convention and the workspace lint table. Findings not fixed yet are
# listed under lint-exceptions/, and each list may only shrink. Run from the
# workspace root by `.github/workflows/ci.yml`'s lint job and by
# `cargo xtask run-local-ci-gates`. `--compare-to <git-ref>` also fails on an
# exception list that grew since that ref.
#
# Needs curl, unzip and Python 3.11 or newer.

set -uo pipefail

LINT_CONFIG_COMMIT=b35e3f2f6a3b5c1e0ec56ca1327bfa0a2a230194

compare_to_arguments=()
if [[ "${1:-}" == "--compare-to" && -n "${2:-}" ]]; then
  compare_to_arguments=(--compare-to "$2")
fi

if ! python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))'; then
  echo "run-structure-lints.sh needs Python 3.11 or newer as python3" >&2
  exit 1
fi

lint_config_directory="target/lint-config/${LINT_CONFIG_COMMIT}"
if [[ ! -f "${lint_config_directory}/package.json" ]]; then
  mkdir -p "${lint_config_directory}"
  curl -sSfL --retry 3 "https://codeload.github.com/tatolab/lint-config/tar.gz/${LINT_CONFIG_COMMIT}" \
    | tar -xz --strip-components=1 -C "${lint_config_directory}" || exit 1
fi
ln -sfn "${LINT_CONFIG_COMMIT}" target/lint-config/pinned

ast_grep_binary="$(bash scripts/install-ast-grep.sh)" || exit 1
ast_grep_directory="$(cd "$(dirname "${ast_grep_binary}")" && pwd)"

failed_lints=()
run_lint() {
  local lint_name="$1"
  shift
  if "$@"; then
    echo "PASS  ${lint_name}"
  else
    echo "FAIL  ${lint_name}" >&2
    failed_lints+=("${lint_name}")
  fi
}

run_lint "ast-grep rules" env PATH="${ast_grep_directory}:${PATH}" \
  python3 "${lint_config_directory}/ast-grep/check_ast_grep_scan.py" \
  --exceptions lint-exceptions/ast-grep.txt ${compare_to_arguments[@]+"${compare_to_arguments[@]}"}
run_lint "file size" python3 "${lint_config_directory}/file-size/check_file_sizes.py" \
  --exceptions lint-exceptions/file-size.txt \
  ${compare_to_arguments[@]+"${compare_to_arguments[@]}"}
run_lint "module files" python3 "${lint_config_directory}/rust/check_module_files.py" \
  --exceptions lint-exceptions/module-files.txt ${compare_to_arguments[@]+"${compare_to_arguments[@]}"}
run_lint "workspace lint table" python3 "${lint_config_directory}/rust/check_lint_table.py"

if (( ${#failed_lints[@]} > 0 )); then
  echo "${#failed_lints[@]} structure lints failed: ${failed_lints[*]}" >&2
  exit 1
fi

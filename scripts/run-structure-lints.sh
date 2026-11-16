#!/usr/bin/env bash
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
#
# The structure lints from tatolab/lint-config, pinned at LINT_CONFIG_COMMIT, and
# this repository's own: the ast-grep rules (through sgconfig.yml), their tests,
# the suppressions they allow and the matches they require, the 1,000-line
# file-size limit, the module-file convention and the workspace lint table.
# Findings not fixed yet are listed under lint-exceptions/, and each list may only
# shrink from the one at the compared ref: `--compare-to <git-ref>`, or
# origin/main when none is given and it exists. A list the ref doesn't have yet is
# not compared. Run by
# `.github/workflows/ci.yml`'s lint job and by `cargo xtask run-local-ci-gates`;
# `bash scripts/run-structure-lints.sh` is the entry point, not a bare
# `ast-grep scan`. lint-config's rules are tested in its CI; the rules in
# ast-grep-rules/ are tested here.
#
# Needs curl, tar, unzip, and Python 3.11 or newer as python3 or through uv.

set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

LINT_CONFIG_COMMIT=79020cc823c1f21b1c68747fb1bc9f623ba6df54

compared_ref=""
case "$#" in
  0) git rev-parse --verify -q origin/main >/dev/null && compared_ref=origin/main ;;
  2) [[ "$1" == "--compare-to" ]] && compared_ref="$2" ;;
esac
if [[ "$#" -ne 0 && -z "${compared_ref}" ]]; then
  echo "usage: run-structure-lints.sh [--compare-to <git-ref>]" >&2
  exit 2
fi
if [[ -n "${compared_ref}" ]] && ! git rev-parse --verify -q "${compared_ref}^{commit}" >/dev/null; then
  echo "run-structure-lints.sh: ${compared_ref} is not a commit to compare the exception lists to" >&2
  exit 2
fi

if python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' 2>/dev/null; then
  python_command=(python3)
elif command -v uv >/dev/null; then
  python_command=(uv run --quiet --no-project --python 3.12 python)
else
  echo "run-structure-lints.sh needs Python 3.11 or newer as python3, or uv to provide one" >&2
  exit 1
fi

lint_config_directory="target/lint-config/${LINT_CONFIG_COMMIT}"
if [[ ! -d "${lint_config_directory}" ]]; then
  mkdir -p target/lint-config
  download_directory="$(mktemp -d target/lint-config/download.XXXXXX)" || exit 1
  trap 'rm -rf "${download_directory}"' EXIT
  curl -sSfL --retry 3 "https://codeload.github.com/tatolab/lint-config/tar.gz/${LINT_CONFIG_COMMIT}" \
    | tar -xz --strip-components=1 -C "${download_directory}" || exit 1
  mv "${download_directory}" "${lint_config_directory}" || exit 1
fi
ln -sfn "${LINT_CONFIG_COMMIT}" target/lint-config/pinned || exit 1

ast_grep_binary="$(bash scripts/install-ast-grep.sh)" || exit 1
ast_grep_directory="$(cd "$(dirname "${ast_grep_binary}")" && pwd)" || exit 1

compare_arguments_for() {
  if [[ -n "${compared_ref}" ]] && git cat-file -e "${compared_ref}:$1" 2>/dev/null; then
    echo "--compare-to ${compared_ref}"
  fi
}

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

# ast-grep skips the hidden directories it walks into, so .github is named. The
# required-match check scans the same paths, so dropping one fails its keeps-wired rules.
ast_grep_scan_paths=(. .github)
ast_grep_scan_path_arguments=()
for ast_grep_scan_path in "${ast_grep_scan_paths[@]}"; do
  ast_grep_scan_path_arguments+=(--path "${ast_grep_scan_path}")
done

# shellcheck disable=SC2046 # compare_arguments_for prints nothing or two words.
run_lint "ast-grep rules" env PATH="${ast_grep_directory}:${PATH}" "${python_command[@]}" \
  "${lint_config_directory}/ast-grep/check_ast_grep_scan.py" \
  --exceptions lint-exceptions/ast-grep.txt "${ast_grep_scan_path_arguments[@]}" \
  $(compare_arguments_for lint-exceptions/ast-grep.txt)
run_lint "ast-grep rule tests" env PATH="${ast_grep_directory}:${PATH}" \
  ast-grep test --skip-snapshot-tests
for script_test in scripts/test_check_ast_grep_suppressions.py scripts/test_check_ast_grep_required_matches.py; do
  run_lint "${script_test}" "${python_command[@]}" "${script_test}"
done
run_lint "ast-grep suppressions" "${python_command[@]}" scripts/check_ast_grep_suppressions.py
run_lint "ast-grep required matches" env PATH="${ast_grep_directory}:${PATH}" "${python_command[@]}" \
  scripts/check_ast_grep_required_matches.py "${ast_grep_scan_paths[@]}"
# shellcheck disable=SC2046
run_lint "file size" "${python_command[@]}" "${lint_config_directory}/file-size/check_file_sizes.py" \
  --exceptions lint-exceptions/file-size.txt $(compare_arguments_for lint-exceptions/file-size.txt)
# shellcheck disable=SC2046
run_lint "module files" "${python_command[@]}" "${lint_config_directory}/rust/check_module_files.py" \
  --exceptions lint-exceptions/module-files.txt $(compare_arguments_for lint-exceptions/module-files.txt)
run_lint "workspace lint table" "${python_command[@]}" "${lint_config_directory}/rust/check_lint_table.py"

if (( ${#failed_lints[@]} > 0 )); then
  echo "${#failed_lints[@]} structure lints failed: ${failed_lints[*]}" >&2
  exit 1
fi

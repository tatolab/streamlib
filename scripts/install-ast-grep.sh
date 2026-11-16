#!/usr/bin/env bash
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
#
# Installs the ast-grep release the structure lints are pinned to under
# target/tools/, from its prebuilt archive checked against a recorded SHA-256, and
# prints the binary's path. A second run reuses the installed binary. The version
# matches the one tatolab/lint-config's CI tests its rules with.

set -euo pipefail
cd "$(dirname "$0")/.."

AST_GREP_VERSION=0.51.2

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) archive=app-x86_64-unknown-linux-gnu.zip expected_sha256=40604c21c38d4c7581c56bdddeef277c56bf3c0113620aa96e78ae87e82366df ;;
  Linux-aarch64) archive=app-aarch64-unknown-linux-gnu.zip expected_sha256=c3ab2bd43959db28621dc1551c13ea5fe5db4c1fc7b18086dade6b65042c78d0 ;;
  Darwin-arm64) archive=app-aarch64-apple-darwin.zip expected_sha256=ea4f4082d25e5f8e0450add6fd488dd008a373c90483cc2031759c8431f1dcfb ;;
  Darwin-x86_64) archive=app-x86_64-apple-darwin.zip expected_sha256=ef347c331d97cb6a043809091bd96bdd64de6a8354f01da66764496885230591 ;;
  *) echo "install-ast-grep.sh: no pinned ast-grep archive for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

install_directory="target/tools/ast-grep-${AST_GREP_VERSION}"
if [[ ! -x "${install_directory}/ast-grep" ]]; then
  mkdir -p target/tools
  download_directory="$(mktemp -d target/tools/ast-grep-download.XXXXXX)"
  trap 'rm -rf "${download_directory}"' EXIT
  curl -sSfL --retry 3 -o "${download_directory}/${archive}" \
    "https://github.com/ast-grep/ast-grep/releases/download/${AST_GREP_VERSION}/${archive}"
  actual_sha256="$( (sha256sum "${download_directory}/${archive}" 2>/dev/null || shasum -a 256 "${download_directory}/${archive}") | cut -d' ' -f1)"
  if [[ "${actual_sha256}" != "${expected_sha256}" ]]; then
    echo "install-ast-grep.sh: ${archive} has SHA-256 ${actual_sha256}, expected ${expected_sha256}" >&2
    exit 1
  fi
  unzip -q "${download_directory}/${archive}" ast-grep -d "${download_directory}/unpacked"
  mkdir -p "${install_directory}"
  mv "${download_directory}/unpacked/ast-grep" "${install_directory}/ast-grep"
fi
echo "${install_directory}/ast-grep"

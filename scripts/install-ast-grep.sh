#!/usr/bin/env bash
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
#
# Installs the ast-grep release the structure lints are pinned to under
# target/tools/, from its prebuilt archive checked against a recorded SHA-256, and
# prints the binary's path. A second run reuses the installed binary.

set -euo pipefail

AST_GREP_VERSION=0.51.1

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) archive=app-x86_64-unknown-linux-gnu.zip sha256=42514f77ae0c8a25d7d32154d31d3e7e16f3ab894f8ce76ce417de234ee49157 ;;
  Linux-aarch64) archive=app-aarch64-unknown-linux-gnu.zip sha256=db02fae2d8eace5fc40c55242068882452dd03eab38a1d950d5fd981cf112926 ;;
  Darwin-arm64) archive=app-aarch64-apple-darwin.zip sha256=a95772097b4d02bd9d45ed63b300c37041ae3c36632b3bf19250cc7c10187dc4 ;;
  *) echo "install-ast-grep.sh: no pinned ast-grep archive for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

install_directory="target/tools/ast-grep-${AST_GREP_VERSION}"
if [[ ! -x "${install_directory}/ast-grep" ]]; then
  download_directory="$(mktemp -d)"
  trap 'rm -rf "${download_directory}"' EXIT
  curl -sSfL --retry 3 -o "${download_directory}/${archive}" \
    "https://github.com/ast-grep/ast-grep/releases/download/${AST_GREP_VERSION}/${archive}"
  if command -v sha256sum >/dev/null; then
    echo "${sha256}  ${download_directory}/${archive}" | sha256sum -c --quiet -
  else
    echo "${sha256}  ${download_directory}/${archive}" | shasum -a 256 -c --quiet -
  fi
  mkdir -p "${install_directory}"
  unzip -q -o "${download_directory}/${archive}" ast-grep -d "${install_directory}"
fi
echo "${install_directory}/ast-grep"

#!/usr/bin/env bash
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
#
# Exits non-zero unless the title given is a conventional-commit title, the form
# a PR title takes for its readers; run this on a title before opening the PR.
#
# Usage: scripts/check-conventional-pr-title.sh "<title>"

set -euo pipefail

title="${1:?usage: $0 \"<pull request title>\"}"
if printf '%s' "$title" | grep -qE '^(feat|fix|perf|refactor|docs|chore|test|ci|build|revert)(\([a-z0-9._-]+\))?!?: .+'; then
  exit 0
fi
echo "not a conventional-commit title: $title" >&2
echo "write it as type(scope): summary, type one of feat fix perf refactor docs chore test ci build revert" >&2
exit 1

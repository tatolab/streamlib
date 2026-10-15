#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Exits non-zero unless the title given is a conventional-commit title. The repo
# squash-merges, so a PR's title becomes the commit release-please reads to
# compute the version bump; run this on a title before opening the PR.
#
# Usage: scripts/check-conventional-pr-title.sh "<title>"

set -euo pipefail

title="${1:?usage: $0 \"<pull request title>\"}"
if printf '%s' "$title" | grep -qE '^(feat|fix|perf|refactor|docs|chore|test|ci|build|revert)(\([a-z0-9._-]+\))?!?: .+'; then
  exit 0
fi
echo "not a conventional-commit title: $title" >&2
echo "feat -> minor, fix -> patch, ! or a 'BREAKING CHANGE:' footer -> breaking (pre-1.0: minor);" >&2
echo "refactor, chore, docs, test, ci, perf, build and revert land no version bump." >&2
exit 1

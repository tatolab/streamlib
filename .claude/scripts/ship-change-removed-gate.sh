#!/usr/bin/env bash
# ship-change gate: every `- REMOVED: <pattern>` bullet in a change file must no longer
# match anywhere in the tree (the change files themselves, their archive, and vendor/
# are excluded). Run from the repo root.
#
# Usage: ship-change-removed-gate.sh docs/plan/changes/<name>.md
# Exit 0 = clean; exit 1 = something REMOVED still exists (locations printed).
set -euo pipefail

change="${1:?usage: ship-change-removed-gate.sh <change-file>}"
[ -f "$change" ] || { echo "no such change file: $change" >&2; exit 2; }
git rev-parse --git-dir >/dev/null 2>&1 || { echo "must run inside the repo — a gate that cannot search must not pass" >&2; exit 2; }

fail=0
hits="$(mktemp)"
trap 'rm -f "$hits"' EXIT

count=0
while IFS= read -r pat; do
  [ -n "$pat" ] || continue
  count=$((count + 1))
  # git grep: tracked files only (build scratch and caches can't fake a pass), no
  # dependence on a ripgrep binary. Judge by captured matches, not the exit code.
  git grep -InF -- "$pat" -- ':!vendor/**' ':!docs/plan/changes/**' >"$hits" 2>/dev/null || true
  if [ -s "$hits" ]; then
    echo "STILL PRESENT: $pat"
    head -20 "$hits" | sed 's/^/  /'
    fail=1
  fi
done < <(sed -n 's/^[-*][[:space:]]*REMOVED:[[:space:]]*//p' "$change")

if [ "$count" -eq 0 ]; then
  echo "note: $change declares no '- REMOVED:' bullets — nothing to verify."
fi

exit "$fail"

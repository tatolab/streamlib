#!/usr/bin/env bash
# gc-merged-worktrees — remove any worktree under .claude/worktrees/ whose branch
# has a MERGED pull request, reclaiming the orphaned build target (each is a full
# multi-GB build tree). Keyed on a merged PR, not on git ancestry, because the repo
# squash-merges (the branch's commits never become ancestors of main). A worktree
# with no PR, or an open PR, is left untouched — so in-flight / unpushed work is safe.
#
# Runs from the PostToolUse hook after `gh pr merge`, and is safe to run standalone
# or from a periodic sweep (it only removes merged-PR worktrees; it is idempotent).
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0
WT_DIR="$ROOT/.claude/worktrees"
[ -d "$WT_DIR" ] || exit 0
command -v gh >/dev/null 2>&1 || exit 0

git -C "$ROOT" worktree prune 2>/dev/null || true

# path<TAB>branch for every registered worktree living under .claude/worktrees/.
git -C "$ROOT" worktree list --porcelain 2>/dev/null | awk -v pfx="$WT_DIR/" '
  /^worktree /{ wt=substr($0, 10) }
  /^branch /{ if (index(wt, pfx)==1) print wt "\t" substr($0, 8) }
' | while IFS=$'\t' read -r wt ref; do
  branch="${ref#refs/heads/}"
  [ -n "$branch" ] || continue
  merged="$(gh pr list --repo "$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null)" \
            --head "$branch" --state merged --json number --jq 'length' 2>/dev/null || echo 0)"
  if [ "${merged:-0}" -gt 0 ]; then
    if git -C "$ROOT" worktree remove --force "$wt" 2>/dev/null; then
      echo "gc-merged-worktrees: removed $wt (branch '$branch' — PR merged)" >&2
    fi
  fi
done

git -C "$ROOT" worktree prune 2>/dev/null || true

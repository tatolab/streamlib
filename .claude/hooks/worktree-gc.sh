#!/usr/bin/env bash
# worktree-gc (PostToolUse / Bash): after a `gh pr merge`, remove the merged PR's
# worktree so its multi-GB build target doesn't leak. Delegates to
# .claude/scripts/gc-merged-worktrees.sh (which only removes merged-PR worktrees,
# so this is safe to fire after any command — it no-ops unless a merge just landed).
# PostToolUse hooks cannot block; this only ever cleans up.
input="$(cat)"
cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // ""' 2>/dev/null)"

printf '%s' "$cmd" | grep -Eq 'gh[[:space:]]+pr[[:space:]]+merge' || exit 0

dir="${CLAUDE_PROJECT_DIR:-.}"
"$dir/.claude/scripts/gc-merged-worktrees.sh" || true
exit 0

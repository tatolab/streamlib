#!/usr/bin/env bash
# worktree-gc (PostToolUse / Bash): after a `gh pr merge`, remove the merged PR's worktree so its
# build output doesn't leak. Delegates to
# .claude/scripts/gc-merged-worktrees.sh, which removes only merged-PR worktrees, so this no-ops
# unless a merge just landed. PostToolUse hooks cannot block; this only ever cleans up.
command -v jq >/dev/null 2>&1 || exit 0
input="$(cat)"
command_text="$(printf '%s' "$input" | jq -r '.tool_input.command // ""' 2>/dev/null)"

printf '%s' "$command_text" | grep -Eq 'gh[[:space:]]+pr[[:space:]]+merge' || exit 0

"${CLAUDE_PROJECT_DIR:-.}/.claude/scripts/gc-merged-worktrees.sh" || true
exit 0

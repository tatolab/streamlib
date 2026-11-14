#!/usr/bin/env bash
# worktree-gc (PostToolUse / Bash): after any `gh pr merge` command, successful or not, runs
# .claude/scripts/gc-merged-worktrees.sh, which force-removes every worktree whose branch has a
# merged PR, so their build output doesn't leak. PostToolUse hooks cannot block; this only cleans up.
command -v jq >/dev/null 2>&1 || exit 0
input="$(cat)"
command_text="$(printf '%s' "$input" | jq -r '.tool_input.command // ""' 2>/dev/null)"

printf '%s' "$command_text" | grep -Eq 'gh[[:space:]]+pr[[:space:]]+merge' || exit 0

"${CLAUDE_PROJECT_DIR:-.}/.claude/scripts/gc-merged-worktrees.sh" || true
exit 0

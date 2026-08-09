#!/usr/bin/env bash
# plan-gate (PreToolUse / Edit|Write): the enforcement layer of the plan-first
# operating model (docs/plan/OPERATING-MODEL.md).
#
#   docs/plan/**                 requires .claude/state/plan-session
#                                (set by /align, /propose-change, /ship-change, /pivot)
#   runtime/ sdk/ adapters/
#   xtask/                       requires .claude/state/active-ticket.json
#                                (set by /implement after the owner confirms the plan)
#
# Contract: exit 0 + JSON permissionDecision:"deny" blocks the edit with the reason;
# plain exit 0 defers to the normal permission flow. Owner escape hatch: create the
# marker file by hand.

input="$(cat)"
fp="$(printf '%s' "$input" | jq -r '.tool_input.file_path // ""')"
[ -n "$fp" ] || exit 0

root="${CLAUDE_PROJECT_DIR:-$(pwd)}"
state="$root/.claude/state"

deny() {
  jq -n --arg r "$1" '{hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: $r}}'
  exit 0
}

rel="${fp#"$root"/}"
# A worktree edit gates the same as a main-checkout edit.
case "$rel" in
  .claude/worktrees/*) rel="${rel#.claude/worktrees/*/}" ;;
esac

case "$rel" in
  docs/plan/*)
    [ -f "$state/plan-session" ] && exit 0
    deny 'docs/plan/ is the locked decision source. Plan edits happen only inside /align, /propose-change, /ship-change, or /pivot — those skills create .claude/state/plan-session for the session. Owner escape hatch: touch .claude/state/plan-session yourself.'
    ;;
  runtime/*|sdk/*|adapters/*|xtask/*)
    [ -f "$state/active-ticket.json" ] && exit 0
    deny 'Source edits happen only inside /implement with an owner-confirmed ticket (.claude/state/active-ticket.json is missing). Run /implement — or, as the owner escape hatch, create the marker by hand.'
    ;;
esac

exit 0

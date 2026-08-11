#!/usr/bin/env bash
# plan-gate (PreToolUse / Edit|Write): the enforcement layer of the plan-first
# operating model (docs/plan/OPERATING-MODEL.md).
#
#   runtime/ sdk/ adapters/
#   xtask/                       requires .claude/state/active-ticket.json
#                                (set by /implement after the owner confirms the plan)
#
# Contract: exit 0 + JSON permissionDecision:"ask" routes the edit to the owner with the
# reason; plain exit 0 defers to the normal permission flow. The gate informs and slows
# down — it does not wall off. An edit the owner approves at the prompt proceeds without
# a marker file, so a stale scope can never strand work that has to land.
#
# docs/plan/** is deliberately NOT gated here. A hook sees a path, never an intent, so it
# cannot tell recording a fact from making a decision — it prompted on both, and the
# every-time prompt on the factual majority is what taught sessions to escalate trivia.
# That line is doctrine the session applies (CLAUDE.md §Recording facts vs deciding), not
# a path match: facts land with the work, decisions go through the lifecycle skills.

input="$(cat)"
fp="$(printf '%s' "$input" | jq -r '.tool_input.file_path // ""')"
[ -n "$fp" ] || exit 0

root="${CLAUDE_PROJECT_DIR:-$(pwd)}"
state="$root/.claude/state"

ask() {
  jq -n --arg r "$1" '{hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: "ask", permissionDecisionReason: $r}}'
  exit 0
}

rel="${fp#"$root"/}"
# A worktree edit gates the same as a main-checkout edit.
case "$rel" in
  .claude/worktrees/*) rel="${rel#.claude/worktrees/*/}" ;;
esac

case "$rel" in
  runtime/*|sdk/*|adapters/*|xtask/*)
    [ -f "$state/active-ticket.json" ] && exit 0
    ask 'Source edits belong inside /implement with an owner-confirmed ticket (.claude/state/active-ticket.json is missing). Approve to edit anyway — the change lands without ticket traceability — or decline and run /implement.'
    ;;
esac

exit 0

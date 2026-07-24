---
name: focus-milestone
description: Set the milestone the milestone-loop and status board scope to. Use when the owner says "focus on <milestone>", "switch to <milestone>", "let's work on <milestone>", or asks which milestone is currently focused. Records the choice in the loop state file header; lists candidates from GitHub when the name is ambiguous.
---

# focus-milestone

The milestone-loop and `loop-status` scope everything to one focused milestone. This skill sets it.

## Procedure
1. **Resolve the name.** Match the owner's free-form milestone text against the repo's milestones via `gh api` (`gh api repos/:owner/:repo/milestones --paginate`, open milestones). On an exact or unambiguous match, take it. On ambiguity or no match, **list the candidate milestones** (title + open-issue count) and ask which one — don't guess the focus.
2. **Record it.** Write the resolved milestone into the `focused_milestone` field of `.claude/loops/state/milestone-loop.json` so the next reconciler pass and the status board pick it up. This is durable loop state — the focus persists across firings until changed. If the state file does not exist yet, create it from `.claude/loops/state.example.json` first.
3. **Confirm.** Report the now-focused milestone and its open-issue count. Milestone shape and scope are the owner's call — this skill only records which one is in focus, it never edits milestone membership.

To just check the current focus, read `focused_milestone` from the state file and report it — no write. A `null` value means no milestone is focused yet and the loop has nothing to scope to.

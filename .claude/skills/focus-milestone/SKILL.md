---
name: focus-milestone
description: Set the milestone the milestone-loop and status board scope to. Use when Jonathan says "focus on <milestone>", "switch to <milestone>", "let's work on <milestone>", or asks which milestone is currently focused. Records the choice in the loop state file header; lists candidates from GitHub when the name is ambiguous.
---

# focus-milestone

The milestone-loop and `loop-status` scope everything to one focused milestone. This skill sets it.

## Procedure
1. **Resolve the name.** Match Jonathan's free-form milestone text against the repo's milestones via `gh api` (`gh api repos/:owner/:repo/milestones --paginate`, open milestones). On an exact or unambiguous match, take it. On ambiguity or no match, **list the candidate milestones** (title + open-issue count) and ask him which one — don't guess the focus.
2. **Record it.** Write the resolved milestone into the header of `loops/milestone-loop-state.md` so the next reconciler pass and the status board pick it up. This is durable loop state — the focus persists across firings until changed.
3. **Confirm.** Report the now-focused milestone and its open-issue count. Milestone shape and scope are Jonathan's call — this skill only records which one is in focus, it never edits milestone membership.

To just check the current focus, read the state-file header and report it — no write.

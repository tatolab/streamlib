---
name: loop-status
description: Render the milestone-loop's current picture into one status board — what it's acting on, what's waiting on Jonathan (oldest first), what it's watching, recent outcomes, and spend. Use when asked "what's the loop doing", "loop status", "what's waiting on me", or before deciding whether to nudge or pause the loop. Read-only — it reports, it never acts.
---

# loop-status — the board

Read-only. Merge three sources into one board and print it; change nothing.

## Sources
- `loops/milestone-loop-state.md` — the loop's four sections and the focused milestone (state header).
- `loops/run-log.md` — the JSON-lines turn history (trends, recent outcomes, token spend).
- `loops/budget.md` — the daily caps, to compute how much budget is left.
- Live `gh` — reconcile the state file's ticket refs against GitHub so the board reflects reality, not a stale cache (a merged PR or closed issue that the state file still lists is a ghost; show it as resolved, don't perpetuate it).

## Board layout
Print these sections, in order:

1. **Focused milestone** — its name, and a one-line progress read (open / total issues in it).
2. **Acting on** — each in-flight ticket with its claim one-liner, current attempt count (of the cap), and stage (recon / implementing / verifying / draft-PR-open).
3. **Waiting on Jonathan** — parked tickets, **oldest question first**, each with the question's age. **Re-surface anything older than 24h** prominently — a question aging past a day is the thing most worth Jonathan's attention.
4. **Watch** — tickets tracked but not yet acted on (blocked, waiting on a dependency to merge, or next-up once capacity frees), with the reason.
5. **Recent outcomes** — the last few run-log turns condensed: what progressed, what got parked or escalated.
6. **Spend** — today's turns and rough tokens against the daily cap, and whether the loop is in propose_only (≥80%) or capped.

## Rules
- Sort "Waiting on Jonathan" strictly oldest-first — the board's job is to make a stale question impossible to miss.
- Never take an action from this skill — no claiming, no launching, no posting. If the board reveals something worth doing, say so as a recommendation and let the operator (or the next `milestone-loop` turn) act.
- If a state entry is a ghost (its issue/PR closed), mark it resolved in the board and note it so the next reconciler pass prunes it — but don't edit the state file here.

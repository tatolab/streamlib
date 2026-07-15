---
paused: false
---

# milestone-loop state

Durable state for the milestone-loop between firings. Setting `paused: true` in the frontmatter
above is one of the three kill switches (see `LOOP.md`). The reconciler rewrites the sections below
each pass; they reflect the loop's current picture of the focused milestone.

## Acting on
Tickets the loop is actively working this pass — in-flight attempts, open PRs, worktrees alive.
Each entry: ticket ref, current attempt count, and what stage it's at.

_(none yet)_

## Waiting on Jonathan
Tickets parked on a question or a decision only the repo owner can make (merge, milestone scope,
an answered question). Cleared only by an owner comment postdating the question.

_(none yet)_

## Watch
Tickets the loop is tracking but not acting on yet — blocked by another ticket, waiting on a
dependency to merge, or next-up once capacity frees.

_(none yet)_

## Ignored this pass (noise)
Items the reconciler looked at and deliberately skipped this pass, with a one-line why — so the
next pass doesn't re-litigate them.

_(none yet)_

---
Turn-by-turn history lives in [`run-log.md`](run-log.md).

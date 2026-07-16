---
paused: false
owner_login: tato123
---

# milestone-loop state

Durable state for the milestone-loop between firings. Setting `paused: true` in the frontmatter
above is one of the three kill switches (see `LOOP.md`). `owner_login` is the repository owner's
GitHub login — operator-set here, never derived from `gh repo view` (which returns the org); it is
the identity the parked-question detection in the milestone-loop skill (step 2) keys off. The
reconciler rewrites the sections below each pass; they reflect the loop's current picture of the
focused milestone, plus the caches (comment-id ledger, delta-probe fields) the next pass reads.

## Acting on
Tickets the loop is actively working this pass — in-flight attempts, open PRs, worktrees alive.
Each entry: ticket ref, current attempt count, and what stage it's at.

_(none yet)_

## Waiting on the owner
Tickets parked on a question or a decision only the repo owner can make (merge, milestone scope,
an answered question). Cleared by any later comment on the ticket whose id is NOT in the loop's
recorded comment-id ledger (a comment the loop did not post itself) — not by `author.login`, since
the loop and the owner share the same login. Each entry carries the ledger of the loop's own
comment ids on that ticket.

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

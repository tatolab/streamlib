---
paused: false
owner_login: tato123
capabilities:
  live_verify: available   # available | unavailable | unknown — set by the preflight probe; owner may hand-override
  camera: true
  gpu: true
  display: true
  probed_utcdate: 2026-07-21
---

# milestone-loop state

Durable state for the milestone-loop between firings. Setting `paused: true` in the frontmatter
above is one of the three kill switches (see `LOOP.md`). `owner_login` is the repository owner's
GitHub login — operator-set here, never derived from `gh repo view` (which returns the org); it is
the identity the parked-question detection in the milestone-loop skill (step 2) keys off. The
reconciler rewrites the sections below each pass; they reflect the loop's current picture of the
focused milestone, plus the caches (comment-id ledger, delta-probe fields) the next pass reads.

`capabilities:` is the rig picture the loop's verify step reads. It is BOTH auto-probed AND a
hand-settable owner override — the "checklist". The milestone-loop preflight (skill step 1) sets
`live_verify`, `camera`, `gpu`, and `display` from a read-only device probe plus a one-time bypass
smoke, and stamps `probed_utcdate`; a hand-set value wins over the probe (the owner may force-enable
or force-disable a capability per milestone, and the loop must not clobber a hand-set value on a
same-day pass). `live_verify: available` means the loop runs `/verify-live` itself via the Bash
`dangerouslyDisableSandbox` bypass for every rig-touching ticket; `unavailable` means it parks the
live check for the human instead. `unknown` forces a re-probe next pass.

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

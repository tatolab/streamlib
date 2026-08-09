---
name: derive-tickets
description: Turn an approved change proposal into the minimum set of tracer-bullet tickets on GitHub — as few as the change honestly needs.
disable-model-invocation: true
---

# Derive tickets

Input: an approved `docs/plan/changes/<name>.md` with zero unresolved `[NEEDS DECISION]`
markers. Anything else stops this skill.

**Readiness gate — tickets are the LAST step of understanding, not a substitute for
it.** Before drafting anything, check: the plan sections behind this change are DECIDED,
the change contains no unknowns you would have to guess through, and both Claude and the
owner could describe the target architecture identically. If the shape is still fuzzy,
the unknowns many, or the scope smells like more than one milestone — stop and route to
`/explore-idea` (burn down the unknowns) or back to `/align` (decide the architecture).
Tickets derived from a half-understood architecture are the exact ticket-mess this
operating model exists to prevent. Right-size for iteration: every change should advance
the MVP with showable work.

1. Draft **as few tickets as the change honestly needs** — ticket count is guidance, not
   a cap (owner decision 2026-08-02, `OPERATING-MODEL.md:271`). Each is a tracer bullet:
   a narrow but COMPLETE vertical slice (schema → engine → SDK → test), demoable on its
   own, sized to one fresh context window. Never a horizontal slice of one layer. A long
   list is a signal to re-read the change for a seam it should have been split at — check
   before you publish, but a change that genuinely needs eight tickets gets eight.
2. Wide mechanical refactors are sequenced **expand–contract**: an expand ticket (new
   form beside old), migration batches (each its own ticket, blocked by expand, CI green
   throughout), a contract ticket (delete the old form, blocked by every batch). The
   contract ticket carries the change's `REMOVED` bullets.
3. **Inline the constraints** each ticket must respect — copied from the change and the
   plan into the ticket body. Agents don't follow pointer chains reliably; the ticket
   must stand alone. Link the change file by path for context only.
4. Declare blocking edges. Milestone = the standing MVP umbrella (`MVP`, M39) — never
   create a per-change milestone.
5. **Quiz the owner**: a numbered list — title / blocked-by / what it delivers. Iterate
   until they approve the LIST. Hard stop; nothing publishes before the yes.
6. Publish blockers-first on the `.github/ISSUE_TEMPLATE/` forms via `gh issue create`,
   wire the blocking edges, report the URLs.

---
name: derive-tickets
description: Turn an approved change proposal into the minimum set of tracer-bullet tickets on GitHub — as few as the change honestly needs.
---

# Derive tickets

How the owner likes a change decomposed into tickets. The input is normally an approved
`docs/plan/changes/<name>.md` with its `[NEEDS DECISION]` markers settled.

**Tickets are the last step of understanding, not a substitute for it.** Before drafting,
it is worth checking that the plan sections behind the change are DECIDED, that there are
no unknowns you'd have to guess through, and that you and the owner would describe the
target architecture the same way. If the shape is still fuzzy, the unknowns many, or the
scope smells like more than one milestone, `/explore-idea` (burn down unknowns) or
`/align` (decide the architecture) usually pays off first — tickets derived from a
half-understood architecture are the ticket mess this way of working exists to avoid.
Right-size for iteration: every change should advance the MVP with showable work.

1. **Draft as few tickets as the change honestly needs** — count is guidance, not a cap
   (owner decision 2026-08-02, `OPERATING-MODEL.md` §Numeric caps). Each is a tracer
   bullet: a narrow but complete vertical slice (schema → engine → SDK → test), demoable
   on its own, sized to one fresh context window. Horizontal one-layer slices tend to
   strand work. A long list is a hint to re-read the change for a seam it should have
   been split at; a change that genuinely needs eight tickets gets eight.
2. **Sequence wide mechanical refactors expand–contract**: an expand ticket (new form
   beside old), migration batches (each its own ticket, blocked by expand, CI green
   throughout), a contract ticket (delete the old form, blocked by every batch). The
   contract ticket carries the change's `REMOVED` bullets.
3. **Inline the constraints** each ticket must respect, copied from the change and the
   plan into the ticket body. Agents don't follow pointer chains reliably, so a ticket
   that stands alone gets built correctly more often. Link the change file by path for
   context.
4. **Declare blocking edges.** Milestone = the epic naming the capability this change
   delivers: reuse that capability's milestone when one exists, create it when none
   does. A milestone names a product capability, not the change file or an activity.
5. **Show the owner the list** — title / blocked-by / what it delivers. When they're
   around, iterate with them on it; the owner likes to see the decomposition before it
   lands. When they're away, use your judgment: publishing is easy to adjust (issues can
   be edited or closed), so it's fine to publish and tell them what you filed.
6. **Publish blockers-first** on the `.github/ISSUE_TEMPLATE/` forms via `gh issue create`,
   wire the blocking edges, and report the URLs.

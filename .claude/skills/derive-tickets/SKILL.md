---
name: derive-tickets
description: Turn an approved change proposal into the minimum set of tracer-bullet tickets on GitHub — never more than five.
disable-model-invocation: true
---

# Derive tickets

Input: an approved `docs/plan/changes/<name>.md` with zero unresolved `[NEEDS DECISION]`
markers. Anything else stops this skill.

1. Draft **≤5 tickets**, each a tracer bullet: a narrow but COMPLETE vertical slice
   (schema → engine → SDK → test), demoable on its own, sized to one fresh context
   window. Never a horizontal slice of one layer. Needing more than 5 means the change
   splits — go back to `/propose-change`.
2. Wide mechanical refactors are sequenced **expand–contract**: an expand ticket (new
   form beside old), migration batches (each its own ticket, blocked by expand, CI green
   throughout), a contract ticket (delete the old form, blocked by every batch). The
   contract ticket carries the change's `REMOVED` bullets.
3. **Inline the constraints** each ticket must respect — copied from the change and the
   plan into the ticket body. Agents don't follow pointer chains reliably; the ticket
   must stand alone. Link the change file by path for context only.
4. Declare blocking edges. Milestone = the change name (create it if missing).
5. **Quiz the owner**: a numbered list — title / blocked-by / what it delivers. Iterate
   until they approve the LIST. Hard stop; nothing publishes before the yes.
6. Publish blockers-first on the `.github/ISSUE_TEMPLATE/` forms via `gh issue create`,
   wire the blocking edges, report the URLs.

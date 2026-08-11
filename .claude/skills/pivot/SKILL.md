---
name: pivot
description: The owner declares a direction change — the plan moves first, then the legacy is inventoried and ripped out.
disable-model-invocation: true
---

# Pivot

Only the owner invokes a pivot. Never propose one mid-implementation.

1. Run `grilling` until the new direction is stated in five sentences or fewer and the
   owner confirms them verbatim.
2. **Plan first**: rewrite the affected ARCHITECTURE.md sections — new DECIDED entries,
   invalidated sections back to OPEN, diagrams updated. Rationale goes to an ADR.
3. **Inventory the legacy** (read-only sweep): the code paths, docs, rules, skills,
   tickets, and milestones the old direction leaves behind. Present the inventory —
   under this model legacy is deleted, never kept running in parallel with the new
   shape.
4. Write the rip-out change via `/propose-change` — its `REMOVED` bullets are the
   inventory. Derive its tickets via `/derive-tickets`.
5. Run `/reconcile-tracker` for the tickets and milestones the pivot orphaned.

Completion = plan PR open + rip-out change proposed + tracker batch presented.

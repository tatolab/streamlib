---
name: pivot
description: A direction change — the plan moves first, then the legacy is inventoried and ripped out.
---

# Pivot

For when the owner has changed direction. Pivots are the owner's to declare — they
retire agreed architecture — so if implementation surfaces a reason to pivot, raise it
with the owner rather than starting one unprompted.

The point of the order below is that agents otherwise keep the old shape alive beside
the new one; moving the plan first and inventorying the legacy makes deletion the
expected outcome.

1. **Pin the new direction** with `grilling` until it can be said in five sentences or
   fewer and the owner agrees with them as written.
2. **Plan first**: rewrite the affected ARCHITECTURE.md sections — new DECIDED entries,
   invalidated sections back to OPEN, diagrams updated. Superseded entries are retired
   by deletion, with a `Rejected:` line only where `.claude/rules/docs-policy.md` allows
   one. Rationale goes to an ADR.
3. **Inventory the legacy** (read-only sweep): the code paths, docs, rules, skills,
   tickets and milestones the old direction leaves behind. Present it — the goal is
   deletion, not keeping the old shape running in parallel.
4. **Write the rip-out change** via `/propose-change`, with the inventory as its
   `REMOVED` bullets, and derive its tickets via `/derive-tickets`.
5. **Clean the tracker** with `/reconcile-tracker` for the tickets and milestones the
   pivot orphaned.

A pivot is usually done when the plan PR is open, the rip-out change is proposed, and the
tracker batch has been presented.

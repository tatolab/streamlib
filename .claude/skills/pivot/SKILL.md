---
name: pivot
description: A direction change — the plan moves first, then the legacy is inventoried and ripped out.
---

# Pivot

A pivot retires agreed architecture, so it is the owner's to declare. If implementation
surfaces a reason to pivot, raise it with the owner.

The plan moves first and the legacy is inventoried, so the old shape is deleted and never
kept alive beside the new one.

1. **Pin the new direction** with `grilling` until it fits in five sentences and the
   owner agrees with them as written.
2. **Plan first:** rewrite the affected ARCHITECTURE.md sections. New DECIDED entries,
   invalidated sections back to OPEN, diagrams updated. A superseded entry is deleted,
   with a `Rejected:` line only where `.claude/rules/docs-policy.md` allows one.
   Rationale goes to an ADR.
3. **Inventory the legacy,** read-only: the code paths, docs, rules and skills the old
   direction leaves behind, and the Linear projects and issues it orphans. Present it.
4. **Write the rip-out change** with `/propose-change`, the inventory as its `REMOVED`
   bullets, and turn it into a Linear project with `/plan-project`.
5. **Cancel the orphaned issues and projects** in Linear, each with a one-line comment
   naming the pivot, once the owner has seen the list.

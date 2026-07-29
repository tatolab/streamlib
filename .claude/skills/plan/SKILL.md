---
name: plan
description: Where are we and what's next — reads the plan statuses, open changes, active tickets, and the tracker, then routes to the right skill.
disable-model-invocation: true
---

# Plan (the router)

Read-only. Report, then route. Never edit anything from this skill.

1. Read `docs/plan/ARCHITECTURE.md` — count sections by status (SHIPPED / IN-FLIGHT /
   DECIDED / OPEN). Name the OPEN sections.
2. List `docs/plan/changes/*.md` (active deltas) and any `[NEEDS DECISION]` markers
   still inside them.
3. Check `.claude/state/active-ticket.json` (work mid-flight?) and
   `gh issue list --state open --limit 100 --json number,title,milestone`.
4. Report in at most five lines: plan state, active changes, in-flight ticket, tracker
   shape, and the single recommended next step.
5. Route by the first rule that matches:
   - §Product (the MVP sentence) still OPEN → `/align` on §Product.
   - A change with every ticket merged → `/ship-change`.
   - An approved change without tickets → `/derive-tickets`.
   - A ready ticket exists → `/implement`.
   - Tracker items that trace to no plan entry → `/reconcile-tracker`.
   - The owner declared a direction change → `/pivot`.

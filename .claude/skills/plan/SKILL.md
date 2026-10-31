---
name: plan
description: Where are we and what's next — reads the plan statuses, open changes, open PRs and the tracker, then suggests which skill or step fits next.
---

# Plan (the router)

A quick orientation pass. It exists because the catalog of skills is large and the
plan's state is spread over several files; reading them once and naming a next step
saves every session from re-deriving it. It suggests, it does not decide — pick
whatever next step the situation actually calls for.

A rough pass that has worked well:

1. Read `docs/plan/ARCHITECTURE.md` and count sections by status (SHIPPED / IN-FLIGHT /
   DECIDED / OPEN). Name the OPEN ones — they are where the owner's input is most
   likely needed.
2. List `docs/plan/changes/*.md` (active deltas) and any `[NEEDS DECISION]` markers
   still inside them.
3. Look at what is mid-flight — open PRs (`gh pr list`), plus remote branches
   (`git branch -r`) and local worktrees (`git worktree list`) that have no PR yet — and
   the tracker
   (`gh issue list --state open --limit 100 --json number,title,milestone`).
4. Report briefly: plan state, active changes, in-flight work, tracker shape, and the
   step you'd recommend.

Common next steps, as a starting point rather than a decision table:

- §Product (the MVP sentence) still OPEN → `/align` on §Product is usually the most
  valuable thing to settle.
- A change with every ticket merged → `/ship-change` folds it in.
- An approved change without tickets → `/derive-tickets`.
- A ready ticket → `/implement`.
- Tracker items that trace to no plan entry → `/reconcile-tracker`.
- The owner has described a direction change → `/pivot`.

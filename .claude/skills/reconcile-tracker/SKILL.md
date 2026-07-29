---
name: reconcile-tracker
description: Make GitHub a projection of the plan — audit every milestone and open ticket, fix them in one owner-approved batch.
disable-model-invocation: true
---

# Reconcile tracker

Invariant: a milestone maps to a plan section or an active change; a ticket traces to a
change or is a bug against SHIPPED behavior. Nothing else exists in the tracker.

1. Pull it all: `gh api repos/{owner}/{repo}/milestones --paginate` and
   `gh issue list --state open --limit 200 --json number,title,milestone,labels,body`.
2. Map each milestone and each ticket to the plan. Classify: **KEEP** / **RETITLE** /
   **RE-MILESTONE** / **REWRITE** (body drifted from the plan) / **CLOSE** (traces to
   nothing, or to a superseded direction — cite the plan entry that supersedes it).
3. Present **one batch table**: item / action / one-line reason. The owner approves the
   batch as a list and may strike lines. **Hard stop — nothing executes before the yes.**
4. Execute the approved batch via `gh`: closures get a comment naming the superseding
   plan entry; rewrites preserve the original text under a `<details>` fold; milestones
   are closed, never deleted.
5. Report counts and anything skipped.

Never act item-by-item outside an approved batch.

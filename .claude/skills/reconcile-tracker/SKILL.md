---
name: reconcile-tracker
description: Make GitHub a projection of the plan — audit every milestone and open ticket, and fix them in one reviewed batch.
---

# Reconcile tracker

The owner likes the tracker to be a projection of the plan: a milestone maps to a plan
section or an active change, and a ticket traces to a change or is a bug against SHIPPED
behaviour. That keeps a picker from building against a retired direction.

1. Pull it all: `gh api repos/{owner}/{repo}/milestones --paginate` and
   `gh issue list --state open --limit 200 --json number,title,milestone,labels,body`.
2. Map each milestone and each ticket to the plan. Classify: **KEEP** / **RETITLE** /
   **RE-MILESTONE** / **REWRITE** (body drifted from the plan) / **CLOSE** (traces to
   nothing, or to a superseded direction — cite the plan entry that supersedes it).
3. Present **one batch table**: item / action / one-line reason. One table is far easier
   for the owner to review than a drip of single changes, and they may strike lines.
   Closing other people's tickets is the visible part, so when the owner is around, let
   them see the table before executing; when they're away, executing the clear-cut lines
   (retitles, re-milestones, closures that cite a superseding plan entry) and leaving the
   judgment calls for them is reasonable.
4. Execute via `gh`: closures get a comment naming the superseding
   plan entry; rewrites preserve the original text under a `<details>` fold; milestones
   are closed, never deleted.
5. Report counts, what you executed, and anything left for the owner.

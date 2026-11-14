---
name: plan-project
description: Turn an agreed goal or design into a Linear project with milestones, issues, per-repo sub-issues and blocking relations. Use when a design is settled and the work needs laying out before anyone builds.
---

# Plan a project

The input is the agreed design — a design doc, a change proposal, or the project's own document
in Linear. If the design still has open questions the work would have to guess through, say
which and settle them with the owner first; issues cut from a half-agreed design are the
expensive mistake here.

1. **Project.** Reuse the existing Linear project if the goal belongs to one; otherwise create
   it with a name that states the goal and a one-sentence summary of what is true when it is
   done. Link or attach the design rather than restating it.
2. **Milestones.** Each is a phase with a one-line outcome a person could check in the running
   product, in the order they would be reached. Not an activity ("refactor", "tests") and not a
   layer.
3. **Issues.** Each is a thin vertical slice: complete through every layer it touches,
   demonstrable on its own, small enough for one session. As few as the design honestly needs.
   A slice that crosses repos is a parent with one sub-issue per repo. Titles, prefixes, labels
   and descriptions follow `file-issue`.
4. **A wide mechanical migration** is sequenced expand, then migrate in batches, then contract:
   the new form beside the old, each batch green on its own, and a last issue that deletes the
   old form, blocked by every batch.
5. **Relations.** Wire every real ordering as `blockedBy`. Copy the constraints an issue must
   respect from the design into its Agent notes, so the issue stands alone.
6. **Publish** in blocker-first order, then report the shape: project, each milestone with its
   outcome, and its issues with their IDs and what blocks what. Name any issue you could not
   make small and any part of the design that produced no issue.

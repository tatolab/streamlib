---
name: ship-change
description: Fold a completed change into the plan and prove its removals happened — the guard against half-migrations.
---

# Ship change

Run once a change's tickets have merged (check the milestone). Its purpose is to make
sure a change is really finished — the old shape gone, the plan describing what shipped —
because half-finished migrations are the failure this repo has paid for most. The steps
below are the sequence that has worked; the removal check in step 1 is the part worth
not skipping.

1. **Check the removals:** `bash .claude/scripts/ship-change-removed-gate.sh
   docs/plan/changes/<name>.md`. Exit 0 means every `REMOVED` bullet is gone from the
   tree. `STILL PRESENT` lists what remains, referenced or on disk; that residue is
   unfinished work — finish it (or file it in the same milestone) before archiving, so
   the archive doesn't claim a removal that didn't happen. `MALFORMED BULLET` is a
   wording problem in the change file, not residue — fix the bullet in place (grammar:
   `docs/plan/changes/README.md`).
2. **Fold** each `ADDED`/`MODIFIED` section into `docs/plan/ARCHITECTURE.md`; flip the
   affected sections `IN-FLIGHT` → `SHIPPED`; delete the plan text the `REMOVED`
   sections retire. Per `.claude/rules/docs-policy.md`, fold the `amended by` /
   `reopened by` clauses this change builds and drop their pointers and completed
   history, re-test each `Rejected:` line it touches, and delete rather than strike.
   Delete a head-of-plan reading rule, or the clause of one, this change discharges.
3. **Diagrams:** update `docs/plan/diagrams/*.mmd` to match. (The Excalidraw view, when
   wanted, is regenerated from the `.mmd` via mermaid-to-excalidraw or the Excalidraw
   app's Mermaid import — the `.mmd` is the source.)
4. **Archive:** `git mv docs/plan/changes/<name>.md
   docs/plan/changes/archive/<YYYY-MM-DD>-<name>.md` — the date is the last ticket's
   merge date from `gh pr view`.
5. **Open the PR** on a branch; plan changes go through the owner's GitHub review.
6. If the change's milestone has no open issues, close it
   (`gh api -X PATCH repos/tatolab/streamlib/milestones/<n> -f state=closed`) and say so.

Pasting the removal check's clean run into the PR body lets the owner see the removals
were proven without re-running it.

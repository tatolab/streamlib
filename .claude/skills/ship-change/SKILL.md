---
name: ship-change
description: Fold a completed change into the plan and prove its removals happened — the guard against half-migrations.
---

# Ship change

Run once every issue of the change's Linear project is Done. It makes sure the change is
really finished: the old shape gone, the plan describing what shipped.

1. **Check the removals:** `bash .claude/scripts/ship-change-removed-gate.sh
   docs/plan/changes/<name>.md`. Exit 0 means every `REMOVED` bullet is gone from the
   tree. `STILL PRESENT` lists residue, which is unfinished work: finish it, as a Linear
   issue in the change's project when it is more than a quick fix, and re-run the check
   before archiving. `MALFORMED BULLET` is a wording problem in the change file: fix the
   bullet in place (grammar: `docs/plan/changes/README.md`).
2. **Fold** each `ADDED` / `MODIFIED` section into `docs/plan/ARCHITECTURE.md`, flip the
   affected sections `IN-FLIGHT` → `SHIPPED`, and delete the plan text the `REMOVED`
   sections retire. Per `.claude/rules/docs-policy.md`, fold the `amended by` /
   `reopened by` clauses this change builds and drop their pointers and completed
   history, re-test each `Rejected:` line it touches, and delete rather than strike.
   Delete a head-of-plan reading rule, or the clause of one, this change discharges.
3. **Diagrams:** update `docs/plan/diagrams/*.mmd` to match. The `.mmd` is the source;
   an Excalidraw view is regenerated from it.
4. **Archive:** `git mv docs/plan/changes/<name>.md
   docs/plan/changes/archive/<YYYY-MM-DD>-<name>.md`, dated by the last PR's merge
   (`gh pr view`).
5. **Open the PR** with the removal check's clean run pasted in its body, so the owner
   sees the removals were proven.

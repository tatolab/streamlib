---
name: ship-change
description: Fold a completed change into the plan and prove the removals — the anti-half-migration gate.
disable-model-invocation: true
---

# Ship change

Precondition: every ticket of the change is merged (check the milestone). Exact
sequence — no improvisation:

1. `bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/<name>.md` —
   **exit 0 required.** Nonzero output lists what still exists in the tree; that residue
   is finished first (a follow-up ticket in the same milestone), and this skill re-runs
   after. Never archive with residue.
2. `mkdir -p .claude/state && touch .claude/state/plan-session`
3. Fold each `ADDED`/`MODIFIED` section into `docs/plan/ARCHITECTURE.md`; flip the
   affected sections `IN-FLIGHT` → `SHIPPED`, each with a `<!-- verify: <glob or
   command> -->` marker; delete the plan text the `REMOVED` sections retire.
4. Update `docs/plan/diagrams/*.mmd` to match. (The Excalidraw view, when wanted, is
   regenerated from the `.mmd` via mermaid-to-excalidraw or the Excalidraw app's Mermaid
   import — the `.mmd` is the source and is never edited from the Excalidraw side.)
5. `git mv docs/plan/changes/<name>.md docs/plan/changes/archive/<YYYY-MM-DD>-<name>.md`
   — the date is the last ticket's merge date from `gh pr view`, not today's guess.
6. `rm -f .claude/state/plan-session`; branch, open the PR (plan changes merge only with
   the owner's review).

Completion = the archive PR is open and the gate script's clean run is pasted into its
body.

---
name: ship-change
description: Fold a completed change into the plan and prove the removals — the anti-half-migration gate.
disable-model-invocation: true
---

# Ship change

Precondition: every ticket of the change is merged (check the milestone). Exact
sequence — no improvisation:

1. `bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/<name>.md` —
   **exit 0 required.** `STILL PRESENT` lists what remains in the tree, referenced or on
   disk; that residue is finished first (a follow-up ticket in the same milestone), and
   this skill re-runs after. Never archive with residue. `MALFORMED BULLET` is not
   residue and never becomes a ticket — the bullet cannot prove anything as written, so
   the change file is corrected in place (grammar: `docs/plan/changes/README.md`).
2. Fold each `ADDED`/`MODIFIED` section into `docs/plan/ARCHITECTURE.md`; flip the
   affected sections `IN-FLIGHT` → `SHIPPED`, each with a `<!-- verify: <glob or
   command> -->` marker; delete the plan text the `REMOVED` sections retire.
3. Update `docs/plan/diagrams/*.mmd` to match. (The Excalidraw view, when wanted, is
   regenerated from the `.mmd` via mermaid-to-excalidraw or the Excalidraw app's Mermaid
   import — the `.mmd` is the source and is never edited from the Excalidraw side.)
4. `git mv docs/plan/changes/<name>.md docs/plan/changes/archive/<YYYY-MM-DD>-<name>.md`
   — the date is the last ticket's merge date from `gh pr view`, not today's guess.
5. Branch, open the PR (plan changes merge only with the owner's review).

Completion = the archive PR is open and the gate script's clean run is pasted into its
body.

---
name: align
description: The working session that turns OPEN plan sections into DECIDED ones, with the owner, one section at a time.
disable-model-invocation: true
---

# Align

The only skill that flips a plan section OPEN → DECIDED.

1. Pick ONE section of `docs/plan/ARCHITECTURE.md` — the owner's choice. While §Product
   is OPEN, recommend it first: every ticket traces to that sentence or does not exist.
2. Run `batch-grilling` over the section's open decisions, with the `glossary` skill
   active for term drift.
3. As each decision lands, edit immediately (don't batch):
   - The section entry: DECIDED entries state WHAT, never why. Rationale goes to a
     `docs/decisions/` ADR that the entry links as `[ADR-name]`.
   - `docs/plan/diagrams/*.mmd` in the same breath, when the decision changes structure.
   - Nothing prompts on a plan edit. The gate is this skill's own contract — the owner's
     spoken yes, step 4 — not a permission dialog.
4. Done when the owner says the section is decided: restate the whole section back and
   get the explicit yes.
5. Commit on a branch, open the PR — plan changes merge only with the owner's review
   (CODEOWNERS).

Hard rule: no code, no tickets, no `changes/` files from this skill. Deciding and
building never share a session.

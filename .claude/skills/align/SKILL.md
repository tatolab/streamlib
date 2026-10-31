---
name: align
description: The working session that turns OPEN plan sections into DECIDED ones, with the owner, one section at a time.
---

# Align

How the owner likes to settle an OPEN plan section. The plan is where architecture is
agreed, so moving a section OPEN → DECIDED is the moment the owner's intent gets written
down; this skill is the shape that has worked for doing that well.

1. **Pick a section** of `docs/plan/ARCHITECTURE.md` — usually the owner's choice. While
   §Product is OPEN it is worth recommending first, since every ticket ultimately
   traces to that sentence.
2. **Work the open decisions** with `batch-grilling`, keeping the `glossary` skill in
   mind for term drift. Facts get looked up; only the genuine judgment calls go to the
   owner.
3. **Edit as decisions land** rather than batching them to the end, so nothing is lost
   if the session ends early:
   - The section entry: DECIDED entries state WHAT, not why — a `Rejected:` line's one
     clause excepted (`.claude/rules/docs-policy.md`). Rationale goes to a
     `docs/decisions/` ADR that the entry links as `[ADR-name]`.
   - `docs/plan/diagrams/*.mmd` alongside, when the decision changes structure.
4. **Say it back.** Restating the whole section and getting the owner's agreement is
   what makes the DECIDED marker trustworthy to later sessions — they will build on it
   without re-asking.
5. **Open a PR** on a branch. Plan changes go through the owner's review on GitHub
   (CODEOWNERS on `docs/plan/**`).

Deciding and building tend to go better in separate passes — a session that just
settled the architecture is anchored on the conversation, not the tree — so this skill
usually produces plan edits, not code, tickets or `changes/` files. Use judgment when a
tiny follow-on obviously belongs in the same PR.

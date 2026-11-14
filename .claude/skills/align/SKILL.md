---
name: align
description: The working session that turns OPEN plan sections into DECIDED ones, with the owner, one section at a time.
---

# Align

Moving a section of `docs/plan/ARCHITECTURE.md` from OPEN to DECIDED is the moment the
owner's intent gets written down.

1. **Pick a section**, usually the owner's choice. While §Product is OPEN, recommend it
   first: every issue traces to that sentence.
2. **Work the open decisions** with `grilling` in batch mode, and the `glossary` skill
   for term drift. Look facts up; only the judgment calls go to the owner.
3. **Edit as each decision lands**, so nothing is lost if the session ends early:
   - A DECIDED entry states what, not why, a `Rejected:` line's one clause excepted
     (`.claude/rules/docs-policy.md`). Rationale goes to a `docs/decisions/` ADR the
     entry links as `[ADR-name]`.
   - `docs/plan/diagrams/*.mmd`, when the decision changes structure.
4. **Say it back.** Restate the whole section and get the owner's agreement: later
   sessions build on a DECIDED marker without re-asking.
5. **Open a PR.** Plan changes go through the owner's review (CODEOWNERS on
   `docs/plan/**`).

An align produces plan edits. Code, Linear issues and `changes/` files come in a later
pass, from the tree and not from the conversation.

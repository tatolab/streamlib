---
paths:
  - "docs/**"
---

# Docs policy

- **Architecture docs (`docs/architecture/`) describe current shipped state only.** No tracker
  references (issue / PR / milestone numbers), no dates, no roadmap / proposed-work / "will become
  X", no history of superseded designs. Proposed architecture lives in the plan (`docs/plan/`) —
  the single decision source — never in individual issues; issues reference the plan, they do not
  restate or extend it.
- **`docs/learnings/` is empirical only** — surprising driver / library / spec behavior in
  symptom → root cause → fix shape. Tie the lesson to the constraint, not a line number. A new
  learning ships with its index line in `docs/learnings/README.md` in the same PR.
- **`docs/decisions/` holds chosen-shape rationale** — why this design over the alternatives.
- **Supersession is annotated, not overwritten:**

  ```markdown
  > ~~Original claim.~~ — Superseded YYYY-MM-DD by <evidence>. <why it's no longer right>.
  ```

  Outright deletion is allowed when content is provably wrong — leave a one-line marker saying what
  was removed and why.
- **Never create a summary doc of what the code already shows.** If it's derivable from the tree,
  read the tree.
- Edit markdown with Opus; show the evidence that drove the change in the PR / commit body.

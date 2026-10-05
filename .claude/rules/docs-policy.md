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
- **`docs/research/` is dated evidence** — a memo answers one question from primary sources as of
  the day it was taken, and describes the tree it measured. It is never rewritten to match a tree
  that has since changed: a memo naming something a later change deleted is the record working, not
  residue.
- **Docs say what holds now; retired text is deleted, never struck.** When a decision retires a
  claim, delete it and repair the sentence around it so it reads as if written that way — no
  `~~strikethrough~~`, no "Superseded …" note, no in-body narration ("superseding …",
  "(amended 2026-…)"). Delete a struck span whole, never half. A fact in the deleted text that
  still bears on a live OPEN moves into that OPEN as `Known (<date the fact was established>):
  …`. In `docs/plan/changes/`, a note that voids part of the file's own scope or inventory is
  deleted only together with every line it voids. `docs/research/`, `docs/decisions/` and
  `docs/plan/changes/archive/` are records and keep their text as written; an ADR clause a
  later decision retires is struck in place, as `docs/decisions/README.md` states.
- **A retired shape survives as one `Rejected:` line, and only when a session without it would
  plausibly rebuild it** — it is the obvious design, or the owner ruled it out by name — and no
  live sentence in the same section already names it. Sessions never infer a decision from git
  history, so a rejection that matters is written down, inside the entry that replaced it, and
  states no tree state:

  ```markdown
  Rejected: <the shape, as a session would propose it> — <why, one clause> (<owner or decision>, YYYY-MM-DD).
  ```

- **A plan history tag says what built the entry and what will change it:**
  `[<decision>; <change> — SHIPPED #<pr>, #<pr>; amended by <decision or change>: <clause>]`.
  An `amended by` / `reopened by` pointer stays while its clause is unbuilt, and only the
  `/ship-change` that builds it folds it into the body — even when the tree already has it. A
  pointer that only says X is not built, or is deleted, counts as built once the tree lacks X,
  and is folded by whichever session next edits the entry. Completed history is dropped, and so
  is a change that only removed part of the entry. The only tag words are `SHIPPED`, `amended
  by` and `reopened by`: each change that built part of the entry adds its own `<change> —
  SHIPPED #…` segment, and part qualifiers (`for <part>`, `closing #N`) are dropped —
  `narrowed`, `settled`, `superseded` and `retired by` are not tag words. An OPEN carries no
  SHIPPED token. A lesson inside a tag moves to `docs/learnings/` or is deleted.
- **Rejection records that already have a form keep it:** the glossary's `_Avoid_` lists and
  "Retired by" paragraphs, `docs/plan/changes/README.md`'s retired-without-shipping list, and the
  placement-ban records that `.claude/rules/placement.md` and `check-no-in-process-placement`
  pin, including that gate's exempt lines.
- **Never create a summary doc of what the code already shows.** If it's derivable from the tree,
  read the tree.
- Edit markdown with Opus; show the evidence that drove the change in the PR / commit body.

---
name: grilling
description: Interview the owner, one question at a time, to reach shared understanding of a plan, decision, or idea. Use when the owner wants to stress-test thinking, align on a design or direction, settle a whole area in one sitting, or says "grill me", "batch grill me", "let's debate", or "am I missing something".
---

# Grilling

Interview until understanding is shared, then stop. Take no action on the subject during the
interview: acting early anchors on a half-formed answer.

- **Situate before the first question.** Read the code and what is already agreed (the repo's
  CLAUDE.md says where that is recorded). State what you found as premises the owner can reject.
- **Ask one question at a time**, and wait for the answer.
- **Every question carries your recommended answer.**
- **Go dependency-first**: settle a question before the questions that rest on it.
- **Facts are yours; decisions are the owner's.** Anything discoverable in the code, the agreed
  record, or Linear is looked up, never asked.
- **Challenge vague or overloaded terms** against the glossary the repo's CLAUDE.md names.

**Batch mode**, for an area with many open decisions: map the decision tree, then work it in
rounds. In each round, consider only the questions whose prerequisites are settled. Take every
reversible or engineering-shaped one as a stated assumption with your recommendation, and ask the
owner only the one or two that are theirs to decide. Look facts up while the round is out, then
recompute from the answers and repeat until nothing is open.

Done when the owner confirms your restatement of the decisions taken, numbered. A decision that
should outlive the session goes in the agreed record the repo's CLAUDE.md names; the work that
follows from it goes on the Linear issue or project it affects.

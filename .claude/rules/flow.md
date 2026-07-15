---
paths:
  - ".claude/**"
  - ".github/ISSUE_TEMPLATE/**"
  - "LOOP.md"
  - "loops/**"
---

# Flow (operating model)

- **Operating-model changes are their own PR, with rationale.** Anything under `.claude/` (rules,
  agents, skills, hooks, settings), `LOOP.md`, `loops/`, or the issue templates changes in a
  dedicated PR that explains why — never mixed into feature work. A run never edits the loop
  definitions, agents, rules, or constraints it is itself using.
- **A new agent definition PR must state three things:** the non-derivable knowledge the agent
  captures, why the existing agents don't already cover it, and its model tier.
- **Model tiers:** implementation and reasoning agents run `opus`; mechanical / prescribed-steps
  agents run `sonnet`; nothing pins `fable`.
- Labels are display output only — nothing reads a label as control flow. The router classifies
  each work item fresh from its content every pass.

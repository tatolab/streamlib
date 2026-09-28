---
name: batch-grilling
description: Frontier-batched grilling for large alignment sessions — plan sections, pivots, roadmap debates. Works the question frontier round by round until nothing is open, asking the owner only what is theirs to decide. Use for plan sessions, or when the owner says "batch grill me" or wants a whole area settled in one sitting.
---

# Batch grilling

The round-based variant of `grilling` for sessions with many open decisions.

1. **Map the decision tree** for the area: every open question, and which questions
   depend on which answers.
2. **Compute the frontier** — every question whose prerequisites are settled. A question
   whose answer depends on another question still open this round belongs to a later
   round; never pre-ask it.
3. **The frontier is a ceiling on what may be asked, not a list to ask.** Take every
   reversible or engineering-shaped question as a stated assumption with your
   recommendation. Ask only what is genuinely the owner's — one or two questions at a
   time, each with a recommended answer and a line on what's driving it. Wait for the
   owner's answers.
4. **Facts never enter a round.** Anything discoverable is dispatched to a read-only
   subagent while the round is out — don't block on it; only questions downstream of
   that exploration wait for it.
5. Recompute the frontier from the answers and repeat.

Done when the frontier is empty: restate every decision taken, numbered, and get one
final yes.

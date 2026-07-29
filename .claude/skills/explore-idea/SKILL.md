---
name: explore-idea
description: Free-form what-if exploration of a direction before it becomes architecture — a sandbox with zero commitments. Use when an idea is fuzzy, when unknowns are too many for a change proposal, when a milestone has become a mess of tickets with no clear architecture behind it, or when the owner says "what if", "explore", or "I want to think through".
disable-model-invocation: true
---

# Explore idea

The pressure-release valve before the pipeline: ideas too fuzzy or too broad for
`/propose-change` get explored here first, so unknowns are burned down before any work
stream exists. Nothing in this skill commits anything: **no tickets, no plan edits, no
code, no milestones.**

1. **Situate.** Read the plan, the relevant code, and any existing tickets circling the
   idea. When the input is a messy milestone, reverse-engineer what its tickets
   collectively want — the coherent picture they never stated — and present that back
   first.
2. **Explore with the owner.** Sketch 2–3 genuinely different shapes the idea could take
   (Mermaid sketches welcome). For each: the unknowns it carries, its cost class (one
   change / one milestone / several milestones), and what existing code or plan entries
   it would displace or delete. Use `grilling` when the owner's intent itself needs
   sharpening.
3. **Drive to identical understanding.** Say the idea back in your own words until the
   owner confirms it matches what is in their head. Unconfirmed = unexplored.
4. **Exit explicitly**, one of three ways:
   - **Graduate** → `/align` (it needs plan sections decided) or `/propose-change` (the
     plan already covers it and the delta is now crisp).
   - **Park** → memo at `docs/research/explorations/<slug>.md`: the idea, the shapes
     considered, and what unknown must resolve before it revives.
   - **Kill** → same memo, with the reason — so the idea is not innocently re-explored
     in three months.

If exploration reveals the idea is really N ideas, or more than one milestone of work,
say so and split before graduating — a huge milestone discovered at ticket time is this
skill having been skipped.

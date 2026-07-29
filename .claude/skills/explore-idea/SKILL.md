---
name: explore-idea
description: Free-form what-if exploration of a direction before it becomes architecture — a sandbox with zero commitments that ends with a concrete recommended starting shape (spike / prototype / MVP slice). Use when an idea is fuzzy, when unknowns are too many for a change proposal, when a milestone has become a mess of tickets with no clear architecture behind it, or when the owner says "what if", "explore", or "I want to think through".
disable-model-invocation: true
---

# Explore idea

The pressure-release valve before the pipeline: ideas too fuzzy or too broad for
`/propose-change` get explored here first, so unknowns are burned down before any work
stream exists. Nothing in this skill commits anything: **no tickets, no plan edits, no
code, no milestones.**

This skill is proactive, not just inquisitive — the owner brings a fuzzy want; Claude's
job is to give it a shape they can react to. Digging and questioning are means; the
deliverable is a formed picture plus the smallest sensible way to start.

1. **Situate.** Read the plan, the relevant code, and any existing tickets circling the
   idea. When the input is a messy milestone, reverse-engineer what its tickets
   collectively want — the coherent picture they never stated — and present that back
   first.
2. **Sharpen the fuzz with `grilling`.** One question at a time, each with a
   recommended answer, until what's in the owner's head and what's in Claude's are the
   same idea. Facts get looked up, never asked.
3. **Sketch 2–3 genuinely different shapes** the idea could take (Mermaid sketches
   welcome). For each: the unknowns it carries, its cost class (one change / one
   milestone / several milestones), and what existing code or plan entries it would
   displace or delete.
4. **Shape the smallest start.** This is the step that prevents accidental overbuilding
   — "sounds easy on paper" is how a weekend idea becomes a quarter. Place the idea on
   the build ladder and recommend the lowest rung that teaches something real:
   - **Spike / proof-of-concept** — hours, throwaway, exists to answer exactly ONE named
     question. Say what the question is.
   - **Prototype** — demoable and ugly; no error handling, no tests, clearly marked
     throwaway. Exists to be reacted to.
   - **MVP slice** — the smallest *showable* version a user or the owner can actually
     run; survives and gets iterated on.
   - **Full change** — only when the idea is already well-understood and small.

   State the recommendation plainly: the smallest version worth building, the explicit
   **deferred list** (what the full idea includes that this deliberately does not), and
   the iteration path from smallest to target. When the idea is iterable, bias one rung
   smaller than feels natural — iteration is cheap, unbuilding is not.
5. **Drive to identical understanding.** Say the idea AND the recommended starting shape
   back in your own words until the owner confirms it matches what is in their head.
   Unconfirmed = unexplored.
6. **Exit explicitly**, one of three ways — the recommended starting shape travels with
   the exit:
   - **Graduate** → `/align` (it needs plan sections decided) or `/propose-change` (the
     plan already covers it and the delta is now crisp — scoped to the starting rung,
     not the full dream).
   - **Park** → memo at `docs/research/explorations/<slug>.md`: the idea, the shapes
     considered, the recommended start, and what unknown must resolve before it revives.
   - **Kill** → same memo, with the reason — so the idea is not innocently re-explored
     in three months.

If exploration reveals the idea is really N ideas, or more than one milestone of work,
say so and split before graduating — a huge milestone discovered at ticket time is this
skill having been skipped.

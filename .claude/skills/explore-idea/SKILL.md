---
name: explore-idea
description: Free-form what-if exploration of a direction before it becomes work, ending in a recommended smallest starting shape. Use when an idea is fuzzy, when it has too many unknowns for an issue, when a set of issues has no clear shape behind it, or when the owner says "what if", "explore", or "I want to think through".
---

# Explore idea

A sandbox with no commitments: no work issues, and code only as a throwaway spike when that is the
fastest way to answer a question. The owner brings a fuzzy want; give it a shape they can react to.

1. **Situate.** Read the relevant code, what the repo's CLAUDE.md names as the record of what is
   agreed, and the Linear issues around the idea. When the input is a messy set of issues, say
   back what they collectively want first.
2. **Sharpen it with `grilling`.**
3. **Sketch two or three genuinely different shapes.** For each: its unknowns, its size (one
   issue, a milestone, a project), what it needs from another repo, and what it would displace.
4. **Recommend the lowest rung that teaches something real**:
   - **Spike** — hours, throwaway, answers exactly one named question.
   - **Prototype** — demoable, ugly, untested, clearly throwaway; exists to be reacted to.
   - **MVP slice** — the smallest version someone can actually run; it survives and is iterated.
   - **Full change** — only when the idea is already small and well understood.

   Name the deferred list (what the full idea has that this start does not) and the path from the
   start to the target. When the idea can be iterated, pick one rung lower than feels natural.
5. **Say the idea and the recommended start back** and check the owner recognises both.
6. **Exit one of three ways**:
   - **Graduate** — `file-issue` for one piece of work, `plan-project` for more, scoped to the
     recommended start.
   - **Park** — a document on the Linear project it belongs to: the idea, the shapes, the
     recommended start, and the unknown that must resolve before it revives.
   - **Kill** — the same document, with the reason, so the idea is not explored again by accident.

If the idea turns out to be several ideas, split it before graduating.

# Workflow: research-labeled issues

Applies to issues labeled `research` — investigations that deliver a
document or decision, not implementation code.

## Deliverable shape

**The primary deliverable is the set of implementation issues the
research generates — the work the team picks up next.** A research
issue earns its keep by emitting concrete, well-scoped follow-up
issues with native `Blocked by` / `Blocks` edges, not by producing a
document.

A reference markdown file under `docs/research/` (or the appropriate
`docs/` subfolder) is **optional** — useful when the synthesis is
large enough that consolidating it into a single document is cheaper
to read than scattering the same content across every generated
issue body. When the synthesis fits cleanly inside the issue bodies,
skip the doc.

In every case the research should also produce:

- A question list for the user to review if consensus isn't reached.
- An unambiguous recommendation when the data supports one.

**No code in a research PR**, except where the investigation produced a
trivial throwaway reproducer — and even then, don't wire it into the
workspace build.

## What to avoid

- Don't "start implementing while researching" — that blurs the
  deliverable and makes the PR harder to review. File a separate
  implementation issue if the research concludes with a clear next
  step.
- Don't fold a research issue into a larger implementation PR —
  research stands alone.
- Don't skip the question-list section. Even if the answer seems
  obvious, the user wants to see the alternatives you considered.

## Structure of the research doc

```markdown
# Research: <topic>

## Question

<One-sentence formulation of what this research answers>

## Context

<Why the question matters, what's blocked behind it>

## Alternatives considered

### Option A: <name>
<description, pros, cons, evidence, rough cost>

### Option B: <name>
<...>

## Recommendation

<Which option, and why. If no clear winner, say so explicitly and list
the question(s) that would break the tie.>

## Open questions for the user

- <specific decisions the user needs to make before work starts>
```

## PR body additions

```markdown
## Research outcome

- **Implementation issues filed**: #<N1>, #<N2>, … (primary deliverable)
- **Recommendation**: <one-line summary>
- **Reference doc** (optional): <path, or "none — content lives in the issue bodies">
- **Open questions**: <list, or "none">
```

## When not to use this workflow

If the user asks for "research on X" but the answer is genuinely
obvious (e.g., a quick API lookup), skip the full process — write a
comment on the issue with the answer and close it. The research
workflow exists to force a rigorous process for genuinely open
questions, not to bureaucratize trivial lookups.

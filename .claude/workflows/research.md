# Workflow: research-labeled issues

Applies to issues labeled `research` — investigations that deliver a
document or decision, not implementation code.

## Deliverable shape

A research issue is done when it produces a reviewable artifact that
lets the user (or a future agent) make a decision. Concretely:

- A markdown file under `docs/research/` or the appropriate `docs/`
  subfolder.
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

- **Artifact**: <path to the doc created>
- **Recommendation**: <one-line summary>
- **Follow-up implementation issue**: #<N> or "not yet filed"
- **Open questions**: <list, or "none">
```

## When not to use this workflow

If the user asks for "research on X" but the answer is genuinely
obvious (e.g., a quick API lookup), skip the full process — write a
comment on the issue with the answer and close it. The research
workflow exists to force a rigorous process for genuinely open
questions, not to bureaucratize trivial lookups.

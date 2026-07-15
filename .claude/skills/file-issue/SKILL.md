---
name: file-issue
description: File a well-shaped GitHub issue from a short intent — pick the right form (feature / bug / research), fill its sections, infer kind/zone/milestone, and set native dependency edges. Use when Jonathan says "file this", "open an issue for X", "log that bug", or when discussion surfaces a trackable task. Never files an umbrella issue — decomposes into self-contained issues with real blocked-by edges.
---

# file-issue

Turn an intent into a GitHub issue that a future picker can pick up cold. The issue is the goal, not a spec — carry the design content but keep implementation mechanics out (they rot; the picker re-derives them).

## 1. Pick the form
Match the intent to one of `.github/ISSUE_TEMPLATE/`:
- **feature** — a change to the engine, a package, or the tooling.
- **bug** — something behaves wrong: a crash, wrong output, a broken invariant.
- **research** — a question to answer before building; produces a doc/decision, not code.

Read the chosen form's YAML and fill **its** sections — don't invent a structure. As of now:
- **feature**: What & why · Design (mermaid + trade-offs; required before build for engine-zone work — `draft-design` fills it) · Done means (2–4 outcome criteria) · Validation shape (test/check shape, not names) · Needs the physical rig? (GPU/camera/audio/none) · Non-derivable notes (hidden invariants / ruled-out approaches with the why; default "None").
- **bug**: Symptom (exact error strings / VUIDs / behavior) · Where it bit · Expected · Repro (smallest sequence) · Needs the physical rig?
- **research**: Question · Why now · Deliverable shape.

## 2. Infer kind / zone / milestone
Infer the form (kind), the zones touched, and the milestone from the intent and the current code. **Milestone assignment is required** — every issue belongs to one. When confidence is low on any of these, **ask Jonathan inline** (`AskUserQuestion`) rather than guessing — a wrong milestone or a mis-scoped issue costs more than one question. Milestone scoping is his call.

Labels are display output only; the router will classify the issue fresh at pickup, so don't over-label to steer it.

## 3. Never file an umbrella issue
An issue that says "do all of X" and lists sub-tasks is the failure mode this skill prevents. Decompose into **self-contained issues**, each with its own Done-means and validation shape, connected by **native GitHub dependency edges** — not text like "depends on #N."

Set the edges through whichever `gh` surface this repo actually exposes — **verify which exists before using it**: the GraphQL issue-relationship mutations (add-blocked-by) or the sub-issue relationship API. Check with a probe (e.g. inspect `gh api graphql` for the relationship mutation, or `gh` sub-issue support) and use the one that works; the `Related` section of a body is for free-text context only, never for dependency edges.

## 4. Phrase claims honestly
Any specific claim about code or behavior in the body is phrased "to the best of our current knowledge" so the picker knows to verify it. Keep file paths, exact test names, and step-by-step ordering out of the body.

## 5. Confirm and report
Show the drafted issue(s) and the edges before filing when the decomposition is non-trivial; file, then report the issue number(s), milestone, and the dependency graph you set. If the intent decomposed into several, list them with their blocks/blocked-by relationships so Jonathan can see the shape.

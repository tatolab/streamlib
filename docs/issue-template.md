# Writing good GitHub issues for streamlib

GitHub is the source of truth for work in this repo. Milestones group deliverables; issues track the individual tasks inside each milestone. Amos is the local cache + AI-context layer that sits on top — it does not *own* the work, it *reflects* what's in GitHub.

This doc is the template every new issue should follow, and the contract agents (and humans) are expected to honor.

## Keep issues low-resolution

An issue captures the *goal* of a piece of work — the problem to
solve, why it matters, and roughly how done looks. The high-resolution
plan (specific file paths, exact test names, suggested implementation
ordering, ruled-out approaches) decays as the surrounding code shifts,
so capturing it in the issue body just creates staleness for the next
agent to clean up. Resist the urge to be exhaustive.

The picker's job is to research current state at pickup time and
produce a fresh implementation plan; the filer's job is to capture
the goal cleanly enough that a competent agent can pick it up cold
and figure the rest out.

What this means in practice:

- **Description** is a short paragraph stating the goal, not a
  pre-implementation plan.
- **Context** explains *why* the work matters and what constraints
  bound it. It does not summarize an investigation that the picker
  could redo themselves.
- **Exit criteria** are 2–4 high-level deliverables that define
  "done," not a checklist of file edits.
- **Tests / validation** describes the *shape* of validation needed
  (unit test, E2E scenario, harness reference), not the exact test
  function names.
- **AI Agent Notes** are reserved for things the picker genuinely
  cannot derive from current code (a hidden invariant, a ruled-out
  approach with a reason). When in doubt, leave it as "None."
- **Phrase claims as "to the best of our current knowledge"** when
  the issue body must reference specific code or behavior. This
  signals to the picker that the claim deserves verification.

The picker is required (per CLAUDE.md → Work Tracking → "Issues are
goals, not specs") to verify the issue body against current code
before announcing a plan, and to update the body in place when
something has gone stale. Plan for that loop; don't try to make the
issue body authoritative forever.

## Template

```markdown
## Description

One short paragraph stating the goal — *what* this issue delivers and
roughly *how* done looks. Written for a future agent picking this up
cold. No ruled-out approaches, no file paths, no implementation
ordering.

## Context

Why this matters and what constraints bound the work — adjacent
milestones, prior work, the architectural commitment that makes this
the right shape. Phrase specific claims as "to the best of our
current knowledge" so the picker knows to verify.

## Exit criteria

2–4 high-level deliverables that define "done." Resist breaking
each one down further; the picker will produce the detailed plan.

- [ ] <high-level deliverable 1>
- [ ] <high-level deliverable 2>

## Tests / validation

The *shape* of validation needed, not exact test names. Either:

- **Inline scope** — the kinds of tests this issue should add:
  - [ ] Unit test(s) covering <what behavior>
  - [ ] E2E scenario: <one-line description>

- **OR** a reference to a test-harness issue:
  - Blocked by #N (test harness for <area>)

The picker fills in the specifics during plan-out.

## Related

- Milestone: <name>
- See also: #N (free-text context only — dependency edges go through
  GitHub's `Blocked by` / `Blocks` / `Parent`, not text).

<!-- amos:ai-notes-begin -->
## AI Agent Notes

None.

(Or: things the picker genuinely cannot derive from current code — a
hidden invariant, a ruled-out approach with reasoning, a non-obvious
gotcha. Default to "None." — absence must be deliberate, not
forgotten, but adding low-value content that will go stale is worse
than leaving the section empty.)
<!-- amos:ai-notes-end -->
```

**Dependency edges are native GitHub relationships**, not text. Set
`Blocked by` / `Blocks` / `Parent` via GitHub's issue UI (or
`gh api graphql` / `amos sync-edges`) — they don't go in the `Related`
section. The `Related` section is for free-text context only ("see
also", "context from #N", etc.).

## Rules agents must follow

1. **GitHub is the source of truth.** Every issue — description, exit
   criteria, tests, dependency edges, AI-agent notes — lives in the
   issue itself. Local plan files are deprecated; don't create new ones.
2. **Keep it low-resolution.** When in doubt, leave detail out. The
   picker will research current state and produce the high-resolution
   plan; specifics in the issue body just create staleness.
3. **Every issue includes an AI Agent Notes section** (wrapped in the
   `<!-- amos:ai-notes-begin -->` / `<!-- amos:ai-notes-end -->` markers
   so tooling can update it safely). Default to "None."; only add
   content that's genuinely non-derivable from current code.
4. **Every issue has exit criteria.** No exit criteria = scope is unclear.
   Push back and refine before starting work. But keep the criteria
   high-level — 2–4 items, not a 12-item checklist.
5. **Every non-trivial issue has a Tests / validation section**, even if
   the answer is "no tests — pure doc change" (write that explicitly so
   reviewers know it was considered, not forgotten). Describe shape,
   not specifics.
6. **Test harnesses are their own issues.** If validating an issue requires
   building new test infrastructure, that infrastructure is its own issue
   with its own exit criteria (the harness exists and works) and its own
   test cases (the harness catches the scenarios it's supposed to catch).
7. **Milestone assignment is required.** Every issue belongs to a milestone.
   If it doesn't fit any existing milestone, either the milestone's scope
   is wrong or a new milestone is warranted — raise it before filing the
   issue.
8. **Cross-cutting concerns are labels, not milestones.** Linux-specific
   work goes in the relevant deliverable milestone with a `linux` label.
   "Linux support" is not a deliverable; "Pipeline Color & Resolution" is.
9. **`polyglot`-labeled issues must answer: are Python AND Deno both
   covered?** The default is yes — pipeline-level work (new processor +
   scenario binary, new escalate op end-to-end, new FD-passing story)
   ships both runtimes together or files paired tickets that block on
   each other and land in the same milestone. The only legitimate split
   is *schema-only / language-specific by construction* (e.g. a Python
   ctypes ABI bug that doesn't exist in the Deno FFI binding); say so
   explicitly in the issue body. "Python first, Deno deferred" is the
   failure mode this rule exists to prevent — see #468 and
   `.claude/workflows/polyglot.md`.

## What this means for CI

Once CI is wired (see the *CI & Test Infrastructure* milestone), the
"Tests / validation" section becomes the gate: the tests the picker
ends up writing must pass in CI before the PR can merge. Test
harnesses land first, tests land inside the issue that drives them,
and the merge signal is automatic.

## Example — a well-formed (low-resolution) issue

```markdown
## Description

Route encoder/decoder Vulkan submissions through `VulkanDevice`'s
mutex-protected submit path so concurrent processor threads can't
race on `vkQueueSubmit`. Goal: release-build encode/decode pipeline
runs without the cross-thread SIGSEGV currently observed.

## Context

The per-queue mutex exists on `VulkanDevice` but the codec processors
appear to bypass it, defeating the guard. To the best of our current
knowledge this is the cause of the release-build SIGSEGV seen when
encoder and decoder submit from different threads — verify against
current code at pickup, the structure may have shifted.

## Exit criteria

- [ ] Codec processors no longer submit to the queue outside the
      RHI's mutex-protected path.
- [ ] Release build runs the encoder/decoder roundtrip end-to-end
      without crashing across multiple cold runs.

## Tests / validation

- [ ] Unit test exercising concurrent submission across two threads
      and asserting no race.
- [ ] E2E: encoder/decoder roundtrip per `docs/testing.md`, release
      build, multiple cold runs.

## Related

- Milestone: Vulkan Video RHI Coupling
```

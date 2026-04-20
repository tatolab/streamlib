# Writing good GitHub issues for streamlib

GitHub is the source of truth for work in this repo. Milestones group deliverables; issues track the individual tasks inside each milestone. Amos is the local cache + AI-context layer that sits on top — it does not *own* the work, it *reflects* what's in GitHub.

This doc is the template every new issue should follow, and the contract agents (and humans) are expected to honor.

## Template

```markdown
## Description

One short paragraph, written for an AI agent with no prior context. Say *what*
this issue delivers in plain terms. Avoid "as discussed in slack" — link or
quote.

## Context

Why this matters. Architectural constraints, prior work, adjacent milestones,
anything a reader needs to understand *before* deciding the scope is right.

## Exit criteria

Concrete, checkable deliverables. An agent or reviewer should be able to
tick each item and know the issue is done.

- [ ] <deliverable 1>
- [ ] <deliverable 2>

## Tests / validation

What proves this works? Every non-trivial issue answers this section. Either:

- **Inline test cases** — unit tests, integration tests, or E2E scenarios to
  write as part of this issue. Each listed as a bullet so reviewers can check
  them off:
  - [ ] `<module>::<test_name>` — <what it exercises>
  - [ ] E2E: <scenario description>

- **OR** a reference to a test-harness issue: if the tests need scaffolding
  that doesn't exist yet, file that scaffolding as its own issue and mark
  this one blocked on it.
  - Blocked by #N (test harness for <area>)

The intent: once CI exists, a PR merging this issue is only reviewable if the
listed tests pass. See `docs/testing.md` for which test types apply when.

## Related

- Milestone: <name>
- Blocked by: #N
- Blocks: #N
- Related to: #N
```

## Rules agents must follow

1. **One source of truth.** Create the issue in GitHub. Don't also create a
   parallel amos plan file with duplicated content. Amos reflects GitHub,
   not the other way around.
2. **If local AI instructions are needed**, put them in an amos plan file
   with the same `@github:tatolab/streamlib#N` identifier. The plan file
   should contain *only* AI-agent-specific context (short-form notes,
   references to local files, anything that doesn't belong in the public
   issue). Never duplicate the description, exit criteria, or test list.
3. **Every issue has exit criteria.** No exit criteria = scope is unclear.
   Push back and refine before starting work.
4. **Every non-trivial issue has a Tests / validation section**, even if
   the answer is "no tests — pure doc change" (write that explicitly so
   reviewers know it was considered, not forgotten).
5. **Test harnesses are their own issues.** If validating an issue requires
   building new test infrastructure, that infrastructure is its own issue
   with its own exit criteria (the harness exists and works) and its own
   test cases (the harness catches the scenarios it's supposed to catch).
6. **Milestone assignment is required.** Every issue belongs to a milestone.
   If it doesn't fit any existing milestone, either the milestone's scope
   is wrong or a new milestone is warranted — raise it before filing the
   issue.
7. **Cross-cutting concerns are labels, not milestones.** Linux-specific
   work goes in the relevant deliverable milestone with a `linux` label.
   "Linux support" is not a deliverable; "Pipeline Color & Resolution" is.

## What this means for CI

Once CI is wired (see the *CI & Test Infrastructure* milestone), the
"Tests / validation" section becomes the gate: the tests listed must pass
in CI before an issue can be considered merge-ready. Test harnesses land
first, tests land inside the issue that drives them, and the merge signal
is automatic.

## Example — a well-formed issue

```markdown
## Description

Route `SimpleEncoder::queue_submit()` calls through `VulkanDevice`'s
mutex-protected `submit_to_queue()` method so concurrent processor threads
can't race on `vkQueueSubmit`. Fixes the release-build SIGSEGV seen when
encoder and decoder submit from different threads.

## Context

#273 added the per-queue mutex on `VulkanDevice`. This issue makes the
encoder actually use it. Without this, the mutex exists but the encoder
still submits directly via `ash::Device`, defeating the guard.

## Exit criteria

- [ ] `SimpleEncoder::queue_submit` calls `VulkanDevice::submit_to_queue`
- [ ] Same change applied to `SimpleDecoder::queue_submit`
- [ ] Release build runs `vulkan-video-roundtrip h264 /dev/video2 30` without
      SIGSEGV for ≥3 consecutive cold runs

## Tests / validation

- [ ] `vulkan_video::tests::concurrent_encode_decode_no_race` — new unit
      test that spawns two threads submitting simultaneously and asserts
      no double-submit
- [ ] E2E: encoder/decoder scenario from docs/testing.md, release build,
      30s duration × 3 cold runs — see the standardized E2E template

## Related

- Milestone: Vulkan Video RHI Coupling
- Blocked by: #273
```

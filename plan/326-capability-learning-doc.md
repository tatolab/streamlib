---
whoami: amos
name: '@github:tatolab/streamlib#326'
adapters:
  github: builtin
description: 'Learning doc: GPU capability typestate pattern — Capture the "why" of the sandbox/escalate pattern in `docs/learnings/` so the invariant survives future refactors.'
github_issue: 326
blocked_by:
- '@github:tatolab/streamlib#325'
- '@github:tatolab/streamlib#369'
- '@github:tatolab/streamlib#370'
---

@github:tatolab/streamlib#326

## Branch

`docs/gpu-capability-learning` from `main` (post-#325, so the doc can cite concrete code).

## Steps

1. Create `docs/learnings/gpu-capability-typestate.md`. Cover:
    - The invariant: Sandbox in `process()`, FullAccess only inside `escalate` closures.
    - Why it matters: NVIDIA Linux concurrent-resource-creation races, per-queue parallelism model, DX/autocomplete enforcement.
    - Rejected alternatives (runtime phase check only, single render thread).
    - Links to the design doc from #320 for depth.
2. Update `docs/learnings/README.md` index.
3. Add a short bullet in `CLAUDE.md`'s "Hard-won learnings" section pointing to the new file.

## Verification

- Doc reads cleanly as a standalone learning — a contributor who hasn't touched #319 can understand the rule and the reason from this file alone.

## Parent

#319

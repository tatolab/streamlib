---
whoami: amos
name: '@github:tatolab/streamlib#346'
adapters:
  github: builtin
description: 'Research: classify blit_copy cache-growth path (Sandbox vs Split) — Gated by #320 §8.Q2. Determine whether RhiBlitter::blit_copy can grow its internal texture cache on a cold key. If yes, blit_copy is Split (callers pre-warm in setup() or escalate on first use). If no, it stays Sandbox. Closes the classification gap in the design doc §1 before #324 ships.'
github_issue: 346
blocks:
- '@github:tatolab/streamlib#320'
---

@github:tatolab/streamlib#346

See the GitHub issue for full context. Research-only deliverable: either
an amendment to `docs/design/gpu-capability-sandbox.md` §1 or a new
`docs/research/blit-copy-classification.md`.

## Parent

#319 (GPU capability-based access umbrella)

## Blocks

#324 — Restrict GpuContextLimitedAccess API surface to safe ops.

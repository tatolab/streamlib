---
whoami: amos
name: '@github:tatolab/streamlib#330'
adapters:
  github: builtin
description: 'Research: H.265 encoder quality configuration on Vulkan Video / NVIDIA — Audit the distinct quality-affecting knobs on our H.265 encode path (Vulkan API effort index vs. H.265 SPS profile/tier/level vs. QP/rate-control vs. tuning_mode) before #294 retest. #306''s framing conflated several concepts; reviewer recalls a specific proper H.265 configuration fix. Research-only deliverable with a question list for interactive review.'
github_issue: 330
blocks:
- '@github:tatolab/streamlib#287'
- '@github:tatolab/streamlib#288'
- '@github:tatolab/streamlib#289'
- '@github:tatolab/streamlib#290'
- '@github:tatolab/streamlib#291'
- '@github:tatolab/streamlib#292'
- '@github:tatolab/streamlib#296'
- '@github:tatolab/streamlib#300'
- '@github:tatolab/streamlib#302'
- '@github:tatolab/streamlib#303'
- '@github:tatolab/streamlib#304'
- '@github:tatolab/streamlib#305'
- '@github:tatolab/streamlib#306'
- '@github:tatolab/streamlib#315'
- '@github:tatolab/streamlib#316'
---

@github:tatolab/streamlib#330

See the GitHub issue for full context. This task is research-only: the
deliverable is `docs/research/h265-encoder-quality-knobs.md` plus a
question list to review interactively with Jonathan. No encoder code
changes land here; any implementation is scoped as a follow-up that
this research gates.

Gating #294 retest so the retest doesn't run against a known-misframed
H.265 quality configuration.

---
whoami: amos
name: '@github:tatolab/streamlib#343'
adapters:
  github: builtin
description: '[BLOCKED — do not start] Umbrella: End-to-end CI lifecycle — GPU runners, validation gates, hermetic harness — BLOCKED — queued behind #213 (container/GPU runner), #337 (hermetic harness), #338 (VkInstance validation-layer wiring). Do NOT pick up. Umbrella that replaces the narrow #293 approach with a full CI lifecycle: reviving the disabled test.yml, wiring unit-test / clippy / fmt gates, adding validation-layer roundtrip gates for H.264/H.265, wiring the PSNR fixture rig, and packaging the dev-bootstrap + container image.'
github_issue: 343
blocked_by:
- '@github:tatolab/streamlib#213'
- '@github:tatolab/streamlib#337'
- '@github:tatolab/streamlib#338'
---

@github:tatolab/streamlib#343

# 🛑 STOP — DO NOT WORK ON THIS PLAN YET 🛑

This plan is **intentionally queued**. Every child task either requires
a GPU runner that doesn't exist yet (#213), validation-layer wiring
that isn't there yet (#338), or would flake on the shared runner under
NVIDIA's driver-state contamination (#337). Adding any of the CI jobs
below before those land would either skip vacuously or be flaky enough
to poison the signal.

## Why this exists

#293 originally set out to add a single CI job that runs a roundtrip
under `VK_LOADER_LAYERS_ENABLE=*validation*` and fails on any
`Validation Error`. The #294 retest exposed that this is far too
narrow — the real gap is that StreamLib has no GPU-capable CI at all.

Rather than land a narrow task that doesn't actually close the CI gap,
#293 was closed with its discovery written up, and this umbrella was
opened to hold the full CI-lifecycle plan so it can be built as one
coherent initiative once the prerequisites land.

## Dependencies

- #213 — Container/Docker integration for cloud GPU deployment
  (runpod). Without this, there is no runner that can execute Vulkan
  Video at all.
- #337 — E2E harness non-hermetic on NVIDIA Linux. Without this,
  multi-scenario CI runs are non-deterministic.
- #338 — `StreamRuntime` must honor `VK_LOADER_LAYERS_ENABLE` /
  `VK_INSTANCE_LAYERS`. Without this, validation output is vacuously
  zero and the validation-layer gate is meaningless.

## Scope

Children to file and sequence once this unblocks:

1. **Runner helper script** — spawns an example with validation env,
   parses output, exits non-zero on `Validation Error`. Portable, no
   GPU required to write; can be tested against any example once #338
   lands.
2. **Re-enable `test.yml`** — the workflow is currently
   `workflow_dispatch`-only for cost. Revive PR / push triggers once
   #213 defines the runner target.
3. **Validation-layer CI jobs** — H.264 vivid roundtrip, H.265 vivid
   roundtrip, each under the helper. Zero `Validation Error` pass bar.
4. **PSNR fixture-rig CI job** — run `e2e_fixture_psnr.sh` for H.264
   and H.265 on a schedule; fail on Y PSNR drops below the WARN
   threshold.
5. **Unit / clippy / fmt gates** — re-enable the existing Linux and
   macOS jobs in `test.yml`.
6. **Allowlist mechanism** — if the post-#338 baseline still shows
   benign VUIDs (unlikely but possible), add an allowlist file and CI
   logic to ignore them while failing on anything new.
7. **Dev-bootstrap / container image** — ensure
   `vulkan-validationlayers` is installed in both the dev setup script
   and whatever container image #213 produces, so local runs match CI.

## Non-goals

- Containerization design itself — that's #213.
- Validation-layer wiring itself — that's #338.
- Harness hermeticity itself — that's #337.
- Adding new E2E scenarios beyond what `docs/testing.md` already
  specifies.

## Supersedes

- #293 — narrow "add a validation-layer CI job" task, closed with its
  PR capturing the discovery.

## Reference

- `docs/retests/294-post-vulkan-cleanup-retest.md` — items C (P1
  harness) and D (P2 validation wiring) that blocked the original
  #293 premise.
- `.github/workflows/test.yml` — the disabled workflow this umbrella
  will revive.

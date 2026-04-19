---
whoami: amos
name: CI with Vulkan validation layer
status: completed
description: Superseded by #343. The original narrow plan assumed StreamRuntime already honored VK_LOADER_LAYERS_ENABLE. #294's retest disproved that and surfaced two other blockers (#337 hermeticity, #213 GPU runner), so the task was closed with its discovery documented and the full CI-lifecycle scope was moved to umbrella #343.
github_issue: 293
dependencies:
  - "down:Retest camera + encoder + display roundtrip after Vulkan cleanup"
adapters:
  github: builtin
---

@github:tatolab/streamlib#293

## Outcome

**Closed without implementation — superseded by #343.**

The narrow plan below was drafted before the #294 retest. That retest
(see `docs/retests/294-post-vulkan-cleanup-retest.md`, follow-up items
C and D) surfaced three blockers that invalidate the original approach:

1. **#338 — `StreamRuntime` does not honor `VK_LOADER_LAYERS_ENABLE` /
   `VK_INSTANCE_LAYERS`.** Confirmed locally: `libs/streamlib/src/vulkan/rhi/`
   has no `ppEnabledLayerNames` wiring. Running any example under
   `VK_LOADER_LAYERS_ENABLE=*validation*` today produces zero VUIDs
   because the layer is never actually loaded, so the CI gate would
   pass vacuously.
2. **#337 — E2E harness non-hermetic on NVIDIA Linux.** Multi-scenario
   runs in one shell hit driver-state contamination (`DEVICE_LOST` in
   position N that passes cold). Any CI job that runs more than one
   scenario back-to-back is unreliable until the harness adds
   per-scenario process isolation, GPU-idle barrier, and cooldown.
3. **#213 — No GPU-capable CI runner exists.** GitHub's
   `ubuntu-latest` has no GPU; Vulkan Video encode/decode requires
   NVENC/NVDEC hardware. Container / cloud-GPU runner design lives in
   #213 and is not built.

Given those, a minimal validation-layer job landed in isolation would
either pass vacuously (pre-#338) or flake on driver contamination
(pre-#337) or fail to schedule at all (pre-#213). Rather than ship a
narrow task that doesn't close the real CI gap, #293 is closed and the
full CI-lifecycle plan moves to umbrella **#343**, which waits for all
three blockers before any CI work resumes.

## Original plan (for reference)

### Branch

Create `test/ci-validation-layer` from `main`.

### Steps

1. Add `vulkan-validationlayers` to the CI runner image / dev
   bootstrap script.
2. Create a runner helper (shell or Rust) that spawns an example under
   `VK_LOADER_LAYERS_ENABLE="*validation*"`, parses output, and exits
   non-zero on any `Validation Error`.
3. Add a CI job that runs a ≤ 5 s vivid H.264 roundtrip under the
   helper.
4. Baseline: land after #294 rollup retest confirms #287-#292 and #296
   fixes cleaned up the validation output. No allowlist needed.

### Verification

- CI job exists and runs on PRs.
- New validation errors introduced by a PR fail its build.

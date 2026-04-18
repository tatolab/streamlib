---
whoami: amos
name: CI with Vulkan validation layer
status: pending
description: Run at least one release-build roundtrip in CI with VK_LOADER_LAYERS_ENABLE=*validation* and fail on any validation error.
github_issue: 293
dependencies:
  - "down:Retest camera + encoder + display roundtrip after Vulkan cleanup"
adapters:
  github: builtin
---

@github:tatolab/streamlib#293

## Branch

Create `test/ci-validation-layer` from `main`.

## Steps

1. Add `vulkan-validationlayers` to the CI runner image / dev bootstrap script.
2. Create a runner helper (shell or Rust) that spawns an example under `VK_LOADER_LAYERS_ENABLE="*validation*"`, parses output, and exits non-zero on any `Validation Error`.
3. Add a CI job that runs a ≤ 5 s vivid H.264 roundtrip under the helper.
4. Baseline: land after #294 rollup retest confirms #287-#292 and #296 fixes cleaned up the validation output. No allowlist needed.

## Verification

- CI job exists and runs on PRs.
- New validation errors introduced by a PR fail its build.

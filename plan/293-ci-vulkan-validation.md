---
whoami: amos
name: CI with Vulkan validation layer
status: pending
description: Run at least one release-build roundtrip in CI with VK_LOADER_LAYERS_ENABLE=*validation* and fail on any validation error.
github_issue: 293
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
4. Decide on baseline: either land after #287-#291 clean up the output, or start with an explicit allowlist of currently-known VUIDs and tighten as each issue closes.

## Verification

- CI job exists and runs on PRs.
- New validation errors introduced by a PR fail its build.

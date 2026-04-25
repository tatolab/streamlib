# Workflow: macOS-labeled issues

Applies to issues labeled `macos` — anything that touches Apple processor
code, IOSurface, Metal RHI, AVFoundation, or CoreAudio paths.

## What the agent needs to know first

- macOS and Linux code live in separate directories under
  `libs/streamlib/src/` — `apple/` and `linux/`. Platform `cfg` gates
  inside those directories are redundant; don't add them.
- The `surface_store.rs`, `gl_interop.rs`, and IOSurface-related code
  are slated for removal as part of the *Polyglot SDK Realignment*
  milestone. Don't extend them without checking that milestone's plan.
- Metal RHI is the macOS equivalent of Vulkan RHI on Linux. The RHI
  boundary rule applies identically — only `apple/rhi/` may call Metal
  directly; processors use `GpuContext`.

## Validating locally

If the current dev machine is Linux (it usually is), the agent cannot
run macOS E2E directly. Two options:

1. **Cross-check via compile only** — `cargo check --target
   aarch64-apple-darwin` on Linux compiles the macOS branch without
   running it. This catches type errors, missing cfg gates, stale
   Apple API usage.

2. **Defer the runtime check to a CI job or human tester** — file the
   runtime verification as a follow-up issue (using the issue
   template) and assign it to *Post-Typed-Ctx Verification* or a
   similar milestone. Note the gap explicitly in the PR body.

Prefer option 1 for every PR touching macOS code; option 2 is the
fallback when a real device test is needed and no macOS CI exists yet.

## Rules specific to macOS issues

- **Never edit Apple processor files on Linux without compile
  verification.** PR #322 did this and required the *Post-Typed-Ctx
  Verification* milestone to clean up; don't repeat that.
- **Don't add new IOSurface / CGL / XPC surface-share code.** If the issue
  seems to require it, escalate to the user — it's a sign the issue
  belongs in the *Polyglot SDK Realignment* milestone, not wherever it
  currently sits.
- **Respect the Metal RHI boundary** — the same rule as Vulkan RHI on
  Linux. No Metal API calls outside `apple/rhi/`.

## PR body additions

```markdown
## macOS verification

- **Cross-compile**: `cargo check --target aarch64-apple-darwin` result
- **Runtime verified**: yes on <hardware> | no — follow-up #<N> filed
- **Apple paths touched**: <list>
- **RHI boundary preserved**: yes (no Metal calls outside apple/rhi/)
```

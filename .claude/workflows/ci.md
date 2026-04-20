# Workflow: CI-labeled issues

This workflow applies to issues labeled `ci`. Read it **before** starting
work — the checks below are mandatory for any change that touches
`.github/workflows/`, `Cargo.toml` test gates, or shell helpers that CI
runs.

## What "done" means for a CI issue

A CI issue is done when the pipeline it adds **actually runs** and its
pass/fail signal is trustworthy. Specifically:

- The workflow file compiles (`gh workflow view` on the PR shows no schema
  errors).
- The job executes on at least one PR — either this one or a tail PR —
  and reports a result.
- The result is meaningful: a failure of the feature the job is supposed
  to catch produces a red CI; a healthy PR produces green.

"Compiles" is not enough. A CI job that passes without actually running
the check is worse than no job — it creates false confidence.

## Validation steps before opening the PR

1. **Workflow lint** — `gh workflow view <workflow.yml>` from the branch
   should parse without errors.
2. **Local dry-run** (where possible) — for shell helpers the workflow
   invokes, run them locally on the branch first. If they pass locally
   but fail in CI, the failure is environmental and needs
   `docs/learnings/` captured before the PR merges.
3. **Negative test** — deliberately break the code the job protects (in
   a throwaway commit on the branch), push, confirm the job turns red,
   then revert the break. This step is what proves the gate isn't
   vacuous.
4. **Green baseline** — after the negative test, a clean PR should show
   green. If it doesn't, something is flaky — flag it as a follow-up
   (`Fix flaky <job> under <condition>`) rather than merging a flaky
   gate.

## Things to check that have bitten us before

- `StreamRuntime` honoring `VK_LOADER_LAYERS_ENABLE` —
  @docs/learnings/nvidia-dma-buf-after-swapchain.md has notes. A
  validation-layer CI job is vacuous if the runtime ignores the env var.
- NVIDIA GPU runner quirks — see the non-hermetic harness docs. Cold-run
  vs. sequential-run results can diverge on shared runners; CI either
  needs the hermetic harness or has to run cold each time.
- `cargo test --workspace` excludes — consult
  @docs/testing-baseline.md for the canonical exclusion list. Don't
  add new excludes without updating that doc in the same PR.

## PR body template additions for CI issues

Beyond the default issue-template sections, add:

```markdown
## CI evidence

- **Workflow file**: <link to the file in this branch>
- **Green baseline run**: <link to a passing run of the new job>
- **Negative test run**: <link to the run where the deliberate break produced red>
- **Environment validated**: <hardware / runner / env vars the job depends on>
```

Without these four links, the PR isn't ready for review — the gate's
real-world behaviour is unverified.

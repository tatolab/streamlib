---
paths:
  - ".claude/**"
  - ".github/ISSUE_TEMPLATE/**"
  - ".github/workflows/**"
---

# Flow (operating model)

- **Operating-model changes are their own PR, with rationale.** Changes under `.claude/`
  (rules, agents, skills, hooks, settings) or the issue templates are easiest for the owner
  to review in a dedicated PR that explains why, rather than mixed into feature work.
- **Shared skills are identical in every Tatolab repo** and name no repo. What a skill needs
  from this repo — the local gates, the review agents, the proof a change needs — lives in
  `CLAUDE.md`'s `## Before you push`. A skill here that differs from its copy elsewhere is a
  defect; streamlib's own skills (the plan lifecycle, live verification) keep their names.
- **Work is tracked in Linear, never in GitHub issues.** GitHub Issues is the public front
  door; Linear's sync copies each report into Triage. An agent never creates, edits, comments
  on or closes a GitHub issue, and `permissions.deny` in `settings.json` refuses the commands
  and tools that would.
- **No other blocking guards.** Beside that deny and the `ask` entry on the licence files
  (`LICENSE`, `LICENSES/`, `docs/license/`), nothing here refuses or pauses an agent: no
  hook that denies or prompts, no skill locked against model invocation. A wall outlives its
  reason, strands work, and can't work under an orchestrator, where no one is at a prompt.
- **Judgement belongs in prose, not machinery.** When something should usually go a
  certain way, say so and why in `CLAUDE.md`, a rule, or a skill; CI can catch the
  mechanical part after the fact. A new hook is limited to cleanup or context and never
  stops a tool call.
- **A new agent definition PR must state three things:** the non-derivable knowledge the agent
  captures, why the existing agents don't already cover it, and its model tier.
- **Model tiers:** implementation and reasoning agents run `opus`; mechanical / prescribed-steps
  agents run `sonnet`; nothing pins `fable`.
- Linear's labels are `Bug`, `Feature` and `Improvement`, and nothing reads one as control
  flow.
- PR titles are conventional-commit typed (`type(scope): summary`), written for a reader; nothing
  reads a title to decide a version. Run `scripts/check-conventional-pr-title.sh "<title>"` before
  opening the PR.
- Every merge to `main` is a release (`release.yml`). The patch moves on every merge; the minor
  moves when the control contract's `x-contract-version` changed since the last release, or on a
  release dispatched with `bump: minor`; the major only on a dispatch with `bump: major`. The owner
  dispatches a minor or major; a session never does.
- Never hand-edit the version. `[workspace.package].version` and the manifests
  `release-version-files.json` lists are written by the release, on the commit it tags.

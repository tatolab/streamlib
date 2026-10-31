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
- **No blocking guards.** Nothing here refuses or pauses an agent: no `permissions.deny`, no
  hook that denies or prompts, no skill locked against model invocation. Skills describe the
  owner's reasoning and leave the agent free to choose. A wall written months ago outlives
  its reason, strands work, and teaches sessions to route around it — and it can't work at
  all under an orchestrator, where no one is at a prompt. The one exception is the `ask`
  entry on the licence files (`LICENSE`, `LICENSES/`, `docs/license/`), which are legal
  text no workflow step needs to touch.
- **Judgement belongs in prose, not machinery.** When something should usually go a
  certain way, say so and why in `CLAUDE.md`, a rule, or a skill; CI can catch the
  mechanical part after the fact. A new hook is limited to cleanup or context and never
  stops a tool call.
- **A new agent definition PR must state three things:** the non-derivable knowledge the agent
  captures, why the existing agents don't already cover it, and its model tier.
- **Model tiers:** implementation and reasoning agents run `opus`; mechanical / prescribed-steps
  agents run `sonnet`; nothing pins `fable`.
- Labels are display output only — nothing reads a label as control flow. Every work item is
  classified fresh from its content and from current code, never from its labels.
- PR titles are conventional-commit typed (`type(scope): summary`). The repo squash-merges, so the
  title becomes the commit release-please parses — a mistitled PR silently skips the version bump.
  No CI job checks it: run `scripts/check-conventional-pr-title.sh "<title>"` before opening the PR.
- Pre-1.0, `feat`, `fix` and `perf` bump the patch version; `!` or a `BREAKING CHANGE:` footer bumps
  the minor (`release-please-config.json`). `refactor` / `chore` / `docs` / `test` / `ci` don't
  bump — title an ABI- or behavior-affecting change that must move the version as `feat` (or
  breaking), never `refactor`.
- Never hand-edit the version. `[workspace.package].version`, `.release-please-manifest.json`, and
  any `Release-As:` footer belong to release-please alone; a manual bump desyncs its baseline.

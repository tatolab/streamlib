---
paths:
  - ".claude/**"
  - ".github/ISSUE_TEMPLATE/**"
  - ".github/workflows/**"
---

# Flow (operating model)

- **Operating-model changes are their own PR, with rationale.** Anything under `.claude/` (rules,
  agents, skills, hooks, settings) or the issue templates changes in a dedicated PR that explains
  why — never mixed into feature work. A session never edits the agents, rules, or skills it is
  itself using.
- **A new agent definition PR must state three things:** the non-derivable knowledge the agent
  captures, why the existing agents don't already cover it, and its model tier.
- **Model tiers:** implementation and reasoning agents run `opus`; mechanical / prescribed-steps
  agents run `sonnet`; nothing pins `fable`.
- Labels are display output only — nothing reads a label as control flow. Every work item is
  classified fresh from its content and from current code, never from its labels.
- PR titles are conventional-commit typed (`type(scope): summary`). The repo squash-merges, so the
  title becomes the commit release-please parses — a mistitled PR silently skips the version bump
  (CI gate: `.github/workflows/pr-title.yml`).
- Only `feat` (minor) and `fix` (patch) bump the version; `!` or a `BREAKING CHANGE:` footer is a
  breaking bump (pre-1.0 → minor). `refactor` / `chore` / `docs` / `test` / `ci` / `perf` don't
  bump — title an ABI- or behavior-affecting change that must move the version as `feat` (or
  breaking), never `refactor`.
- Never hand-edit the version. `[workspace.package].version`, `.release-please-manifest.json`, and
  any `Release-As:` footer belong to release-please alone; a manual bump desyncs its baseline.

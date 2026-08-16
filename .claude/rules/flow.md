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
- **Path guards ask; they never deny.** `permissions.deny` and a hook's `"deny"` decision are both
  banned — use `ask` and let the owner resolve it at the prompt. A hard wall cannot be overridden
  in the moment, so a scope written months ago outlives the reason for it and blocks a removal the
  plan already committed to; the session then routes around the wall, which is where the damage
  happens. Prompting keeps the friction and the reason while leaving the owner a way through.
  This is not permission to bend doctrine: the rules still say what is right, and an approved
  prompt only means the owner judged this edit to be the exception.
- **Guard only what a wrong edit makes expensive to undo** — in this repo that is the licence
  files and nothing else. Never guard prose. A guard fires on a path and
  cannot see intent, so guarding a directory whose edits are mostly routine records trains the
  session to escalate trivia and the owner to click through without reading — which costs the
  guard its meaning everywhere else. Judgement the session must exercise belongs in an always-on
  rule (`CLAUDE.md`), not in an `ask` entry; a path-scoped rule file loads too late to govern the
  decision to ask.
- **Adding an `ask` entry or a hook branch needs the same justification as an agent.** State what
  a wrong edit costs, why an always-on rule can't carry it, and what the session should do when it
  fires. An entry no one can justify in those terms is friction, and friction is not free.
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

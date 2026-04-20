# Task execution

> **Retired.** The old per-project task-execution protocol is now a global
> skill in amos.

To pick up the next task, invoke **`/amos:next`** (or say "continue",
"next task", "what's next", etc. — the skill triggers on natural phrasing).

The skill:

1. Finds the next ready-to-start issue in the focused milestone
   (`/amos:focus <title>` sets the focus; `amos milestones` lists
   candidates).
2. Pulls the issue's body, exit criteria, and test plan from GitHub.
3. Loads any matching workflow file from `.claude/workflows/<label>.md`
   for each label on the issue (`ci`, `video-e2e`, `macos`, `polyglot`,
   `research`, etc.).
4. Announces the task and gates on your confirmation.
5. Branches, does the work, runs the test gate, opens a PR, reports.

See `~/.claude/skills/amos-next/SKILL.md` for the full protocol. Rules
specific to this repo (naming, RHI boundary, test philosophy, etc.) live
in [`CLAUDE.md`](CLAUDE.md) and are loaded into every session
automatically.

**To add a new specialty workflow**, drop a markdown file at
`.claude/workflows/<label-name>.md` and label the relevant issues with
that label. The skill will load the file automatically for those issues.

**To file a new issue**, follow the template at
[`docs/issue-template.md`](docs/issue-template.md) — Description /
Context / Exit criteria / Tests / Related. Test harnesses are their own
issues; cross-cutting concerns are labels, not milestones.

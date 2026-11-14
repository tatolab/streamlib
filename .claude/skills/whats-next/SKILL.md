---
name: whats-next
description: Orient — what is in flight and what to pick up next, read from Linear and the open pull requests. Use when the owner asks "where are we" or "what's next", or at the start of a session with no assigned issue.
---

# What's next

Read-only. It suggests; the owner decides.

1. **Linear**, team Tatolab: the projects with work under way (started, or holding any Todo, In
   Progress or In Review issue), each one's milestones and their progress, the Todo and In
   Progress issues, and the blocking relations between them. List open issues with no project or milestone too;
   they are reported, not picked. Check Triage for unanswered public
   reports.
2. **Pull requests**: `gh pr list -R tatolab/streamlib`, `-R tatolab/tatolab-app` and
   `-R tatolab/main-website`. Match each to its issue by the `TAT-n` in the branch or title; note
   any that is failing checks, waiting on review answers, or has no issue.
3. **Pick the next issue**: unblocked (every blocker Done), in the earliest unfinished milestone
   of a project already under way, and preferring one that unblocks others. Finishing an open PR
   comes before starting a new issue.
4. **Report briefly**: in flight, blocked and on what, the issue you would pick up and why, and
   the skill that fits — `implement` for a ready issue, `plan-project` for an agreed design with
   no issues, `file-issue` for untracked work, `triage` for a full inbox or issues that no longer
   match the code, `explore-idea` or `grilling` when the next step is still a question.

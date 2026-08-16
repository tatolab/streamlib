---
name: implement
description: Build one ticket end-to-end — the only path by which source code changes.
disable-model-invocation: true
---

# Implement

1. **Load**: the ticket — body plus every comment oldest→newest (a comment IS
   specification and is newer than the body; conflicts resolve toward the comment) —
   its change file if it has one, and the plan sections it cites.
2. **Plan gate**: list the architectural decisions this work needs. Any of them not
   DECIDED in the plan (or stated in the approved change) → post a `[NEEDS DECISION]`
   comment on the ticket with the options and your recommendation, and **stop**. Never
   decide inline; never infer a decision from existing code.
3. **Staleness check**: verify the ticket's load-bearing claims against the current
   tree; if drifted, correct the body via `gh issue edit` with strikethroughs preserved.
4. **Announce** the refined plan — goal, files, the seams tests will exercise, scope —
   and **wait for the owner's confirmation**. Hard stop.
5. On yes: `mkdir -p .claude/state` and write `.claude/state/active-ticket.json`
   (`{"issue": <N>, "branch": "<type>/<N>-<slug>"}`) — the mid-flight marker `/plan` reads.
   Branch `<type>/<N>-<slug>` off fresh `main`.
6. **Build test-first at the seams the ticket names.** Commit at logical checkpoints
   with conventional-commit prefixes; every commit builds.
7. Scope is the ticket. Side findings go in the PR body as notes — a new ticket only if
   the milestone does not land without it.
8. **Gates**: spawn `local-ci-runner`; fix what it reports.
9. **Review**: spawn `review-pr` and `rust-craftsmanship-reviewer`; fix-loop capped at
   3 rounds; a 4th disagreement escalates to the owner as DISCUSS.
10. **PR**: push, `gh pr create` — title `type(scope): summary` (the repo
    squash-merges; the title is the commit release-please parses); body `## Summary`,
    `## Closes` (one `Closes #N` per line), `## Exit criteria`, `## Test plan`,
    `## Notes for owner` (the batched findings).
11. `rm -f .claude/state/active-ticket.json`. Report: PR URL, tests run, notes awaiting
    the owner. Merge is always the owner's call.

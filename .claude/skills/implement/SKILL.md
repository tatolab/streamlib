---
name: implement
description: Build one ticket end-to-end — the only path by which source code changes.
disable-model-invocation: true
---

# Implement

1. **Load**: the ticket — body plus every comment oldest→newest (a comment IS
   specification and is newer than the body; conflicts resolve toward the comment) —
   its change file if it has one, and the plan sections it cites.
2. **Ultracode gate**: a ticket whose body opens with **Run with ultracode** is built
   only in a run that has ultracode on — a system-reminder confirms it when the owner's
   message carries the keyword (`/implement <N> ultracode`) or the session has it on.
   Without it, **stop** before any other step: one line saying the ticket runs with
   ultracode and the exact command to re-run it, nothing more. Build without it only when
   the owner says so in this run, in their own words. Under ultracode, every agent that
   edits files runs in its own worktree; the steps below hold unchanged.
3. **Plan gate**: list the architectural decisions this work needs. Any of them not
   DECIDED in the plan (or stated in the approved change) → post a `[NEEDS DECISION]`
   comment on the ticket with the options and your recommendation, and **stop**. Never
   decide inline; never infer a decision from existing code.
4. **Staleness check**: verify the ticket's load-bearing claims against the current
   tree; if drifted, correct the body via `gh issue edit` with strikethroughs preserved.
5. **Announce** the refined plan — goal, files, the seams tests will exercise, scope —
   and **wait for the owner's confirmation**. Hard stop.
6. On yes: `mkdir -p .claude/state` and write `.claude/state/active-ticket.json`
   (`{"issue": <N>, "branch": "<type>/<N>-<slug>"}`) — the mid-flight marker `/plan` reads.
   Branch `<type>/<N>-<slug>` off fresh `main`.
7. **Build test-first at the seams the ticket names.** Commit at logical checkpoints
   with conventional-commit prefixes; every commit builds.
8. Scope is the ticket. Side findings go in the PR body as notes — a new ticket only if
   the milestone does not land without it.
9. **Gates**: spawn `local-ci-runner`; fix what it reports.
10. **Review**: spawn `review-pr` and `rust-craftsmanship-reviewer`; fix-loop capped at
    3 rounds; a 4th disagreement escalates to the owner as DISCUSS.
11. **PR**: push, `gh pr create` — title `type(scope): summary` (the repo
    squash-merges; the title is the commit release-please parses); body `## Summary`,
    `## Closes` (one `Closes #N` per line), `## Exit criteria`, `## Test plan`,
    `## Notes for owner` (the batched findings).
12. `rm -f .claude/state/active-ticket.json`. Report: PR URL, tests run, notes awaiting
    the owner. Merge is always the owner's call.

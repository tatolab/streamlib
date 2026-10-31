---
name: implement
description: Build one ticket end-to-end — load it, check it against the plan and the tree, build test-first, review, and open the PR.
---

# Implement

The owner's usual shape for building a ticket. Each step is here because skipping it has
cost real rework before; use judgment about how much of each a given ticket needs.

1. **Load** the ticket — body plus every comment oldest→newest (a comment is
   specification and is newer than the body; conflicts resolve toward the comment) —
   its change file if it has one, and the plan sections it cites.
2. **Ultracode.** A ticket whose body opens with **Run with ultracode** was sized by the
   owner for multi-agent orchestration. If this run doesn't have ultracode on, mention
   that and the command to re-run it with (`/implement <N> ultracode`), then decide
   whether to go ahead single-session — a small ticket often can. Under ultracode, every
   agent that edits files runs in its own worktree.
3. **Check the decisions the work needs.** List the architectural choices the ticket
   depends on. Where one isn't DECIDED in the plan (or stated in the approved change),
   the owner prefers to make it: raise it as a `[NEEDS DECISION]` comment on the ticket
   with options and your recommendation, and keep going on the parts that don't depend
   on it — or, when it's small and easily reversed, build your recommendation and call
   it out plainly in the PR. Existing code is a weak guide to intent here; the plan is
   the stronger one.
4. **Staleness check.** Verify the ticket's load-bearing claims against the current tree;
   if they've drifted, correct the body via `gh issue edit`, rewritten to current intent.
5. **Share the plan** — goal, files, the seams tests will exercise, scope. If the owner
   is around, a quick confirmation catches misreadings cheaply; if not, proceed and let
   the PR carry it.
6. **Branch** `<type>/<N>-<slug>` off fresh `main` (or the branch the session was given).
7. **Build test-first at the seams the ticket names.** Commit at logical checkpoints with
   conventional-commit prefixes; every commit builds. Prune the comments that break
   `.claude/rules/comments.md` in every file you touch.
8. **Keep to the ticket's scope.** Side findings go in the PR body as notes — a new
   ticket only if the milestone doesn't land without it.
9. **Local checks**: `local-ci-runner` runs the fast local gates; fix what it reports.
   The full suites run in PR CI, so there's no need to repeat them locally first.
10. **Review**: `review-pr` and `rust-craftsmanship-reviewer` are the usual lenses. If
    you and a reviewer still disagree after a few rounds, bring the disagreement to the
    owner rather than looping.
11. **PR**: push, `gh pr create` — title `type(scope): summary` (the repo squash-merges;
    the title is the commit release-please parses); body `## Summary`, `## Closes` (one
    `Closes #N` per line), `## Exit criteria`, `## Test plan`, `## Notes for owner` (the
    batched findings).
12. If step 7 pruned comments, say so on the ticket: unnecessary comments were removed
    in favour of code discoverability. Report the PR URL, tests run, and notes awaiting
    the owner. The owner merges.

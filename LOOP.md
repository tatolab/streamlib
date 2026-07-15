# LOOP.md — loop registry

Loop engineering means the standing work of this repo runs as recurring reconciler passes, not
one-shot prompts. A `/loop` fires a `/goal` on an interval; each firing is one bounded turn that
moves the repo toward the goal and then yields. Durable loop state lives in `loops/`; work
artifacts (issues, comments, branches, draft PRs) live on GitHub.

Every loop reads [`loops/constraints.md`](loops/constraints.md) (binding) at the start of every
run and stays inside [`loops/budget.md`](loops/budget.md).

## milestone-loop
Invocation (paste-ready): /loop 30m /goal all issues in the focused milestone are
      merged, or waiting on the owner or on blockers — each turn: run one
      milestone-loop reconciler pass; obey loops/constraints.md; max 8 turns/firing
Sprint mode: the same /goal without the wrapper, when the owner is present.
Turn procedure: the milestone-loop skill (one reconciler pass per turn)
Knobs: heartbeat 30m · max_parallel 2 · attempts_per_ticket 3 · propose_only false
Kill switch: stop the /loop + /goal clear, `paused: true` in the state file, or the budget cap

State file: [`loops/milestone-loop-state.md`](loops/milestone-loop-state.md).

## Registering a new loop
A new loop is three things landing together in one operating-model PR: its own state file under
`loops/` (its `paused:` flag, its acting-on / waiting / watch / ignored sections), its own entry
in this registry (invocation, turn procedure, knobs, kill switch), and its own line in
`loops/budget.md` (daily caps, on-exceed procedure). A loop with no budget line does not run.

## Multi-loop priority
When more than one loop is registered, a health/watchdog loop outranks a work loop: the
milestone-loop yields to any future red-CI watcher — if that watcher is firing on a red build, the
milestone-loop goes propose-only until the build is green again, so no new work piles onto a broken
baseline.

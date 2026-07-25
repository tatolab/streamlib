---
name: work-on-milestone
description: Drive a whole milestone to done with the standing loop — resolve the milestone, set the goal, and register the recurring reconciler that runs passes until every issue is merged or parked. Use when the owner says "work on <milestone>", "start the loop on <milestone>", "close out <milestone>", or wants the loop running without hand-typing a /loop invocation. Also the place to stop or re-target a running milestone driver.
---

# work-on-milestone — the milestone driver

One command starts the standing loop on a milestone and keeps it running until the milestone is done. This replaces the old two-step ritual (set the focus, then hand-type a `/loop 30m /goal …` invocation) — a skill can register the recurring firing itself.

## What this is and is not

The driver schedules; it does not do the work. Each firing runs one `milestone-loop` pass, which launches `.claude/workflows/milestone-loop.js`. Routing, conflict analysis, and stage composition all live in that workflow.

**The schedule is session-scoped.** `CronCreate` jobs live only in the current Claude session — nothing is written to disk, the `durable` flag has no effect, and recurring jobs auto-expire after 7 days. Jobs also fire only while the REPL is idle, so a firing waits for the current turn to finish. There is no unattended heartbeat, and there cannot be one while the loop needs the local rig and worktrees. Say so plainly if the owner expects the loop to survive closing the session — re-run this skill to restart it.

## Procedure

### 1. Resolve the milestone

Match the owner's free-form text against open milestones (`gh api repos/:owner/:repo/milestones --paginate`). On an unambiguous match, take it. On ambiguity or no match, present the candidates (title + open-issue count) with `AskUserQuestion` — never guess which milestone to spend a day's budget on.

Report the open/total issue count once resolved.

### 2. Record the focus

Write the resolved title into `focused_milestone` in `.claude/loops/state/milestone-loop.json`, creating the file from `.claude/loops/state.example.json` if absent. This is what every reconciler pass and `loop-status` scope to, and it persists across firings until changed.

Set `paused` to `false` — starting a driver on a paused loop is otherwise a silent no-op.

### 3. Set the goal

Set the completion condition with `/goal`, phrased so it can actually be evaluated:

> All issues in `<milestone>` are merged, or waiting on the owner, or blocked. Each turn: run one milestone-loop reconciler pass. Obey `.claude/loops/constraints.md`.

The goal is the stop condition the driver checks each firing, and it survives in `~/.claude/projects/<project>/goals/`. Include anything milestone-specific the loop must not re-litigate — locked architecture decisions, a pre-flight check to run first — the same way a hand-written goal would.

### 4. Register the recurring firing

`CronCreate` with the heartbeat from `.claude/loops/README.md` (30m default → `"*/30 * * * *"`). Pick an off-minute rather than `0`/`30` when the exact cadence does not matter.

The prompt is enqueued standalone, so it must carry everything a fresh turn needs:

> Run one `milestone-loop` reconciler pass for milestone `<name>`. First re-read the milestone from GitHub so newly added, closed, or re-scoped issues are picked up. Then evaluate the goal: if every issue is merged, waiting on the owner, or blocked, the milestone is done — report it, delete this cron job by id `<id>`, and stop. Otherwise run the pass and yield.

Report the job id to the owner and tell them the 7-day expiry and session-scoping apply.

### 5. Each firing re-checks scope, then works

The firing is a check condition first and a work pass second:

- **Refresh** — re-read the milestone's issues. Issues added to or removed from the milestone since the last pass change the candidate set, so a pass that trusted a cached list would work on a stale scope.
- **Evaluate the goal** — if the completion condition holds, the driver has done its job: report, `CronDelete`, stop. A driver that keeps firing against a finished milestone burns budget for nothing.
- **Otherwise** run one `milestone-loop` pass and yield.

### 6. Stopping and re-targeting

Any of these stops the driver:

- The goal is met — the firing deletes its own job.
- The owner says stop → `CronList` to find the job, `CronDelete` it, and confirm.
- `"paused": true` in the state file — firings become near-free no-ops, but the job keeps firing. For a real stop, delete the job.
- The daily budget cap — the loop goes propose-only, then stops landing code. The job still fires; it just does not land work.

To re-target, delete the existing job first, then run this skill again for the new milestone. Two drivers on two milestones would both draw on one `max_parallel` budget and one rig, and neither would know about the other.

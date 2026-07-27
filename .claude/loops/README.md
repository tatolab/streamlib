# Loop registry

Loop engineering means the standing work of this repo runs as recurring reconciler passes, not
one-shot prompts. A `/loop` fires a `/goal` on an interval; each firing is one bounded turn that
moves the repo toward the goal and then yields. Work artifacts (issues, comments, branches, PRs)
live on GitHub; the loop's own memory lives in `state/`.

Every loop reads [`constraints.md`](constraints.md) (binding) at the start of every run and stays
inside [`budget.md`](budget.md).

## Layout

| Path | Tracked? | What |
|---|---|---|
| `README.md` (this file) | yes | The registry — invocation, knobs, kill switches, state formats |
| `constraints.md` | yes | Binding hard rules every run obeys |
| `budget.md` | yes | Daily caps, exemptions, model tiers |
| `state.example.json` | yes | Bootstrap defaults for a fresh clone |
| `state/<loop>.json` | **no** | The loop's live picture — rewritten every pass |
| `state/<loop>.run-log.jsonl` | **no** | Append-only turn history |

Policy is tracked so operating-model PRs stay reviewable. Runtime state is gitignored: it is
machine-local, rewritten every pass, and `branch-shield.sh` refuses commits on `main` — a tracked
state file could never be persisted by the loop that owns it.

Runtime state is also **absent from every worktree**, since `git worktree add` checks out tracked
files only. That is deliberate — state is orchestrator-owned, and a per-worktree copy could only
diverge. Anything running in a worktree that needs it must be handed an absolute path to the
primary checkout (`git rev-parse --git-common-dir`).

## milestone-loop

Invocation: **`/work-on-milestone <milestone>`**. That skill resolves the milestone, records it as
`focused_milestone`, sets the completion `/goal`, and registers the recurring firing itself — there
is no `/loop` line to hand-type and no separate focus step.

Turn procedure: the `milestone-loop` skill (one reconciler pass per firing), which launches
`.claude/workflows/milestone-loop.js`.

Knobs: heartbeat 30m · max_parallel 6 · attempts_per_ticket 3 · propose_only false · live_verify auto

`max_parallel` bounds tickets in flight per pass, not agents. Rig work is still serialized
independently — one GPU and one `/dev/videoN` at a time — so raising it widens throughput on the
many tickets that never touch hardware without loosening the device rule in `constraints.md`.

Kill switch: `CronDelete` the driver's job (`/work-on-milestone` reports its id; `CronList` finds
it), `"paused": true` in the state file, or the budget cap.

**Trigger durability — read this before relying on the heartbeat.** The schedule is
**session-scoped**, whether registered by `/loop` or by `CronCreate`. Cron jobs live only in the
current Claude session: nothing is written to disk, the `durable` flag has no effect, recurring jobs
auto-expire after 7 days, and a job fires only while the REPL is idle. Closing the session stops the
loop; re-run `/work-on-milestone` to restart it.

There is no unattended heartbeat, and there cannot be one while the loop needs the local rig and
worktrees — durable cloud scheduling runs somewhere without a GPU, a camera, or this checkout. What
`CronCreate` buys over a hand-typed `/loop` is that a skill can register it, and that a firing can
delete its own job once the goal is met.

`live_verify: auto` (default) runs `/verify-live` in-loop for rig-touching tickets when the preflight
capability probe reports the rig available (parks for the human otherwise); `off` disables in-loop
live verification and always parks. Safety-gated evals (actuators, motors, drone control) still ask
the owner first — the auto-run covers read-only observation only.

State file: `state/milestone-loop.json`.

## State file shape

`state.example.json` holds the bootstrap defaults. A live file adds entries under `tickets`, keyed
by issue number:

```json
{
  "stage": "verify",
  "attempt": 1,
  "fix_rounds": 0,
  "branch": "feat/1234-slug",
  "worktree_path": "/abs/path/.claude/worktrees/1234-slug",
  "workflow_run_id": "wf_...",
  "zones": ["rhi"],
  "shape": "implement",
  "rig_needs": ["gpu"],
  "lead": "gpu-vulkan-expert",
  "stages_planned": ["claim", "rederive", "implement", "gates", "self_review", "verify"],
  "comment_ids": [123456],
  "parked_question_comment_id": null,
  "note": ""
}
```

- `stage` — one of `claimed`, `design`, `research`, `implement`, `verify`, `fixing`, `reverify`,
  `pr-open`, `parked`, `escalated`, `done`. This is the loop's resumable position on a ticket.
- `stages_planned` — what the worktree workflow's composition rules selected for this ticket, so a
  later pass (or a human) can see why it ran the stages it did.
- `comment_ids` — every comment the loop posted on this ticket. Parked-question detection keys off
  this ledger, never on `author.login`: the loop's `gh` identity shares the owner's login, so a
  question is cleared only by a comment whose id is **not** in this list.
- `capabilities.owner_overrides` — capability keys the owner set by hand. The preflight probe
  refreshes every key **except** these.

The Acting on / Waiting on the owner / Watch / Ignored views are **derived** from `tickets[].stage`
by `loop-status`; nothing stores them.

## Run-log format

`state/<loop>.run-log.jsonl` is append-only JSON-lines — one object per turn, newest last, no
markdown wrapper (a fenced example inside the file would corrupt the daily meter).

```json
{"ts":"2026-07-24T18:03:11Z","loop":"milestone-loop","turn":7,"items":["#1234"],"actions":["opened PR"],"attempts":{"1234":1},"verdicts":{"1234":"pass"},"escalations":[],"est_tokens":48000,"outcome":"progressed"}
```

- `ts` — turn end time, ISO-8601 UTC. Its **UTC date** defines the budget "day".
- `items` — ticket refs the pass looked at. `actions` — terse phrases (`"opened PR"`, `"posted question"`).
- `attempts` — running attempt count per ticket (cap 3, then escalate). `verdicts` — per-ticket result.
- `escalations` — tickets handed to the owner. `est_tokens` — rough spend, for the daily budget.
- `outcome` — `progressed` | `blocked` | `budget-cap` | `idle`.

**Rotation.** On the first turn of a new UTC month, or once the file exceeds 200 lines, move the
events into `state/<loop>.run-log-YYYY-MM.jsonl` and start fresh.

## Registering a new loop

A new loop is four things landing together in one operating-model PR: its own state file under
`state/` (its `paused` flag and ticket map), its own entry in this registry (invocation, turn
procedure, knobs, kill switch), its own line in [`budget.md`](budget.md) (daily caps, on-exceed
procedure), and a driver skill that registers its firing and can stop it. A loop with no budget line
does not run, and a loop whose only start path is a hand-typed invocation will not get run.

## Multi-loop priority

When more than one loop is registered, a health/watchdog loop outranks a work loop: the
milestone-loop yields to any future red-CI watcher — if that watcher is firing on a red build, the
milestone-loop goes propose-only until the build is green again, so no new work piles onto a broken
baseline.

---
name: milestone-loop
description: Run one reconciler pass of the standing milestone-loop — the turn procedure the `/loop 30m /goal …` firing invokes. Use when a loop firing (or a sprint-mode `/goal` with Jonathan present) needs to move the focused milestone one bounded step toward "merged, or waiting on Jonathan or on blockers." Reconciles loop state against GitHub, routes and classifies ready issues, launches the matching workflow, parks anything needing the owner, records state, and yields at a coherent boundary.
---

# milestone-loop — the reconciler pass

One firing of the standing loop is one turn of this procedure. A turn moves the focused milestone forward and yields; the `/goal` evaluator decides whether to fire again. Do the eight steps in order. Never end mid-edit — a turn ends only at a coherent boundary (a checkpoint commit + a state note, or work cleanly handed off).

This skill operates the loop; it never edits the loop's own definition, rules, agents, or constraints (that is a separate operating-model PR — see `.claude/rules/flow.md`).

## 1. Load state and check the stops
Read, in order:
- `loops/constraints.md` — **binding**. These are hard rules for this turn, not guidance.
- `loops/budget.md` and today's spend in `loops/run-log.md`. If the day is **≥80% of budget spent**, run this turn in **propose_only** mode (plan, draft, post questions — land no new code). If a daily cap is already hit, append a `"outcome":"budget-cap"` run-log line and stop.
- `loops/milestone-loop-state.md`. If its frontmatter has `paused: true`, **stop** — that is a kill switch.

The knobs (`heartbeat`, `max_parallel`, `attempts_per_ticket`, `propose_only`) come from `LOOP.md`. Honor `max_parallel` and `attempts_per_ticket` (cap 3) as hard limits this turn.

## 2. Sync against GitHub
GitHub is the source of truth; the state file only caches the loop's picture. Reconcile via `gh`:
- **Prune ghosts** — drop state entries for PRs that merged or issues that closed since last turn.
- **Validate IDs** — every ticket ref in the state file still resolves to an open issue in the focused milestone.
- **Consume owner answers** — for each parked question, check whether it is now cleared. A parked question is cleared **only** by a comment that postdates the question **and** whose `author.login` equals the repo owner's login (fetch the owner login via `gh api`; never assume a non-owner comment is an answer, and never clear your own parked question).

## 3. Route and classify every candidate
A **candidate** is an issue in the focused milestone with: no open `blocked-by` edge, unclaimed, ungated (no open owner question), and not on `hold`. For each candidate, read the issue **fresh from GitHub** (labels are display output — never read a label as input) and classify:
- **Work shape** — one of: `design-first` (architecture brief before any build; **required** for engine-zone features per `.claude/rules/engine-doctrine.md`), `implement`, `bug-reproduce-first` (a failing test lands before the fix), or `research`.
- **Zones touched** — which code areas (RHI/vulkan, plugin-abi, python/deno, packages/registry, camera/v4l2, docs, …) — drives which domain expert the workflow spawns and the parallel-conflict analysis.
- **Rig needs** — GPU / camera / audio / none.
- **Risk** — how far the change reaches (engine-core vs leaf).

Stamp the classification back as **display labels** (`gh label create` any that don't exist yet). The labels are output for humans and for `loop-status`; nothing in this loop reads them back as control.

## 4. Parallel-conflict analysis
Before launching a batch, refuse any pair that would collide:
- **Same physical rig** — one in-flight ticket per GPU and per `/dev/videoN`. Serialize rig work; never launch two rig attempts at once.
- **Predicted same hot-file surface** — especially ABI vtable / layout files, shared schema files, or a single core module two tickets both rewrite. Serialize these; a merge race there is expensive.
- **Dependency edges** — never launch a ticket whose `blocked-by` is still open.

Reduce the candidate set to a launchable batch that fits `max_parallel` and violates no conflict.

## 5. ACT (skip in propose_only)
For each ticket in the launchable batch:
1. **Claim it** — post an owner-visible comment `▶ claimed — <one-sentence what & why>` and create the branch `issue/<n>-<slug>`.
2. **Launch the matching workflow** via the Workflow tool, in the **background**, one fresh worktree per attempt (`isolation: 'worktree'` on the build agents inside the script):
   - `design-first` → `.claude/workflows/draft-design.js`
   - `implement` / `bug-reproduce-first` → `.claude/workflows/implement-ticket.js` (the script's rederive phase picks the build lead by zone; bug shape lands the failing test first)
   - `research` → `.claude/workflows/run-research.js`
   - After an `implement` run returns a green self-review, launch `.claude/workflows/verify-change.js` for that branch — a `PASS` opens a **draft** PR (never a merge); a `FIX` bounces once within the attempt cap; a `DISCUSS` parks.

   Invoke as `{ scriptPath: ".claude/workflows/<script>.js", args: { issue: N, … } }`. Track attempt counts; at the third failed attempt on a ticket, **escalate** (step 6) rather than starting a fourth.

In **propose_only** mode, do not launch — instead post the plan-of-record for each candidate as an issue comment and move on.

## 6. PARK anything needing Jonathan
When a ticket needs a decision only the repo owner can make (an answered question, a merge, a milestone-scope call, or the attempt cap tripped):
- Add the `gate` display label.
- Post **one** question comment ending in an explicit question block (the owner answers by commenting).
- Send **one** Telegram ping — only for a *new* question this turn, never a re-ping of a still-open one.

Move the ticket to the state file's "Waiting on Jonathan" section with the question's age.

## 7. Write state and append the run-log
Rewrite the four sections of `loops/milestone-loop-state.md` (Acting on / Waiting on Jonathan / Watch / Ignored this pass) to the loop's current picture, each entry terse with its attempt count and stage. Append **exactly one** JSON-lines event to `loops/run-log.md` with the turn's `items`, `actions`, `attempts`, `verdicts`, `escalations`, `est_tokens`, and `outcome` (`progressed` / `blocked` / `budget-cap` / `idle`).

## 8. Yield at a coherent boundary
End the turn only when work is checkpointed or cleanly handed off — a background workflow launched, a draft PR opened, a question posted, plus the state write and run-log line from step 7. A headless final turn never ends with un-checkpointed background work. Do not decide whether to continue — the `/goal` evaluator fires the next turn.

---
name: milestone-loop
description: Run one reconciler pass of the standing milestone-loop — the turn procedure the `/loop 30m /goal …` firing invokes. Use when a loop firing (or a sprint-mode `/goal` with the owner present) needs to move the focused milestone one bounded step toward "merged, or waiting on the owner or on blockers." Reconciles loop state against GitHub, routes and classifies ready issues, launches the matching workflow, parks anything needing the owner, records state, and yields at a coherent boundary.
---

# milestone-loop — the reconciler pass

One firing of the standing loop is one turn of this procedure. A turn moves the focused milestone forward and yields; the `/goal` evaluator decides whether to fire again. Do the eight steps in order. Never end mid-edit — a turn ends only at a coherent boundary (a checkpoint commit + a state note, or work cleanly handed off).

This skill operates the loop; it never edits the loop's own definition, rules, agents, or constraints (that is a separate operating-model PR — see `.claude/rules/flow.md`).

## 1. Load state and check the stops
**FIRST, before any other read, `gh` call, or run-log line:** read ONLY the `loops/milestone-loop-state.md` frontmatter. If it has `paused: true`, **stop** — no other reads, no `gh` calls, no run-log line. That is a kill switch, and a paused firing must be a near-free no-op.

If not paused, read (in order):
- `loops/constraints.md` — **binding**. These are hard rules for this turn, not guidance.
- `loops/budget.md`. Measure **today's spend** with the mechanical meter — one Bash call, never a full read of the run-log:

  ```
  grep -c "^{\"ts\":\"$(date -u +%F)" loops/run-log.md
  ```

  This counts the run-log lines whose `ts` is **today's UTC date** (`date -u +%F`); the day is the UTC date of `ts`, so the count can never be conflated with the cumulative turn number, and it survives the day rollover. Read at most the last few events with `tail -n` for context — never the whole file. If the count is **≥80% of the daily cap**, run this turn in **propose_only** mode (plan, draft, post questions — land no new code). If the daily cap is already hit, append a `"outcome":"budget-cap"` run-log line and stop. Owner-directed corrections and red-CI fixes are **exempt** from propose-only/cap (see `loops/budget.md`).

The knobs (`heartbeat`, `max_parallel`, `attempts_per_ticket`, `propose_only`) come from `LOOP.md`. Honor `max_parallel` and `attempts_per_ticket` (cap 3) as hard limits this turn.

## 2. Sync against GitHub
GitHub is the source of truth; the state file only caches the loop's picture.

**Idle delta-probe first (cheap short-circuit).** Before the full sync, check whether anything actually changed since last turn. The state file caches, from the previous pass: the focused milestone's newest issue/comment `updatedAt`, each loop-owned open PR's head SHA + mergeable state, and the ids of in-flight background tasks. Re-read those with 1–2 `gh` calls plus a task-status check. If there is **no delta** AND (all code slots are full OR the loop is fully owner-gated), append the idle run-log line (`"outcome":"idle"`) and yield immediately — skip the rest of step 2 through step 6. Otherwise continue.

**Freshness.** `git fetch origin` and fast-forward the primary checkout's `main` as part of reconciliation — the local tree is part of the picture GitHub is the source of truth for, and a stale local `main` gives every new worktree a stale base (which is what lets an implement agent fabricate a no-diff "success").

Reconcile via `gh`:
- **Prune ghosts** — drop state entries for PRs that merged or issues that closed since last turn.
- **Validate IDs** — every ticket ref in the state file still resolves to an open issue in the focused milestone.
- **Consume owner answers.** The owner login is `owner_login` in the state frontmatter (operator-set — never derived from `gh repo view`, which returns the org). The loop's own `gh` identity shares that login, so `author.login` equality **cannot** distinguish an owner answer from the loop's own comment. Detection runs off a **comment-id ledger** instead: every time the loop posts a comment on a ticket (a claim, a parked question, any update — step 6), it records that comment's id (returned by `gh api`) in the ticket's state entry. A parked question is cleared by any comment on that ticket that postdates the question **and whose id the loop did NOT record as its own** — never by a comment the loop posted itself.
- **Conflicting PRs.** A loop-owned open PR that is CONFLICTING vs `main` → launch `.claude/workflows/fix-ticket.js` with `mode: 'rebase'` (a zone-matched lead rebases in-worktree, re-runs the gates, force-pushes). Rebases do **not** occupy `max_parallel` code slots — they add no new surface — so a mergeable PR never queues behind busy code slots, and the main context never hand-rebases.
  <!-- OWNER: N4 — merge-queue policy. The delegate-rebase mechanics (fix-ticket.js mode:'rebase') are safe either way; the queueing preference is yours: auto-rebase-on-conflict (throughput) vs stagger PR opening (less force-push noise on PRs you may be mid-review on). -->

Cache the delta-probe fields and the comment-id ledger back into the state file in step 7.

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
1. **Claim it** — post an owner-visible comment `▶ claimed — <one-sentence what & why>` (record its comment id in the ledger, step 2) and create the canonical branch `feat/<n>-<slug>`.
2. **Launch the matching workflow** via the Workflow tool, in the **background**, one fresh worktree per attempt (`isolation: 'worktree'` on the build agents inside the script):
   - `design-first` → `.claude/workflows/draft-design.js`
   - `implement` / `bug-reproduce-first` → `.claude/workflows/implement-ticket.js` (the script's rederive phase picks the build lead by zone; bug shape lands the failing test first). It returns the branch, worktree path, commits, and diff-stat.
   - `research` → `.claude/workflows/run-research.js`
   - After an `implement` run returns a green self-review, launch `.claude/workflows/verify-change.js` for that branch (pass `args.branch`) — a `PASS` opens a PR **ready for review** (a PASS means the branch is verified and ready to merge, so it is NEVER a draft — but never a merge either; merging is the owner's); a `FIX` routes to fix-ticket (below); a `DISCUSS` parks. Any PR the loop opens directly (e.g. after a verified fix) is likewise opened ready, never `--draft`.
   - **Fixing an existing branch never gets hand-rolled.** A verify `FIX`, an owner-directed refinement, or a conflicting-PR rebase → `.claude/workflows/fix-ticket.js` (`mode: 'fix'` applies the enumerated findings in-worktree; `mode: 'rebase'` rebases onto origin/main and force-pushes). **Never a hand-rolled `fix-<issue>.js` script, never a main-context edit.**

   Invoke as `{ scriptPath: ".claude/workflows/<script>.js", args: { issue: N, branch, … } }`.

   **Attempt accounting.** An *attempt* is a fresh implement launch — a new worktree, from Rederive. The attempt cap is **3** per ticket; at the third failed attempt, **escalate** (step 6) rather than starting a fourth. Verify-finding fix rounds via `fix-ticket.js` are bounded **separately** — ≤2 per verify verdict, then escalate — and owner-directed corrections and CI-red fixes never count toward the attempt cap.

In **propose_only** mode, do not launch — instead post the plan-of-record for each candidate as an issue comment and move on.

## 6. PARK anything needing the owner
When a ticket needs a decision only the repo owner can make (an answered question, a merge, a milestone-scope call, or the attempt cap tripped):

**If the owner is interactively reachable this turn** (the loop is running attended, not a headless cron firing), surface the decision with the **`AskUserQuestion` tool** — it's cleaner than a comment (structured, one-click). Frame 2–4 tight options, and **ALWAYS include an option that lets the owner launch research subagents to help decide** (e.g. "Research it first" → the loop spawns Opus research agents on the decision — approaches, trade-offs, a grounded recommendation — then re-presents). Still mirror the decision as a GitHub comment (below) so there's a durable record, and still record the ticket in "Waiting on the owner".

**When the owner is NOT reachable this turn** (autonomous firing), park it as a GitHub comment instead — `AskUserQuestion` needs a live user:
- Add the `gate` display label.
- Post **one** decision comment in this **strict shape**, so the owner sees a pending decision at a glance and can answer in one token without reading prose (the owner has reported not even realizing a decision was pending, buried under paragraphs):
  - **First line is a marked header** — `## ⛔ DECISION NEEDED — <the decision in one line>` (or `## ❓ OWNER QUESTION — <…>`). The marker + the one-line ask must be the very first thing in the comment.
  - **Immediately below, the isolated replyable ask** — the choice as tight options the owner answers in one token: `**(a)** … · **(b)** … — reply **(a)** or **(b)**.` Put nothing between the header and the options.
  - **Grounding / context goes BELOW a `---` divider** (keep it short) — never before the ask.
- **Ping the owner via the `PushNotification` tool** — one line: what's pending + the ticket #. It reaches the owner's phone through their linked channel (e.g. Telegram). Only for a *new* question this turn; never re-ping a still-open one. **Only the loop's main context can reach the owner** — workflow subagents have no notification tool, so a workflow that surfaces an owner-facing need RETURNS it to the main loop (a `DISCUSS` / `owner-question` verdict) and the main loop posts the comment + sends the ping. A subagent never tries to reach the owner directly.

Move the ticket to the state file's "Waiting on the owner" section with the question's age, and record the decision comment's id in the ledger (step 7).

## 7. Write state and append the run-log
Rewrite the four sections of `loops/milestone-loop-state.md` (Acting on / Waiting on the owner / Watch / Ignored this pass) to the loop's current picture, each entry terse with its attempt count and stage. Also persist the reconciler's caches so the next pass can run cheaply: each **Waiting on the owner** entry carries the comment-id ledger (the ids of the loop's own comments on that ticket, so the next pass can detect an owner answer per step 2), and the **delta-probe cache** (the milestone's newest issue/comment `updatedAt`, each loop-owned open PR's head SHA + mergeable state, and in-flight background-task ids). Append **exactly one** JSON-lines event to `loops/run-log.md` with the turn's `items`, `actions`, `attempts`, `verdicts`, `escalations`, `est_tokens`, and `outcome` (`progressed` / `blocked` / `budget-cap` / `idle`).

## 8. Yield at a coherent boundary
End the turn only when work is checkpointed or cleanly handed off — a background workflow launched, a draft PR opened, a question posted, plus the state write and run-log line from step 7. A headless final turn never ends with un-checkpointed background work. Do not decide whether to continue — the `/goal` evaluator fires the next turn.

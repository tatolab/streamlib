---
name: milestone-loop
description: Run one reconciler pass of the standing milestone-loop — the turn procedure each `/work-on-milestone` firing invokes. Use when a loop firing (or the owner directly) needs to move the focused milestone one bounded step toward "merged, or waiting on the owner or on blockers." Checks the kill switch, probes rig capability, launches the milestone-loop workflow, and surfaces anything needing the owner. To START the loop on a milestone rather than run a single pass, use work-on-milestone.
---

# milestone-loop — the reconciler pass

One firing of the standing loop is one run of `.claude/workflows/milestone-loop.js`. That workflow does the reconciling, classifying, planning, dispatching, and recording. This skill exists only for the four things a workflow script structurally cannot do:

- **Filesystem access** — workflow scripts have none, so the kill-switch check happens here and paths arrive as arguments.
- **The date** — `Date.now()` and argless `new Date()` throw inside a workflow (they would break resume), so `today` is computed here.
- **The sandbox bypass** — the rig capability probe decides whether live verification may run at all.
- **Reaching the owner** — `PushNotification` and `AskUserQuestion` are main-context only. A workflow returns owner-facing items; it never posts them.

Everything with branching logic in it — routing, conflict analysis, stage selection, fix-round bounding — lives in the workflow as real control flow. Do not re-implement any of it here.

This skill operates the loop; it never edits the loop's own definition, rules, agents, or constraints (that is a separate operating-model PR — see `.claude/rules/flow.md`).

## 1. Check the kill switch

**FIRST, before any other read, `gh` call, or run-log line:** read ONLY the `paused` field of `.claude/loops/state/milestone-loop.json`. If it is `true`, **stop** — no other reads, no `gh` calls, no run-log line. A paused firing must be a near-free no-op, which is why this check stays here rather than costing a workflow launch to discover.

If the file does not exist, create it from `.claude/loops/state.example.json`.

Read `.claude/loops/constraints.md` — **binding**, hard rules for this turn.

## 2. Preflight capability probe

Read `capabilities` from the state file. If `probed_utcdate` equals today's UTC date (`date -u +%F`), trust the cache and skip the probe.

Otherwise re-probe with READ-ONLY, sandbox-safe device probes: `ls /dev/video*`, `v4l2-ctl --device <n> --info` (query verbs only), `$DISPLAY` + `xdpyinfo`, and `/dev/dri/*` for a GPU — plus a ONE-TIME cheap bypass smoke (capture a single vivid frame via the Bash `dangerouslyDisableSandbox` bypass) to confirm the bypass is actually permitted.

Set `live_verify` to `available` only when the rig is present AND the bypass smoke succeeded; `unavailable` otherwise. Refresh every capability key **except** those named in `capabilities.owner_overrides` — that list is how a hand-set value is distinguished from a probed one. Stamp `probed_utcdate`.

The `live_verify: off` knob in `.claude/loops/README.md` short-circuits all of this: treat live verification as unavailable and let every rig-touching ticket park.

## 3. Launch the workflow

Compute `today` (`date -u +%F`) and the absolute repo root (`git rev-parse --path-format=absolute --git-common-dir`, then its parent — never `--show-toplevel`, which returns the worktree when called from inside one).

**First, reap a pass that died.** A workflow that dies mid-flight cannot report itself: no completion notification fires, no run-log line is written, and the firing is indistinguishable from one that never happened. Read `pass_in_flight` from the state file. If it is set, the previous pass never reached its Record phase — append one run-log line with `"outcome":"pass-died"` naming the launch timestamp it carried, clear any ticket entry that records a stage but has no branch, worktree, or PR (so the ticket is retried rather than stranded), and tell the owner. Then set `pass_in_flight` to this launch's UTC timestamp before launching. The workflow's Record phase clears it.

Launch in the **background**:

```
Workflow({ scriptPath: ".claude/workflows/milestone-loop.js",
           args: { today, repo_root, max_parallel, attempts_per_ticket, live_verify, attended } })
```

Knobs (`heartbeat`, `max_parallel`, `attempts_per_ticket`, `propose_only`, `live_verify`) come from `.claude/loops/README.md`.

Run this in the **primary checkout, never a worktree**. A session started in a worktree has no `.claude/loops/state/` and no `settings.local.json` — `git worktree add` checks out tracked files only. The loop would start with no state, write a fresh one, and split-brain against the primary. Git permits a worktree inside a worktree, so this fails silently rather than erroring.

## 4. Surface what needs the owner

The workflow returns `owner_items` — parked questions, `DISCUSS` verdicts, escalations, and cap notices. It cannot reach the owner itself. For each item:

**If the owner is interactively reachable** (running attended, not a headless firing), use **`AskUserQuestion`** — structured and one-click. Frame 2–4 tight options, and **ALWAYS include an option that lets the owner launch research subagents to help decide** (e.g. "Research it first" → spawn Opus research agents on the decision, then re-present). Mirror the decision as a GitHub comment so there is a durable record.

**When the owner is NOT reachable** (autonomous firing), park it as a GitHub comment — `AskUserQuestion` needs a live user:

- Add the `gate` display label.
- Post **one** decision comment in this **strict shape**, so a pending decision is visible at a glance without reading prose (the owner has reported not realizing a decision was pending, buried under paragraphs):
  - **First line is a marked header** — `## ⛔ DECISION NEEDED — <the decision in one line>` (or `## ❓ OWNER QUESTION — <…>`).
  - **Immediately below, the isolated replyable ask** — `**(a)** … · **(b)** … — reply **(a)** or **(b)**.` Nothing between the header and the options.
  - **Grounding goes BELOW a `---` divider**, kept short — never before the ask.
- **Ping via `PushNotification`** — one line: what's pending plus the ticket #. Only for a *new* question this turn; never re-ping a still-open one.

Record every comment id the loop posts into that ticket's `comment_ids` ledger in the state file. Parked-question detection keys off that ledger, never `author.login` — the loop's `gh` identity shares the owner's login, so a question is cleared only by a comment whose id the loop did not record.

Labels are display output. Nothing reads a label as control.

## 5. Yield

The workflow writes the state file and appends exactly one run-log event before it returns; a turn is checkpointed by that write, not by a commit (the state directory is gitignored — `branch-shield.sh` refuses commits on `main`, which is why it must be).

End the turn once the workflow is launched or its owner-items are posted. Do not decide whether to continue — the `/goal` evaluator fires the next turn.

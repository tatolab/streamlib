# Loop constraints (BINDING)

Read this at the start of every loop run. These are hard rules, not guidance.

## Isolation
- Every code-changing attempt runs in its **own git worktree** — one worktree per attempt. Sweep
  (remove) the worktree on reject or escalate; never leave orphan worktrees behind.
- One in-flight ticket per physical rig resource (one GPU, one `/dev/videoN` at a time). The loop
  serializes rig work; it never launches two attempts that would contend for the same device.

## Attempts and escalation
- Attempt cap is **3 per ticket**. After the third failed attempt, escalate: post the attempt
  history (what was tried, what failed) as a question on the issue and move the ticket to
  "Waiting on the owner." Do not start a fourth attempt.

## Turn boundaries
- A turn may only end at a **coherent boundary**: a checkpoint commit plus a state-file note, or
  work cleanly handed off (a background task, an opened PR, or a posted question). Never end mid-edit.
- Headless runs (no human present) never end the **final** turn with un-checkpointed background
  work — either the work is committed / handed off, or the turn keeps going until it is.

## The operating model is off-limits to the run that uses it
- A run never modifies loop definitions, agents, skills, rules, or these constraints in the same
  run that is using them. Operating-model changes are their own PR (see `.claude/rules/flow.md`).

## Rig safety
- Never run rig-consuming commands (camera/display/GPU-runtime). The `rig-brake` hook enforces
  this; when you hit it, park the work for `/verify-live` and post the exact command block for
  the owner's terminal.

## Parked questions
- "The owner" throughout these files is the repository owner's GitHub login — the human who merges
  PRs and answers parked questions. A question parked on an issue is cleared **only** by a comment
  from the owner's login that postdates the question. A loop never clears its own parked question,
  and never treats a non-owner comment as an answer.

## Budget
- If the day's budget is **≥80% spent**, the loop goes **propose-only** for the rest of the day —
  it may plan, draft, and post, but lands no new code. See `loops/budget.md`.

## Model tiers
- Implementation and reasoning spawns run `opus`; mechanical / prescribed-steps spawns run
  `sonnet`. Nothing pins `fable`.

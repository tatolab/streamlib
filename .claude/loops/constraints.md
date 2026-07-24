# Loop constraints (BINDING)

Read this at the start of every loop run. These are hard rules, not guidance.

## Isolation
- Every code-changing attempt runs in its **own git worktree** — one worktree per attempt. Sweep
  (remove) the worktree on reject or escalate; never leave orphan worktrees behind.
- One in-flight ticket per physical rig resource (one GPU, one `/dev/videoN` at a time). The loop
  serializes rig work; it never launches two attempts that would contend for the same device.

- **Existing branches are touched by the workflow, not the main context.** Verify-finding fixes
  run as the fix round inside `.claude/workflows/worktree-work.js`; a conflicting-PR rebase runs
  the same workflow with `mode: 'rebase'`. Never a hand-rolled `fix-<issue>.js` script and never a
  main-context edit. This is a **strict ban with no trivial-fix exemption** (N5, decided): the
  worktree already exists and the workflow takes its path, so routing a two-line change costs
  almost nothing, and every carve-out for "trivial" edits has to be adjudicated by the very run
  that wants the exemption.

## Attempts and escalation
- An **attempt** is a fresh implement launch — a new worktree, starting from Rederive. The attempt
  cap is **3 per ticket**. After the third failed attempt, escalate: post the attempt history (what
  was tried, what failed) as a question on the issue and move the ticket to "Waiting on the owner."
  Do not start a fourth attempt.
- **Verify-finding fix rounds are bounded separately.** Applying verify findings to an existing
  branch is not a fresh attempt; it is bounded at **≤ 2 fix rounds per verify verdict**, then
  escalate. That bound is enforced by the `while` loop in `worktree-work.js`, not by recall.
  Owner-directed corrections and CI-red fixes never count toward either cap.

## Turn boundaries
- A turn may only end at a **coherent boundary**: a state write plus its run-log line, or work
  cleanly handed off (a background task, an opened PR, or a posted question). Never end mid-edit.
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
  PRs and answers parked questions. It is recorded as `owner_login` in
  `.claude/loops/state/milestone-loop.json` (operator-set — never derived from `gh repo view`,
  which returns the org).
- The loop's own `gh` identity shares the owner login, so `author.login` equality **cannot** tell an
  owner answer from the loop's own comment. Detection runs off a **comment-id ledger** instead: the
  loop records the id of every comment it posts on a ticket. A question parked on an issue is cleared
  by any comment on that ticket that postdates the question **and whose id the loop did NOT record as
  its own** — never by a comment the loop posted itself.

## Budget
- If the day's budget is **≥80% spent**, the loop goes **propose-only** for the rest of the day —
  it may plan, draft, and post, but lands no new code. See `.claude/loops/budget.md`.

## State
- The loop's runtime state (`.claude/loops/state/`) is **gitignored and never committed**. It is
  machine-local and rewritten every pass; `branch-shield.sh` refuses commits on `main`, so a
  tracked state file could never be persisted by the loop that owns it.
- Runtime state is **absent from every worktree** (`git worktree add` checks out tracked files
  only). Resolve the primary checkout with `git rev-parse --git-common-dir`, never
  `--show-toplevel`, which returns the worktree when called from inside one.

## Model tiers
- Implementation and reasoning spawns run `opus`; mechanical / prescribed-steps spawns run
  `sonnet`. Nothing pins `fable`.

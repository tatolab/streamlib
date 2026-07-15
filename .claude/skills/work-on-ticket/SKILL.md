---
name: work-on-ticket
description: Take one ticket end-to-end in the current session with Jonathan present — the interactive counterpart to the standing loop. Use when he says "work on #N", "let's do this ticket", "pick up X and I'll answer questions as you go", or wants to drive a single issue to a draft PR now. Same router, same workflow scripts, same verifier as the loop — but questions come inline instead of parking.
---

# work-on-ticket — the interactive one

This is the debug interface for the loop: the same machinery a `milestone-loop` turn runs, driven on a single ticket with the owner in the room. The one difference is where questions go — **inline, not parked**.

## When to use vs the loop
Use `work-on-ticket` when Jonathan is present and wants to drive a specific issue now and answer questions in real time. Use `milestone-loop` for the standing autonomous pass. The routing, the workflow scripts, and the verifier are identical — so a ticket taken here lands the same shape of work (branch, worktree-per-attempt, draft PR, verifier gate) that the loop would have produced.

## Procedure

### 1. Read the ticket fresh
Pull the issue body from GitHub and read it against current code — the body is the goal, not a spec, and its file paths / claims may have gone stale. Confirm what's still true before locking a plan.

### 2. Classify (same router as the loop)
Classify the work exactly as `milestone-loop` step 3 does: **work shape** (`design-first` — required for engine-zone features / `implement` / `bug-reproduce-first` / `research`), **zones touched**, **rig needs**, **risk**. Pick the build lead by zone (abi → plugin-abi-expert, python/deno → polyglot-ipc-expert, packages/registry → package-registry-expert, vulkan/rhi/video → gpu-vulkan-expert, camera/v4l2 → linux-media-expert, else generic).

### 3. Announce the plan-of-record
State the fresh plan — what you'll change, the test shape, the scenario for live verification if any — and where it supersedes the issue body. This is the moment to catch a wrong assumption before work starts.

### 4. Run the matching workflow
Launch the same script the loop would:
- `design-first` → `.claude/workflows/draft-design.js`
- `implement` / `bug-reproduce-first` → `.claude/workflows/implement-ticket.js`
- `research` → `.claude/workflows/run-research.js`

Work in a fresh worktree per attempt; checkpoint at logical boundaries (commits are contractual, not optional). Honor the attempt cap of 3.

### 5. Questions go inline — this is the whole difference
Whenever the work hits a decision only Jonathan can make — an ambiguous requirement, a scope call, a design fork, a milestone question — **ask him directly with `AskUserQuestion`**, right then. Do NOT park a `gate` label and a question comment the way the loop does; he's here, so get the answer and keep going. Reserve GitHub-comment parking for the autonomous loop.

Everything else that isn't owner-only — a fact you can derive, a check you can run — you resolve yourself; don't turn a derivable question into an interruption.

### 6. Verify and open a draft PR
After a green self-review, run `.claude/workflows/verify-change.js` on the branch. A `PASS` opens a **draft** PR (never a merge — merging stays Jonathan's call). A `FIX` bounces once within the attempt cap. A `DISCUSS` surfaces the disagreement to Jonathan inline rather than parking it.

If the change needs live rig verification (GPU / camera / display), you cannot run it from a sandboxed session — hand off via `/verify-live`: emit the exact command block for his terminal, and audit the output he reports back.

### 7. Report
Close with what landed: the branch, the draft PR, the verifier verdict, any follow-ups surfaced (don't auto-file them — surface and let him decide), and whatever still needs a live run.

---
name: work-on-ticket
description: Take one ticket end-to-end in the current session with the owner present — the interactive counterpart to the standing loop. Use when they say "work on issue N", "let's do this ticket", "pick up X and I'll answer questions as you go", or want to drive a single issue to a PR now. Same workflow, same composed stages, same verifier as the loop — but questions come inline instead of parking.
---

# work-on-ticket — the interactive one

The debug interface for the loop: the same machinery a `milestone-loop` turn runs, driven on a single ticket with the owner in the room. The one difference is where questions go — **inline, not parked**.

## When to use vs the loop

Use `work-on-ticket` when the owner is present and wants to drive a specific issue now and answer questions in real time. Use `milestone-loop` for the standing pass. The workflow, the stage composition, and the verifier are identical — so a ticket taken here lands the same shape of work the loop would have produced.

## Procedure

### 1. Classify the ticket

Read the issue fresh from GitHub against current code — the body is the goal, not a spec, and its file paths and claims may have gone stale. Determine the same four things the loop's classifier does:

- **shape** — `design-first` (required for engine-zone features per `.claude/rules/engine-doctrine.md`) / `implement` / `bug-reproduce-first` / `research`
- **zones** — the code areas touched
- **rig_needs** — gpu / camera / display / audio, or none
- **lead** — the domain expert for those zones, or null for generic

Labels are display output; never read one as control input.

### 2. Announce the plan-of-record

State the fresh plan — what you'll change, the test shape, the scenario for live verification if any — and where it supersedes the issue body. This is the moment to catch a wrong assumption before work starts.

### 3. Launch the workflow

Compute `today` (`date -u +%F`) and the absolute repo root, then launch the same script the loop dispatches to:

```
Workflow({ scriptPath: ".claude/workflows/worktree-work.js",
           args: { issue, slug, zones, shape, lead, rig_needs, live_verify, repo_root, today } })
```

`design-first` routes to `draft-design.js` and `research` to `run-research.js` instead.

The workflow composes its own stage list from `shape` and `zones`, creates the worktree and branch, gates, verifies, applies bounded fix rounds, and opens the PR ready for review on a `PASS`. Do not hand-roll any of those steps here, and never edit the branch from this context — the constraint against main-context edits is not relaxed just because the owner is watching.

Honor the attempt cap of 3.

### 4. Questions go inline — this is the whole difference

The workflow returns `owner_items` for anything only the owner can decide. Surface each one with **`AskUserQuestion`**, right then. Do NOT park a `gate` label and a question comment the way the loop does — they're here, so get the answer and keep going.

Everything that isn't owner-only — a fact you can derive, a check you can run — you resolve yourself; don't turn a derivable question into an interruption.

### 5. Live verification

If the ticket's `rig_needs` are non-empty, the workflow runs the Evidence stage itself when `live_verify` is `available`. When it isn't, hand off via `/verify-live`: emit the exact command block for the owner's terminal and audit the output they report back.

### 6. Report

Close with what landed: the branch, the PR, the verifier verdict, the stages the workflow actually composed and ran, any follow-ups surfaced (don't auto-file them — surface and let the owner decide), and whatever still needs a live run.

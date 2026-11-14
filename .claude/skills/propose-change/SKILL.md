---
name: propose-change
description: Write a typed delta change proposal against the plan — how new behaviour and changed contracts usually get architected.
---

# Propose change

A change proposal is a delta against the plan: what is ADDED, MODIFIED and REMOVED.
Architecture is agreed on paper before issues and code commit to it, and `/ship-change`
later proves the removals happened.

It builds on DECIDED plan sections. If a section the change touches is OPEN, run
`/align` first, or carry the question in the proposal as `[NEEDS DECISION]`.

1. **Size it first**, and say which tier this is:
   - Bug fix, refactor or test work: no change file. A Linear issue and `/implement`.
   - New behaviour or a changed contract: this skill.
   - Anything touching the RHI, the IPC wire format, the processor model or the Python
     API's public contract: this skill plus an ADR, because those are expensive to
     reverse.
   - Too fuzzy to state as a delta of 350 lines with few unknowns: `/explore-idea` first.
2. **Recon, read-only.** Map the current state before writing; the domain experts are
   good for this. A proposal written without reading the tree names APIs that don't
   exist.
3. **Write `docs/plan/changes/<name>.md`** (grammar: `docs/plan/changes/README.md`):
   - Sections typed `ADDED:` / `MODIFIED:` / `REMOVED:` against ARCHITECTURE.md. Each
     `- REMOVED: <pattern>` bullet is a pattern `/ship-change` checks is gone.
   - At most 350 lines (the owner's number). Keep the file:line citations and worked API
     spelling: they are the delta's evidence.
   - Close a factual gap by reading the repo. Write an architectural choice the plan
     doesn't state as `[NEEDS DECISION]`, with the options and your recommendation.
4. **Flip the affected plan sections** to `IN-FLIGHT (→ <name>)`.
5. **Open the PR in the same turn** and present it, open `[NEEDS DECISION]` blocks first.
   Once the owner approves, `/plan-project` turns the change into a Linear project.

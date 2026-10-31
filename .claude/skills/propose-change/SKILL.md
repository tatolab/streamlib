---
name: propose-change
description: Write a typed delta change proposal against the plan — how new behaviour and changed contracts usually get architected.
---

# Propose change

A change proposal is a delta against the plan: what is ADDED, MODIFIED and REMOVED. It
exists so architecture gets agreed on paper before tickets and code commit to it, and so
`/ship-change` can later prove the removals happened. This is how the owner likes new
behaviour shaped; adapt it when the situation calls for something lighter.

It works best on top of DECIDED plan sections. If a section the change touches is still
OPEN, an `/align` pass first usually saves rework — or carry the question inside the
proposal as `[NEEDS DECISION]`.

1. **Size it first** — say which tier this is and why:
   - Bug fix / refactor / test work → usually no change artifact; a bug ticket and
     `/implement` is enough.
   - New behaviour or a changed contract → this skill.
   - Anything touching the RHI, the IPC wire format, the processor model, or the Python
     API's public contract → this skill plus an ADR, because those are the expensive
     things to reverse.
   - Too fuzzy or broad to state as a ≤350-line delta with few unknowns → `/explore-idea`
     tends to pay off first.
2. **Recon, read-only.** Map current state before writing — domain experts are good for
   this. Proposals written without reading the tree tend to reference APIs that don't
   exist.
3. **Write `docs/plan/changes/<name>.md`:**
   - Sections typed `ADDED:` / `MODIFIED:` / `REMOVED:` against ARCHITECTURE.md. Each
     `- REMOVED: <pattern>` bullet is a grep pattern `/ship-change` will check is gone.
   - **≤350 lines** (owner's number). Keep the file:line citations and worked API
     spelling — they are the delta's evidence, not padding.
   - A factual gap you can close by reading the repo: close it. An architectural choice
     the plan doesn't state: write `[NEEDS DECISION]` with the options and your
     recommendation. The owner prefers to make those calls, so surface them clearly
     rather than settling them silently in the text.
4. **Flip the affected plan sections** to `IN-FLIGHT (→ <name>)`.
5. **Branch, commit and open a PR** in the same turn — a proposal left uncommitted is
   lost to the next session.
6. **Present it.** The owner usually approves a proposal before tickets are derived from
   it, and open `[NEEDS DECISION]` markers are what they'll want to settle first. Deriving
   tickets ahead of that is your call — it just risks rework if the shape moves.

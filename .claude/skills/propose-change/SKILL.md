---
name: propose-change
description: Write a typed delta change proposal against the plan — the only way new work gets architected.
disable-model-invocation: true
---

# Propose change

Precondition: every plan section this change touches is DECIDED. An OPEN section stops
this skill — route to `/align` first.

1. **Scale gate** — state the tier and why, before anything else:
   - Bug fix / refactor / test work → **no change artifact**. A bug ticket and
     `/implement`. Stop here.
   - New behavior or a changed contract → this skill.
   - Anything touching the RHI, the IPC wire format, the processor model, or the Python
     API's public contract → this skill **plus** an ADR.
   - Too fuzzy or too broad to state as a ≤350-line delta with few unknowns →
     `/explore-idea` first; come back when the shape is crisp.
2. **Recon, read-only.** Spawn the relevant domain experts to map current state before
   writing a word — proposals invented without reading the tree reference APIs that
   don't exist.
3. `mkdir -p .claude/state && touch .claude/state/plan-session`, then write
   `docs/plan/changes/<name>.md`:
   - Sections typed `ADDED:` / `MODIFIED:` / `REMOVED:` against ARCHITECTURE.md.
     Every `- REMOVED: <pattern>` bullet is a grep pattern the ship gate will verify is
     gone.
   - **≤350 lines.** Never reach it by dropping file:line citations or the worked API
     spelling — those are the delta's evidence, not its padding.
   - A factual gap you can resolve by reading the repo: resolve it. An architectural
     choice the plan doesn't state: write `[NEEDS DECISION]` with the options and your
     recommendation. **You may never resolve one yourself.**
4. Still inside the marker window: flip the affected plan sections to
   `IN-FLIGHT (→ <name>)`. Then `rm -f .claude/state/plan-session`.
5. **Stop.** Present the proposal. The owner approves in their own words before
   `/derive-tickets` may run — and a proposal with an unresolved `[NEEDS DECISION]`
   cannot be approved yet.

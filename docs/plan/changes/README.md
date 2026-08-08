# Changes

In-flight delta proposals against `../ARCHITECTURE.md` — written by `/propose-change`,
ticketed by `/derive-tickets`, folded in and archived by `/ship-change`.

Format: sections typed `ADDED:` / `MODIFIED:` / `REMOVED:`. Every `- REMOVED: <pattern>`
bullet is a grep pattern `.claude/scripts/ship-change-removed-gate.sh` verifies is gone
from the tree before the change may archive. Limits: ≤350 lines per change. Ticket count
is guidance, not a cap (owner decision 2026-08-02): derive as few tracer-bullet tickets
as the change honestly needs — the cap existed to stop ticket-spam, and a change that
needs more vertical slices splits only when it is genuinely two changes.

Unresolvable choices are written as `[NEEDS DECISION]` blocks (options + recommendation)
and only the owner resolves them. Archived changes live in `archive/` as
`<YYYY-MM-DD>-<name>.md`.

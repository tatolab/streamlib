# Changes

In-flight delta proposals against `../ARCHITECTURE.md` — written by `/propose-change`,
ticketed by `/derive-tickets`, folded in and archived by `/ship-change`.

Format: sections typed `ADDED:` / `MODIFIED:` / `REMOVED:`. Every `- REMOVED: <pattern>`
bullet is a grep pattern `.claude/scripts/ship-change-removed-gate.sh` verifies is gone
from the tree before the change may archive. Limits: ≤200 lines per change, ≤5 derived
tickets — exceeding either means the change splits.

Unresolvable choices are written as `[NEEDS DECISION]` blocks (options + recommendation)
and only the owner resolves them. Archived changes live in `archive/` as
`<YYYY-MM-DD>-<name>.md`.

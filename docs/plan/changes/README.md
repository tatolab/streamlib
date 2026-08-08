# Changes

In-flight delta proposals against `../ARCHITECTURE.md` — written by `/propose-change`,
ticketed by `/derive-tickets`, folded in and archived by `/ship-change`.

Format: sections typed `ADDED:` / `MODIFIED:` / `REMOVED:`. Every `- REMOVED: <pattern>`
bullet is a pattern `.claude/scripts/ship-change-removed-gate.sh` verifies is gone from
the tree before the change may archive — searched in file contents *and* checked as a
path, since `git grep` never matches a filename.

**Bullet grammar — one artifact per bullet, plain text, on the bullet's first line.**
Continuation lines are prose the gate does not search, and are where notes, file:line
citations and `[NEEDS DECISION]` blocks go. The gate rejects a bullet that carries
backticks (they appear only in markdown and rustdoc prose, never in a definition), joins
items with a space-slash-space " / ", uses `{a,b}` brace expansion, or trails a
" (parenthetical)" — each is searched verbatim, matches nothing, and passes green
forever; the surrounding spaces in those two are part of what the gate looks for, which
is why they are quoted here rather than set in code spans. Write the artifact the way
it is spelled in source: a crate name, a symbol, or a repo-root-relative path. A bullet
you have not watched fail is not a bullet you have proved.

Limits: ≤350 lines per change. Ticket count
is guidance, not a cap (owner decision 2026-08-02): derive as few tracer-bullet tickets
as the change honestly needs — the cap existed to stop ticket-spam, and a change that
needs more vertical slices splits only when it is genuinely two changes.

Unresolvable choices are written as `[NEEDS DECISION]` blocks (options + recommendation)
and only the owner resolves them. Archived changes live in `archive/` as
`<YYYY-MM-DD>-<name>.md`.

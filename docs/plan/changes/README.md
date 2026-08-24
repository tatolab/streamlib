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

## Retired without shipping

`archive/` holds deltas that shipped and were folded into the plan. A proposal the plan
overtook before it was built is deleted instead, with its line recorded here — left in
place it reads as pending work forever, and `/ship-change` has nothing to fold.

- **`mvp-app-experience.md`** — superseded 2026-08-02 by `importable-python-library`,
  retired 2026-08-24 (owner). Its package-source, discovery-scan, string-id and
  subprocess-execution sections died with the SDK-shape pivot. The clauses that survived
  it — the `app.py`/`setup(rt)` convention, `streamlib new`, class-form `rt.add` — are
  §Product plan text, SHIPPED #1683, #1684, #1711.
- **`pypi-packaging.md`** — superseded 2026-08-02 by `importable-python-library`,
  retired 2026-08-24 (owner). It packaged a standalone binary; the shipped artifact is
  the PyO3 wheel served from a repo-hosted PEP 503 simple index, which is §Distribution
  plan text, SHIPPED #1691, #1692, #1694, #1711.

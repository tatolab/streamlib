# Decisions

A decision record captures **chosen-shape rationale the code cannot show**: why this design over
the ones we rejected. Architecture docs say what the system *is* now; a decision says *why it is
that way* so a future reader doesn't re-litigate a settled call or repeat a dead end.

Each record is one file with this shape:

- **Trigger** — when a reader should reach for this (the situation that makes it relevant).
- **Decision** — the shape we chose, stated plainly.
- **Rejected alternatives** — each with a one-line why-not.
- **Consequences** — what this commits us to, and the costs we accepted.

Conventions:
- One topic per file, `kebab-case-topic.md`.
- Living documents (per CLAUDE.md's markdown rules): validate and update freely; supersede with a
  dated `> ~~old~~ — Superseded YYYY-MM-DD by <evidence>.` strikethrough rather than a silent
  overwrite.
- No tracker references (issue / PR / milestone numbers) — the record stands on the reasoning.

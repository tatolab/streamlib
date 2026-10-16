# Comments

The code documents itself. Before writing a comment, make the code say it: a more explicit name, a
smaller function, a type that carries the constraint. A comment is written for one of three purposes
only:

- **Developer experience.** A public type or function tells an external developer or agent how to
  use it.
- **The code as it is now.** It describes current behaviour, never how the code got here.
- **What the code cannot show.** An ABI or wire contract, a spec convention, a driver quirk, or an
  external constraint.

Outside those, an agent writes no comment. A comment never holds prose essays, decision records,
issue or PR references, dates, or change history: git blame, git bisect, the issue and the plan hold
those. Comments are not version history.

A `FIXME` meets a high bar. It names a real defect left unfixed on purpose, never a style or
preference point. In most cases the defect is proposed as a GitHub issue instead. A `FIXME` is
written only when a reader must see the defect at that line, and it names no issue number.

When work on an issue touches a file, prune the comments in it that break this rule, and note on the
issue that unnecessary comments were removed in favour of code discoverability. Agents and
subagents, reviewers included, push back on a comment wherever clearer code would do its job.

## Don't
- Don't restate what the code does.
- Don't narrate a change (`changed X to Y`, `now uses…`, `previously…`).
- Don't record or justify a decision — that belongs on the issue, in the plan, or in the commit.
- Don't cite an issue, a PR or a date.
- Don't leave per-line play-by-play.
- Don't leave breadcrumbs: `moved-to`, `was`, `carved-from`.
- Don't write multi-paragraph rustdoc essays.
- Don't put `# Example` or `# Usage` sections in a doc comment.
- Don't put ASCII or markdown tables in a doc comment.

## Do
- Give each public item a one-line doc.
- Comment an ABI or wire contract.
- Comment a spec convention or a driver quirk.
- Comment a non-obvious external constraint.
- Comment a *why* the code's shape can't show.

Applies to Rust, Python, and everything else.

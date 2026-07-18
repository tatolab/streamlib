# Comments

Write a comment only when it carries information you can't get by reading the code. Default to none.

## Don't
- Don't restate what the code does.
- Don't narrate a change (`changed X to Y`, `now uses…`, `previously…`).
- Don't explain or justify a decision — that belongs in the PR/commit, not an inline comment.
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
- Delete narration or decision comments from any method you touch, in the same edit.

Applies to Rust, Python, Deno, and everything else.

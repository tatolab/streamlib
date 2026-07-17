# Comments

Comments cost tokens and bloat diffs. Write one only when it carries information **not derivable from
reading the code**. Default to none.

- **Allowed:** an ABI / wire contract, a spec convention or driver quirk, a non-obvious external
  constraint, or a *why* the code's shape cannot show. Public items get a **one-line** rustdoc (per
  `engine-doctrine`) — no multi-paragraph essays, no `# Example` / `# Usage`, no ASCII or markdown
  tables padding a doc comment.
- **Banned:** narrating what the code does, restating a name in prose, per-line play-by-play, or
  explaining / dictating a decision. Decisions live in the PR and commit rationale and in the code's
  shape — never in a perpetual inline comment. No breadcrumbs (`moved-to` / `was` / `carved-from`).
- **Clean as you go:** when you touch or review a method carrying narration or decision comments,
  remove them in the same edit. Cutting comment noise is part of the change, not a separate task.

Applies to Rust, Python, Deno, and everything else.

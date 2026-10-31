---
paths:
  - "docs/plan/**"
  - "docs/decisions/**"
  - "docs/working-agreements.md"
---

# Owner decisions

- **A design change enters the plan from the owner, never from a review thread.** A review
  comment — bot or human, on any PR — that would change what we build (a boundary, a wire
  shape, a catalog layout, where the engine reads a payload, what a tool names) is answered
  on the thread with `Owner decision needed: <the gap, one sentence>` and brought to the
  owner; the PR can merge without that change while the owner decides. A reviewer's finding that is a
  record — wording, an internal contradiction, a claim the tree falsifies, a format rule — is
  fixed in the PR and says so.
- **`(owner, <date>)` cites something the owner said.** The tag — in a `Rejected:` line, a
  `Decided` heading, or a trailing `Owner, <date>` — is written only from the align's Decided
  list, an owner comment, or an owner message in the session, and the PR body names which. A
  line the session derived carries `(session, <date>)` until the owner ratifies it at the next
  `/align`, and never reaches `docs/decisions/` or a `DECIDED` entry with that tag.

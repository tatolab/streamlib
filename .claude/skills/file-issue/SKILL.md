---
name: file-issue
description: File a piece of work in Linear — one issue, or a parent with one sub-issue per repo — in the right project and milestone. Use when the owner says "file this", "log that bug", or a discussion surfaces work worth tracking.
---

# File an issue

Issues live in Linear, team **Tatolab** (`TAT`), reached through the `mcp__Linear__*` tools.
Never create, edit or comment on a GitHub issue.

1. **Look for it first.** Search Linear for an existing issue that covers the intent; extend or
   comment on that one instead of filing a second.
2. **Shape it.** Work that lands in one repo is one issue whose title starts with that repo's
   prefix. Work that needs more than one repo is a parent issue with a plain title stating the
   outcome, and one prefixed sub-issue per repo (`parentId`).

   | prefix | repo |
   |---|---|
   | `streamlib:` | tatolab/streamlib |
   | `studio:` | tatolab/tatolab-app |
   | `site:` | tatolab/main-website |

   The title states the outcome or the symptom as a person would see it, not the mechanism.
3. **Place it.** Pick the existing project and milestone it advances; sub-issues take their
   parent's. If nothing fits, file it without one and say so, or propose a milestone or project —
   creating a project is the owner's call unless they asked for it.
4. **Label it** exactly one of `Bug`, `Feature`, `Improvement`. A parent carries none.
5. **Relate it.** Order between issues is a `blockedBy` / `blocks` relation, never a sentence in
   the description. A sub-issue that needs another repo's sub-issue first is blocked by it.
6. **Write the description for a person**: a few short sentences of what and why, then
   **Done means** — two to four outcomes someone could check. For a bug: symptom with the exact
   error text, what was expected, the smallest repro. Anything only an agent needs (where it
   bit in the code, ruled-out approaches, constraints copied from a design) goes last in a
   collapsible section, and only when it cannot be re-derived from the tree:

   ```
   +++ Agent notes

   ...

   +++
   ```

   Leave out file lists and step-by-step plans; they go stale before the issue is picked up.
7. **Report** each ID with its project, milestone and relations, and anything you guessed.

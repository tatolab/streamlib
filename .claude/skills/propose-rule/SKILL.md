---
name: propose-rule
description: Add, change, or delete a rule that governs how agents work in a repo — a CLAUDE.md line, a rules file, or the check that replaces one. Use when a mistake keeps recurring or the owner says "make that a rule".
---

# Propose rule

Rules pile up and rarely leave, and prose rules cost every later session attention. A rule a tool
enforces costs none.

1. **Bring the evidence**: the recurring review finding, the repeated owner correction, or the
   shipped defect. Two occurrences or one shipped incident is the bar.
2. **Check for overlap.** If an existing rule or check already covers it, extend or fix that one.
3. **Try a mechanical check first**: a CI step, a lint rule, a permission setting, or a
   hook that adds context or cleans up (a hook never blocks a tool call). Propose
   the check, a planted violation that proves it fires, and the deletion of any prose it replaces.
4. **Write prose only when no check can carry it** — a judgment the session makes before it opens
   a file. One invariant, one concern, positive phrasing, scoped to the paths it governs unless
   it is global, in as few lines as hold it.
5. **Show the owner** the exact wording or check, with the incident it would have prevented, and
   land it once they agree — in the PR of the work that needs it, or in its own.

Deleting a rule follows the same steps: the evidence is that a check now covers it, or that
nothing it guards against still happens.

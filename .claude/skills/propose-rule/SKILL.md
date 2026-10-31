---
name: propose-rule
description: Add, change, or delete a rule under .claude/rules/ — evidence first, one concern, the owner's agreement.
---

# Propose rule

Rules accumulate easily and rarely leave, so the owner likes each one to be an
invariant backed by evidence rather than a reaction to one annoyance. This is the shape
for proposing one.

1. **Evidence**: the recurring review finding, the repeated owner correction, or the
   shipped defect. Two occurrences, or one shipped incident, is a good bar.
2. **Overlap check**: does an existing rule or skill already cover it? Extending beats
   adding. When the concern is mechanical, a CI check or script that reports it often
   beats prose — but avoid hooks that block or prompt an agent; describe the reasoning
   instead and let CI catch the mechanical part.
3. **Draft**: the rule text (invariant style, one concern, positive phrasing), where it
   lives (`.claude/rules/<file>.md`, with a `paths:` scope unless genuinely global), and
   the concrete incident it would have prevented.
4. **Get the owner's agreement** on the draft — rules shape every later session, so they
   want to read the wording.
5. **Land it in its own PR** (`flow.md`), separate from feature work, so the owner can
   review the operating-model change on its own.

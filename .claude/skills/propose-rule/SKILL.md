---
name: propose-rule
description: The only path by which a rule under .claude/rules/ is added, changed, or deleted.
disable-model-invocation: true
---

# Propose rule

Rules are invariants born from evidence — never from one annoyance.

1. **Evidence in**: the recurring review finding, the repeated owner correction, or the
   shipped defect. Link at least two occurrences, or one shipped incident.
2. **Overlap check**: does an existing rule or an existing skill gate already cover it?
   Prefer extending. When the operation is mechanical, a gate (hook, script, CI check)
   beats prose — propose the gate AND the deletion of the prose it replaces.
3. **Draft**: the rule text (invariant style, one concern, positive phrasing), where it
   lives (`.claude/rules/<file>.md`, with a `paths:` scope unless genuinely global), and
   the concrete incident it would have prevented.
4. **The owner approves the draft in their own words.** Hard stop.
5. Land it in a **dedicated operating-model PR** (per `flow.md` — never mixed into
   feature work). A session never lands a rule it is itself subject to mid-task.

---
name: audit-drift
description: Detect (never repair) drift — stale plan sections, dangling references, dead rule globs, tickets pointing at retired plan entries.
disable-model-invocation: true
---

# Audit drift

Report only. No edits from this skill — staleness detection is automatable, staleness
repair is not.

1. For each SHIPPED section in `docs/plan/ARCHITECTURE.md` carrying a
   `<!-- verify: X -->` marker: has X changed since the section's last edit (compare
   `git log` on the verify target vs. the section)? List possibly-stale sections.
2. Dangling references: docs linking files that no longer exist; `.claude/agent-knowledge/`
   index rows pointing at removed docs; `.claude/rules/*.md` `paths:` globs that match
   nothing in the tree.
3. Tracker: open tickets referencing archived changes or OPEN plan sections.
4. Output one table: item / kind of drift / evidence / the skill that repairs it
   (`/align`, `/propose-change`, `/reconcile-tracker`). Repairs happen through those
   skills, never here.

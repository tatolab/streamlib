---
name: hygiene
description: Sniff out everything in the project that runs counter to the operating model — stale docs contradicting the plan, retired patterns still endorsed, references to deleted machinery, rotted README claims, dead rule globs, memory staleness, tickets pointing at retired plan entries. Detects and reports only; it never fixes anything.
disable-model-invocation: true
---

# Hygiene

The project's smell test. Report only — no edits from this skill, ever. Staleness
detection is automatable; staleness repair is a decision, and decisions route to the
skill that owns them.

Sweep these surfaces:

1. **Plan contradictions in docs.** Any `docs/` statement that contradicts a DECIDED or
   SHIPPED plan entry (the known worst offenders are pattern endorsements the plan has
   retired — e.g. a doc teaching a resolution or authoring pattern the plan replaced).
   The plan wins; the doc is the finding.
2. **Verify-marker staleness.** For each SHIPPED plan section with `<!-- verify: X -->`:
   has X changed since the section's last edit (`git log` both sides)?
3. **References to deleted machinery.** Mentions of removed workflows, retired skills,
   deleted files, or dead vocabulary anywhere in docs, skills, agents,
   `.claude/agent-knowledge/` indexes, hooks, or issue templates.
4. **Dead rule scopes.** `.claude/rules/*.md` `paths:` globs that match nothing in the
   tree; rules whose subject a skill gate now enforces (propose deletion via
   `/propose-rule`).
5. **README and top-level rot.** Claims in `README.md` and other entry docs that are
   false against the tree (platform status, SDK status, links into directories that
   left the repo).
6. **Memory staleness.** Memory files naming files, flags, or skills that no longer
   exist, or recording states the plan has since superseded.
7. **Tracker misalignment.** Open tickets referencing archived changes, OPEN plan
   sections, or retired patterns (summary only — `/reconcile-tracker` owns the deep
   audit).

Output: ONE table — surface / finding / evidence (`file:line` or quote) / the skill that
owns the fix (`/align`, `/propose-change`, `/reconcile-understanding`,
`/reconcile-tracker`, `/propose-rule`, or a docs-change ticket). Order by thrash risk:
things an agent might actually read and obey rank above cosmetic rot.

Run it: before a plan session (so decisions aren't debated against stale premises),
after a big change ships, or whenever something feels off. Its first full run seeds the
docs-consolidation change's kill-list.

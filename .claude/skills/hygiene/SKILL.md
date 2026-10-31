---
name: hygiene
description: Sniff out everything in the project that runs counter to the operating model — stale docs contradicting the plan, retired patterns still endorsed, references to deleted machinery, rotted README claims, dead rule globs, memory staleness, tickets pointing at retired plan entries. A report, with each finding routed to where it gets fixed.
---

# Hygiene

The project's smell test. It is a report: detecting staleness is mechanical, but
deciding which side of a contradiction moves (the doc or the plan) is often a judgment
call, so the output routes each finding to where it's best fixed. A finding that is
plainly a record — a dead link, a reference to a deleted file — can just be fixed in the
same pass if you're already opening a PR.

Sweep these surfaces:

1. **Plan contradictions in docs.** Any `docs/` statement that contradicts a DECIDED or
   SHIPPED plan entry (the known worst offenders are pattern endorsements the plan has
   retired — e.g. a doc teaching a resolution or authoring pattern the plan replaced).
   The plan wins; the doc is the finding.
2. **References to deleted machinery.** Mentions of removed workflows, retired skills,
   deleted files, or dead vocabulary anywhere in docs, skills, agents,
   `.claude/agent-knowledge/` indexes, hooks, or issue templates.
3. **Dead rule scopes.** `.claude/rules/*.md` `paths:` globs that match nothing in the
   tree; rules a skill or CI check now covers (propose deletion via `/propose-rule`).
4. **README and top-level rot.** Claims in `README.md` and other entry docs that are
   false against the tree (platform status, SDK status, links into directories that
   left the repo).
5. **Memory staleness.** Memory files naming files, flags, or skills that no longer
   exist, or recording states the plan has since superseded.
6. **Tracker misalignment.** Open tickets referencing archived changes, OPEN plan
   sections, or retired patterns (summary only — `/reconcile-tracker` owns the deep
   audit).

Output: ONE table — surface / finding / evidence (`file:line` or quote) / the skill that
owns the fix (`/align`, `/propose-change`, `/reconcile-understanding`,
`/reconcile-tracker`, `/propose-rule`, or a docs-change ticket). Order by thrash risk:
things an agent might actually read and obey rank above cosmetic rot.

Run it: before a plan session (so decisions aren't debated against stale premises),
after a big change ships, or whenever something feels off. Its first full run seeds the
docs-consolidation change's kill-list.

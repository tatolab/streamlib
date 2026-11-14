---
name: hygiene
description: Sweep the repo for rot an agent might read and obey — docs, rules, skills and comments that contradict the code or what is agreed, references to deleted things, dead rule scopes, stale Linear issues. Use before a design session, after a large change ships, or when something feels off.
---

# Hygiene

A report. Spotting staleness is mechanical; deciding which side of a contradiction moves is often
the owner's call. A finding that is plainly a record (a dead link, a mention of a deleted file)
can be fixed in the same pass, in a PR that says so.

Sweep:

1. **Contradictions with what is agreed.** Docs, rules, skills or agents that teach something the
   agreed record (the repo's CLAUDE.md says where it lives) has replaced. The agreed record wins.
2. **Contradictions with the code.** README and entry-doc claims, comments, and CLAUDE.md lines
   that are false against the tree: paths, commands, options, platform or CI behaviour.
3. **References to deleted things**: removed features, files, skills, workflows or vocabulary,
   anywhere in docs, rules, skills, agents, hooks, comments or memory files.
4. **Dead rules.** Rule `paths:` globs that match nothing, and prose a check now enforces; those
   go to `propose-rule` for deletion.
5. **Stale Linear issues.** Open issues that describe code or decisions that have changed, or
   whose blocking issue is Done or Canceled. Summarize only; `triage` does the grooming.

Output one list, ordered by how likely an agent is to read the wrong text and act on it. Each
finding gives the place, the evidence (`file:line` or a quote), and where the fix goes: a docs PR,
`reconcile-understanding`, `propose-rule`, `triage`, `file-issue` for another repo, or the owner
when the agreed record itself looks wrong.

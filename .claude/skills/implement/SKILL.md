---
name: implement
description: Build one Linear issue to an open pull request. Use when given a TAT id to implement.
---

# Implement

Input: a `TAT-n` id.

1. **Load** the issue from Linear with its relations, every comment, its parent, and its project
   and milestone descriptions. A comment wins over the description where they conflict. If the
   issue is a parent, build the sub-issue whose prefix names this repo; if its prefix names
   another repo, stop and say so.
2. **Check what it waits on.** Every issue blocking this one must be Done, and where the blocker
   is another repo's work, present in the version this repo builds against. If not, say which
   one and stop; never work around a missing dependency.
3. **Check it against the tree.** Where the issue's claims about the code have drifted, correct
   the description and carry on with what it now means.
4. **Branch.** Use the issue's `gitBranchName`. If the session was handed a branch, keep it and
   put the id in the PR title instead: `fix(scope): summary (TAT-n)`. Either one links the PR;
   never write `Closes #n`.
5. **Build test-first at the real seams** the repo already tests through. Every commit builds.
6. **Keep to the issue's scope.** A side finding is a note in the PR body, or a new Linear issue
   in the same project only when this work cannot land without it. A scope note forbids
   redesigning something; it does not forbid fixing a defect the issue's own fix also cures.
7. **A decision the issue does not settle.** Read the issue, its comments and the tree first —
   most questions are answered there. For what remains, build your recommendation and state the
   assumption in the PR. Only when the choice is a real fork — materially different outcomes, a
   wrong guess wastes the work, or it is expensive to undo — comment on the Linear issue with the
   options and your recommendation, and keep building everything that does not depend on it.
8. **Before you push**, run everything the repo's `CLAUDE.md` names under `## Before you push`:
   the checks, the review agents, the proof. Fix what they find.
9. **Open the PR**: what changed and why, how it was tested, the proof, and notes for the owner
   (assumptions made, side findings, anything not proven). Do not move the issue's status or
   close it — Linear does both from the PR. The owner merges.

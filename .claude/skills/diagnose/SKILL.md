---
name: diagnose
description: Feedback-loop-first debugging. Use when something is broken, throwing, failing, flaky, or slow — before reading code to build a theory.
---

# Diagnose

1. **Build the feedback loop first**: one command, already run once, deterministic and fast, that
   shows the failure — a failing test, else a script, a throwaway harness, or a bisection. Reading
   code to build a theory before that command exists is the trap this skill is for. If no loop
   can be built, say what access or artefact is missing and continue on the best evidence.
2. **Reproduce the reporter's failure**, on the platform and install kind where it happened, then
   minimize one element at a time until everything left is load-bearing.
3. **Write three to five ranked, falsifiable hypotheses before testing any.** Show the list and
   proceed on the ranking. When the system has layers, first find which layer the failure is in.
4. **Instrument one variable at a time**, each probe tied to a prediction. Tag every debug line
   `[DEBUG-xxxx]` so one grep removes them all.
5. **Write the regression test before the fix**, at a real seam. If no good seam exists, say so in
   the PR and land the fix with the best test available.
6. **Clean up**: the repro and every `[DEBUG-` line gone, the confirmed hypothesis in the commit
   message.

The fix lands through `implement` against a `Bug` issue. A defect that belongs to another repo is
filed there with `file-issue`, never patched around here.

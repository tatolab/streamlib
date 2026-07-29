---
name: diagnose
description: Feedback-loop-first debugging. Use when something is broken, throwing, failing, flaky, or slow — before reading code to build a theory.
---

# Diagnose

Phases in order. The feedback loop is the skill; everything else is mechanical.

1. **Build a red-capable feedback loop first**: one command — already run at least once
   (paste the invocation and its output) — deterministic, fast, that shows the failure.
   Failing test > script > throwaway harness > bisection. Catching yourself reading code
   to build a theory before this command exists is the exact failure this skill
   prevents: **no red-capable command, no phase 2.** Genuinely can't build one → stop
   and say what access or artifact you need.
2. **Reproduce the reporter's failure mode** (wrong bug = wrong fix), then minimize one
   element at a time until everything left is load-bearing.
3. **3–5 ranked falsifiable hypotheses before testing any** — single-hypothesis
   generation anchors on the first plausible idea. Show the list; proceed on your
   ranking if the owner is away. Route domain symptoms through the matching expert's
   index in `.claude/agent-knowledge/` before debugging from scratch.
4. **Instrument** one variable at a time, each probe mapped to a prediction. Tag every
   debug line `[DEBUG-xxxx]` so cleanup is one grep.
5. **Regression test before the fix**, at a real seam. If no correct seam exists, that
   itself is a finding for `/propose-change` — and the fix still lands with the best
   available test.
6. **Cleanup**: repro gone, every `[DEBUG-` gone, the confirmed hypothesis stated in the
   commit message.

The fix itself lands through `/implement` against a bug ticket — bug fixes are tier 1 of
the scale gate and need no change artifact.

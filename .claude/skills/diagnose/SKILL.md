---
name: diagnose
description: Feedback-loop-first debugging. Use when something is broken, throwing, failing, flaky, or slow — before reading code to build a theory.
---

# Diagnose

Phases in order. The feedback loop is the skill; everything else is mechanical.

1. **Build a red-capable feedback loop first**: one command — already run at least once
   (paste the invocation and its output) — deterministic, fast, that shows the failure.
   Failing test > script > throwaway harness > bisection. Catching yourself reading code
   to build a theory before this command exists is the trap this skill exists to avoid
   — theories built without a loop tend to be wrong and hard to falsify. If you
   genuinely can't build one, say what access or artifact you'd need, and carry on with
   the best evidence available.
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

The fix usually lands like any ticket (`/implement`) — bug fixes are the first scale
tier and need no change artifact.

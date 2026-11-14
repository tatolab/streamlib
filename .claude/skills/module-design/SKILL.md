---
name: module-design
description: Vocabulary and procedure for designing a new or reshaped module, interface, or API surface before writing it. Use when designing any interface, module, command, contract, or cross-package seam, or when comparing alternative shapes for one.
---

# Module design

Use exactly these terms. A **module** is anything with an interface and an implementation. Its
**interface** is everything a caller must know: the signature, plus invariants, ordering, error
modes, and performance or asynchrony. **Depth** is behaviour gained per unit of interface learned.
A **seam** is a place behaviour can change without editing there; an **adapter** fills one.
**Locality** is what maintainers get, **leverage** what callers get. Never "component", "service",
or "boundary".

1. **Search first.** Prove no existing system in the tree already covers the concern. Extend that
   one; never build a parallel one.
2. **Design it twice or more.** Produce independent designs under different constraints, in
   parallel subagents when available: the smallest interface, the best fit for the dominant
   caller, ports and adapters. Each gives the interface, a usage example, what it hides, its
   error taxonomy, and its trade-offs.
3. **Compare on depth, locality and seam placement**, and give one recommendation, not a menu.
4. **Record the chosen shape before code commits to it**, where the repo's CLAUDE.md says design
   decisions live; failing that, in the Linear issue's description. What the shape needs from
   another repo is an issue there, through `file-issue`, that this work is blocked by.

The interface is the test surface: wanting to test past it means the module has the wrong shape.
One adapter is a hypothetical seam; two is a real one.

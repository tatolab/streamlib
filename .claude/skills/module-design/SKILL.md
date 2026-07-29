---
name: module-design
description: Design vocabulary and procedure for new or reshaped modules, traits, and API surfaces — before writing them. Use when designing any trait, module, public interface, or cross-crate boundary, or when comparing alternative shapes for one.
---

# Module design

Vocabulary — use exactly these terms: **module** (anything with an interface and an
implementation), **interface** (everything a caller must know: signature plus
invariants, ordering, error modes, performance), **depth** (behavior gained per unit of
interface learned), **seam** (a place behavior can change without editing there),
**adapter**, **locality** (what maintainers get), **leverage** (what callers get).
Never "component", "service", or "boundary".

Procedure:

1. **Search first.** Prove no core system (RHI, `GpuContext`, pubsub, processor model,
   package source) already covers the concern. Extending beats parallel — always.
2. **Design it twice.** Spawn parallel design subagents with different constraints —
   minimize the interface / optimize for the dominant caller / ports-and-adapters. Each
   returns: the interface, a usage example, what's hidden, the error taxonomy,
   trade-offs.
3. **Compare** on depth, locality, and seam placement. Give ONE opinionated
   recommendation — never a menu.
4. The chosen shape goes into the change proposal via `/propose-change` — it is never
   implemented straight from chat.

Testing stance: **the interface is the test surface.** Wanting to test past it means the
module is the wrong shape. One adapter is a hypothetical seam; two adapters is a real one.

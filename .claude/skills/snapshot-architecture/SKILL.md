---
name: snapshot-architecture
description: Rebuild the code-derived architecture snapshot — Claude's current understanding of how the system actually works — as a living Claude Artifact with Mermaid diagrams, code references, and a drift table against the plan. Use when the owner wants the snapshot refreshed, suspects code and plan have drifted, or wants a shared picture to discuss and correct.
disable-model-invocation: true
---

# Snapshot architecture

Two truths, kept separate: **code is the authority on what IS; the plan is the authority
on what we AGREED to build.** The snapshot is descriptive — derived from code, never from
the plan — and its whole value is showing where the two disagree so the owner can pick
which one moves. This skill never edits the plan, the docs, or the code.

1. **Survey, read-only.** Fan out Explore/domain-expert subagents across `runtime/`,
   `sdk/`, `tools/`, `adapters/`, `xtask/` — what subsystems exist, how they connect,
   what the real boundaries and data flows are. Every claim needs a source
   (`file:line` or path); anything not verified in the tree is labeled INFERRED.
2. **Load the `artifact-design` skill**, then build one HTML page:
   - A top-level system diagram plus per-subsystem sections, each with a Mermaid diagram
     (```mermaid fences render natively in artifacts) and prose.
   - Every claim tagged **[VERIFIED path:line]** or **[INFERRED — confidence]**.
   - A **drift table**: plan says / code shows / which should probably move (owner
     decides; the table only surfaces).
   - An **open questions** section — the things Claude is least sure it understands,
     explicitly inviting correction.
3. **Publish as ONE living artifact.** Stable title "StreamLib Architecture Snapshot
   (code view)", stable favicon. In a session that didn't create it, find it with the
   Artifact tool's `action: "list"` and pass its `url` to redeploy — never mint a second
   snapshot URL.
4. **Close the loop.** End by telling the owner: corrections go through
   `/reconcile-understanding`; drift resolutions go through `/align` (plan moves) or a
   bug ticket (code regressed). The snapshot is regenerated after either.

---
whoami: amos
name: "Research: DMA-BUF FD passing for Linux polyglot escalation"
status: pending
description: Gated by #320 §8.Q4. #325 ships with pool-IDs-only over JSON-RPC, which can't carry file descriptors. Before the next Linux polyglot iteration (Python/Deno processors rendering to host-allocated DMA-BUF textures), we need a story for FD passing. Compares Unix-domain-socket side-channel, iceoryx2 FD transfer (if available), and generalizing the macOS SurfaceStore broker pattern to Linux.
github_issue: 347
dependencies:
  - "down:Design doc: GpuContextLimitedAccess + GpuContextFullAccess API surface"
adapters:
  github: builtin
---

@github:tatolab/streamlib#347

See the GitHub issue for full context. Research-only deliverable:
`docs/research/polyglot-dma-buf-fd.md` (or §5 amendment to the design
doc). Lists the chosen option, IPC schema sketch, host-side routing,
subprocess client surface, and platform conditionalization.

## Parent

#319 (GPU capability-based access umbrella)

## Relationship

- Follow-up to #325 (polyglot escalate-on-behalf initial shape).
- Not a blocker for #325 itself — filed so it isn't lost when #325
  lands and someone asks "now what about DMA-BUFs on Linux."

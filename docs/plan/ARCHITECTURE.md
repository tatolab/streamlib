# StreamLib Architecture Plan

The single source of architectural decisions. Sessions implement this plan; they do not make
architecture. A decision missing here stops work and comes back to the owner — it is never
inferred from existing code, consumers, or history. This document and the diagrams under
`diagrams/` (Mermaid `.mmd`, the committed source — Excalidraw files are generated views,
never round-tripped back) move together: every DECIDED entry is represented in the diagram.

Legend: **DECIDED** — build exactly this. **OPEN** — do not build; needs an owner decision.

## Product (the MVP sentence)

- **OPEN** — One sentence a real user acts on: what they install, what they run, what they see.
  Every ticket traces to this sentence or does not exist.

## Module system — packages, versions, imports

- **DECIDED** — Packages resolve like node modules: version ranges resolved from a package
  source at build time, never from sibling directories. `add`/`install` take finalized
  artifacts only; `link` is the sole local-dev path.
- **OPEN** — Remaining resolution semantics (lockfile story, upgrade flow, engine-version
  compatibility signaling).

## Processor model & scheduling

- **OPEN**

## Graphics (RHI / GPU)

- **DECIDED** — All Vulkan lives in the RHI (`vulkan/rhi/` + `streamlib-consumer-rhi`); one
  kernel abstraction per pipeline kind; consumers go through `GpuContext` only.
- **OPEN** — Everything else.

## Media I/O — camera, display, audio

- **OPEN**

## Networking — transport, moq, webrtc

- **OPEN**

## Language SDKs & parity

- **OPEN** — Which runtime leads during MVP; which surfaces are parity-required.

## Distribution & versioning

- **OPEN**

## Control plane & observability

- **OPEN**

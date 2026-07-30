# StreamLib Architecture Plan

The single source of architectural decisions. Sessions implement this plan; they do not make
architecture. A decision missing here stops work and comes back to the owner — it is never
inferred from existing code, consumers, or history. This document and the diagrams under
`diagrams/` (Mermaid `.mmd`, the committed source — Excalidraw files are generated views,
never round-tripped back) move together: every DECIDED entry is represented in the diagram.

Legend: **DECIDED** — build exactly this. **OPEN** — do not build; needs an owner decision.

## Product (the MVP sentence)

- **DECIDED** — A Python developer on Linux with an NVIDIA GPU installs streamlib from
  PyPI, runs `streamlib new` then `streamlib dev`, sees their camera live in a window
  within a minute, and makes the pipeline theirs by editing the scaffolded processor —
  zero ceremony: no manifest, no `main()`, no schema wrangling, hot-reload on save.
  Every ticket traces to this sentence or does not exist. [product-mvp-sentence]
- **DECIDED** — Terms of the sentence: PyPI ships the single binary (CLI + runtime +
  orchestrator); platform floor is Linux + NVIDIA; `streamlib new` scaffolds a working
  camera → effect → display app with dependencies already installed — first `streamlib
  dev` shows live video before any editing; `dev`/`run` find `app.py`'s `setup(rt)` by
  convention, `-f <file>` overrides; an app carries no manifest (entry file +
  `streamlib.lock` + `streamlib_modules/`) and promotes to a package by adding the
  identity label; app-local processors live in `processors/`, discovered by the same
  scan as plugins, minted `@app/local/<Name>`, and `rt.add` accepts the string id or
  the imported class; the pipeline API is `add`/`connect`. [product-mvp-sentence]
- **DECIDED** — The zero-ceremony bar (the sentence is untrue until all hold): no
  manifest authoring; no boilerplate entry; bags/schemas fixed (no engine schema
  matching, cast-at-read, no versions at the code layer); scaffolding commands for
  app, plugin, processor, and schema. [product-mvp-sentence]
- **DECIDED** — Rust processor authoring (C++ later) stays a supported capability for
  hardware-facing packages, outside the MVP sentence; existing Rust plugins port to
  the new format as the final step, so they install as modules. [product-mvp-sentence]

## Module system — packages, versions, imports

- **DECIDED** — Packages resolve like node modules: version ranges resolved from a package
  source at build time, never from sibling directories. `add`/`install` take finalized
  artifacts only; `link` is the sole local-dev path.
- **OPEN** — Remaining resolution semantics (lockfile story, upgrade flow, engine-version
  compatibility signaling).

## Processor model & scheduling — IN-FLIGHT (→ schema-agreement-ripout)

- **DECIDED** — A link is pure plumbing: output port → input port, carrying a bag
  (self-describing msgpack named map). Producer and consumer type declarations are
  unilateral hints, never compared; consuming is a cast at read time. The engine
  mediates no schema agreement: connect never refuses a link (advisory log at most),
  no per-read tag matching, the wire tag is inert observability metadata, and versions
  never appear at the code layer — resolution-time only. [data-plane-cast-not-contract]
- **DECIDED** — Channel policy (delivery profile, ring depth, overflow) is declared
  port-locally at the consuming input port, never carried by schemas; a concretely-typed
  input port with no declared delivery profile is a wiring error, not a silent default.
  [data-plane-cast-not-contract]
- **DECIDED** — Three execution modes (reactive / manual / continuous); one dedicated
  OS thread per processor with descriptor-driven priority (realtime / high / normal);
  synchronous lifecycle traits; Full/Limited capability typestate on the phase axis
  (setup/teardown vs process). [execution-model]
- **OPEN** — Additional execution flavors to scale processor count (lightweight /
  green-thread style): intended, do not build until designed; hard constraint — no new
  configuration dials. [execution-model]

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

- **DECIDED** — One control plane: the api-server's HTTP + WebSocket + MCP surface,
  hosted in-process by any runtime that enables it. The MCP tool set is the canonical
  control vocabulary; the CLI is a pure JSON-RPC client of it — agents and humans drive
  the same verbs; REST/WS routes serve the same operations for programmatic clients.
  [control-plane-one-surface]
- **DECIDED** — The api-server is engine-side infrastructure and relocates into the
  `runtime/` tree: it is a host — statically linked, never dlopen'd — and cannot follow
  the packages tree out of the repo. [control-plane-one-surface]
- **DECIDED** — One shipped binary (CLI + runtime + build orchestration): `run`/`dev`
  host the runtime in-process; the standalone streamlib-runtime binary retires. The
  engine remains an embeddable Rust library for host apps; non-Rust embedding drives a
  runtime through the client-SDK / control-plane path. [single-binary-launch]
- **DECIDED** — Node discovery is a per-user on-disk registry — one JSON file per live
  node in the OS's standard per-user runtime directory — written only by
  control-plane-hosting runtimes, pruned only when both liveness signals (control
  round-trip, process check) fail. [control-plane-one-surface]
- **DECIDED** — Observability: the JSONL log schema is a durable contract; tap forwards
  bags verbatim, trading completeness for guaranteed non-interference; graph and health
  inspection ride the same control plane. [control-plane-one-surface]
- **OPEN** — Auth and remote-access posture.

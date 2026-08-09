# CLAUDE.md

## Licensing (load-bearing — do not modify)

StreamLib is licensed under the Business Source License 1.1 (BUSL-1.1). Never suggest MIT / Apache
or relax the commercial-use restriction. Every new Rust file carries:

    // Copyright (c) 2025 Jonathan Fontanez
    // SPDX-License-Identifier: BUSL-1.1

Exception: `vendor/tatolab-vulkanalia*` is the vendored vulkanalia fork and stays Apache-2.0 —
never add a BUSL header there, never reformat those sources. Do not modify `LICENSE`, `LICENSES/`,
or `docs/license/` without explicit approval. See `docs/architecture/vendored-vulkanalia.md`.

---

StreamLib is a BUSL-1.1-licensed real-time streaming processing runtime like Nvidia holoscan.It is built like a game engine:
ONE core system per concern — extend the existing system, never build a parallel one. Search first.

## The router — how work happens

- All work enters through `/plan` — it reads the plan statuses and the tracker and says
  which skill is next.
- Source edits (`runtime/ sdk/ adapters/ xtask/`) happen only inside `/implement`
  with an owner-confirmed ticket — hook-enforced via `.claude/state/active-ticket.json`.
- Plan edits (`docs/plan/**`) happen only inside `/align`, `/propose-change`,
  `/ship-change`, or `/pivot` — hook- and ask-rule-enforced.
- Lifecycle: `/align` (decide) → `/propose-change` (delta) → `/derive-tickets` (as few
  tracer bullets as the change honestly needs) →
  `/implement` (build) → `/ship-change` (fold + prove removals). `/pivot` for direction
  changes, `/diagnose` for bugs, `/research` for questions, `/reconcile-tracker` keeps
  GitHub a projection of the plan, `/propose-rule` is the only way rules change.
- Shared understanding: `/explore-idea` (what-ifs before any commitment),
  `/snapshot-architecture` (the living code-derived picture), `/architecture-question`
  (how does X work, with evidence), `/reconcile-understanding` (corrections that stick).
  Code is the authority on what IS; the plan on what we AGREED.
- Full model: `docs/plan/OPERATING-MODEL.md`.

## Runtime-first, plan-first (MVP doctrine)

The runtime is the product; consumers lag behind it by design (the Holoscan / Next.js model:
framework repo first, holohub/examples follow releases). Architecture lives in ONE place —
`docs/plan/ARCHITECTURE.md` plus `docs/plan/architecture.excalidraw` — agreed with the owner
before implementation. Sessions implement the plan; they never make architecture.

- `examples/` and the consumer entries in `packages/` (everything except `escalate`, `core`,
  `test-fixtures`) are downstream consumers, **not contract sources**. Never read them to infer
  what the engine guarantees; never edit them to make an engine change pass; never bend a runtime
  design to fit their existing patterns. Contracts are stated in the engine and proven by engine
  tests and fixtures.
- An engine change that breaks a package or example is **expected, not a defect** —
  it is upgrade backlog for a later consumer-upgrade session run inside that consumer, not work
  for this session. Do not file tickets for it.
- File an issue only when something blocks the current milestone. Non-blocking findings go in the
  PR description as a note, then we move on. Getting an MVP into users' hands beats completeness.
- These directories are moving to `tatolab/streamlib-packages` (#1672). Reading them is allowed —
  they are reference material for parity and completeness checks (how processors are actually
  written, which API surfaces real code exercises). Editing stays deny-ruled: they are never bent
  to make an engine change pass, and never treated as contract sources.

Captured knowledge lives in `docs/learnings/`; design rationale in `docs/decisions/`. However, these may go stale and should be verified, not viewed as facts. It serves as a cache. Everything else is re-derived
from code at need — do not create summary docs of what code already shows.

## Non-negotiables
- All Vulkan calls live in the RHI (`runtime/streamlib-engine/src/vulkan/rhi/` +
  `runtime/streamlib-consumer-rhi/`). Nothing else touches `vulkanalia`. CI enforces.
- Logging is `tracing` only — no `println!`/`eprintln!` (CI enforces).
- No `todo!()`/`unimplemented!()` in library code; no back-compat shims (pre-1.0).
- New Rust files carry the BUSL header. Never touch `vendor/tatolab-vulkanalia*` or license files.
- Names pass the zero-context test: `LinkOutputDataWriter`, never `Writer`. Explicit beats short.
- Engine-wide defects get fixed at the engine layer, never bandaided in the consumer that
  surfaced them. Pattern migrations cover the engine tree only — `packages/` and `examples/`
  lag by design.
- Architecture is decided in `docs/plan/`, never per-ticket. A missing decision stops work and
  goes to the owner; it is never inferred from existing code.
- Tests are always in scope and never need approval. Code drives tests, never the reverse.


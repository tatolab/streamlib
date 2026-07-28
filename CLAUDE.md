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

Captured knowledge lives in `docs/learnings/`; design rationale in `docs/decisions/`. However, these may go stale and should be verified, not viewed as facts. It serves as a cache. Everything else is re-derived
from code at need — do not create summary docs of what code already shows.

## Non-negotiables
- All Vulkan calls live in the RHI (`runtime/streamlib-engine/src/vulkan/rhi/` +
  `runtime/streamlib-consumer-rhi/`). Nothing else touches `vulkanalia`. CI enforces.
- Everything crossing the plugin ABI is `#[repr(C)]` with a layout regression test.
- Logging is `tracing` only — no `println!`/`eprintln!` (CI enforces).
- No `todo!()`/`unimplemented!()` in library code; no back-compat shims (pre-1.0).
- New Rust files carry the BUSL header. Never touch `vendor/tatolab-vulkanalia*` or license files.
- Names pass the zero-context test: `LinkOutputDataWriter`, never `Writer`. Explicit beats short.
- Engine-wide defects get fixed at the engine layer, never bandaided in the consumer that
  surfaced them. When a change makes a new pattern canonical, migrate every consumer of the old
  pattern in the same PR.
- Tests are always in scope and never need approval. Code drives tests, never the reverse.


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

StreamLib is a BUSL-1.1-licensed real-time media engine (Vulkan RHI, V4L2, vulkan-video codecs,
iceoryx2 IPC, dlopen'd .slpkg plugin packages, Python/Deno SDKs). It is built like a game engine:
ONE core system per concern — extend the existing system, never build a parallel one. Search first.

Rules load from `.claude/rules/` (licensing, naming, engine doctrine, comments always; RHI, plugin-ABI,
polyglot, docs-policy, flow rules load when you read matching files). Empirical driver knowledge
lives in `docs/learnings/`; design rationale in `docs/decisions/`. Everything else is re-derived
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

## How work happens
One ticket at a time, with the owner present. `amos` tracks the dependency graph: `amos focus`
scopes to a milestone, `amos next` reports what is ready to start, `amos blocked` shows what is
gated and by what. Pick a ticket, agree the plan, then one branch per issue and a PR when the
gates are green.

Read every issue fresh against current code — the body is the goal, not a spec, and its file
paths and claims may have gone stale since it was filed. Labels are display output only; nothing
reads a label as control flow.

Work artifacts live on GitHub (issues, comments, branches, PRs). Anything needing the owner is
asked directly in session. Merging PRs and milestone scoping are always the owner's calls.
"The owner" is the repository owner's GitHub login — the human who merges PRs.

## Environment
- A plain `Bash` call cannot observe GPU/IPC runtime (exit 144). Live verification runs via
  `/verify-live` — the Bash `dangerouslyDisableSandbox` bypass unlocks the rig, so the build
  happens in the sandbox, the built binary runs with the bypass, the window is captured, and the
  result is audited in place. Falls back to the owner-terminal handshake when the rig is
  unavailable. Read-only device probes (`v4l2-ctl` query verbs) are fine.
- One camera consumer per /dev/videoN; single GPU — never run two rig tasks at once.
- Host-specific facts (device indices, driver, cameras) live in `docs/rig-profile.local.md`
  (gitignored, per machine); a runtime probe always beats the file.

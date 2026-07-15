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

Rules load from `.claude/rules/` (licensing, naming, engine doctrine always; RHI, plugin-ABI,
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
Loops drive the work (see `LOOP.md` and `loops/`). The standing form is
`/loop 30m /goal <milestone condition> — each turn: one milestone-loop reconciler pass`.
A router classifies each work item fresh every pass and launches the matching workflow;
labels are display output only — nothing reads them as control. Durable loop state lives in
`loops/` state files; work artifacts live on GitHub (issues, comments, branches, draft PRs).
Anything needing the owner parks as a question on the issue; they answer in a comment.
Merging PRs and milestone scoping are always the owner's calls. "The owner" is the
repository owner's GitHub login — the human who merges PRs and answers parked questions.

## Environment
- Sandboxed sessions cannot observe GPU/IPC runtime (exit 144). Live verification is human-run
  via `/verify-live`. Read-only device probes (`v4l2-ctl` query verbs) are fine.
- One camera consumer per /dev/videoN; single GPU — rig work is serialized by the loop.
- Host-specific facts (device indices, driver, cameras) live in `docs/rig-profile.local.md`
  (gitignored, per machine); a runtime probe always beats the file.

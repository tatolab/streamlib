# CLAUDE.md

## Licensing (load-bearing — do not modify)

StreamLib is licensed under the Business Source License 1.1 (BUSL-1.1). Never suggest MIT / Apache
or relax the commercial-use restriction. Every new Rust file carries:

    // Copyright (c) 2025 Jonathan Fontanez
    // SPDX-License-Identifier: BUSL-1.1

Exception — vendored third-party trees keep the licence they arrived under. Never add a BUSL
header to one, and never reformat or "improve" those sources — a change to one is a
recorded patch against its upstream, never a drive-by edit:

- `vendor/tatolab-vulkanalia`, `-sys` and `-vma` — the vulkanalia fork, Apache-2.0. See
  `docs/architecture/vendored-vulkanalia.md`.

The exception is those paths and nothing else; BUSL is not relaxed anywhere a path is not
listed. Do not modify `LICENSE`, `LICENSES/`, or `docs/license/` without explicit approval.

---

StreamLib is a BUSL-1.1-licensed real-time streaming processing runtime like Nvidia holoscan.It is built like a game engine:
ONE core system per concern — extend the existing system, never build a parallel one. Search first.

## How work usually flows

Skills describe how the owner likes work reasoned about; adapt or skip them as needed.
Skills, agents, the repository rules and hooks come from `dev-plugin` in
[tatolab/tato-plugins-official](https://github.com/tatolab/tato-plugins-official), which
`.claude/settings.json` enables; change them there, never in this repository.

- **Work is tracked in Linear, never in GitHub issues** (settings deny `gh issue` writes and the GitHub MCP issue tools):
  team Tatolab, issues `TAT-n`, structured project → milestone → issue → sub-issue. An issue
  that lands here is titled `streamlib: …`; a change spanning repos is a parent with one
  sub-issue per repo.
- **Linear's projects and milestones are the order of work**, and blocking relations the
  order inside them. `/whats-next` suggests the next issue; the order is the owner's call.
- The usual lifecycle: `/align` (decide) → `/propose-change` (delta) → `/plan-project` (the
  Linear project) → `/implement` (one issue to a PR) → `/ship-change` (fold + prove removals).
  Beside it: `/pivot`, `/file-issue`, `/diagnose`, `/research`, `/triage`, `/explore-idea`,
  `/snapshot-architecture`, `/architecture-question`, `/reconcile-understanding`, `/propose-rule`.
- Plan *decisions* travel through `/align`, `/propose-change`, `/ship-change` or `/pivot`;
  plan and doc *records* are ordinary work — see §Recording facts vs deciding.
- Code is the authority on what IS; the plan on what we AGREED (`docs/plan/OPERATING-MODEL.md`).

## Recording facts vs deciding

Writing down what is true is ordinary work. Deciding what we build is not. The lifecycle
skills exist to sharpen decisions, not to license prose.

- **A factual record needs no skill, no prompt, and no approval.** What a shipped change
  removed, renamed, or proved — a `REMOVED:` bullet, a superseded claim, a corrected file
  anchor, a stale reference, a doc describing code that no longer exists — is part of
  finishing the work. Write it in the same PR as the change it describes. Leaving the
  record wrong to avoid ceremony is the worse outcome, always.
- **A decision is the owner's.** Adding, retracting or reversing a `DECIDED` / `OPEN`
  entry, changing what we agreed to build, or settling a question the plan leaves open
  is best brought to the owner — usually through `/align`, `/propose-change`,
  `/ship-change`, or `/pivot`.
- **A review comment is not a decision source.** A finding on a PR — bot or human — that
  would change what we build is answered `Owner decision needed: <the gap>` and brought to
  the owner; a record is fixed in the PR (`streamlib-owner-decisions` (dev-plugin)).
- The test: **could a careful reader derive it from the diff and the tree?** Then it is a
  fact — write it, and say in the PR body that you did. Does it commit us to something we
  have not agreed? Then it is a decision — bring it, while you keep going on the rest.
- Doc hygiene inside work you are already doing — fixing a claim your own change falsified —
  is never a separate issue and never a question.

## Asking the owner

An approved design is standing authority. Implementing it, recording it, and cleaning up
after it need no further confirmation. Re-asking is not diligence; it spends the owner's
attention, and attention spent on settled things is unavailable for real forks.

- **Ask only for a live fork the plan does not settle** — two defensible paths, materially
  different outcomes, where guessing wrong wastes real work.
- **Never ask** to re-confirm something already agreed, to re-approve a scope already
  signed off, to check that work is going well, or in place of reading the tree. If the
  answer is discoverable, discover it.
- **Prefer a stated assumption to a blocking question.** Do the work, say plainly what you
  assumed and where it would bite, and let the owner correct it. Wait only where
  proceeding either way is unsafe or would waste the work.
- **Batch** what genuinely must be asked into one round at the point of decision — never a
  drip of one-question stops.
- A finding is not a question. Non-blocking findings go in the PR description, then move on.

## Runtime-first, plan-first (MVP doctrine)

The runtime is the product; consumers lag behind it by design (the Holoscan / Next.js model:
framework repo first, holohub/examples follow releases). Architecture lives in ONE place —
`docs/plan/ARCHITECTURE.md` plus `docs/plan/architecture.excalidraw` — agreed with the owner
before implementation. Sessions implement the plan; when work needs architecture the plan
doesn't state, the owner wants to make that call (§Asking the owner).

- `examples/` and the consumer entries in `packages/` (everything except `test-fixtures`) are
  downstream consumers, **not contract sources** — converted or not. Never read one to infer what
  the engine guarantees; never bend a runtime design to fit their existing patterns. Contracts
  are stated in the engine and proven by engine tests and fixtures. Disposition per directory is
  decided in `docs/plan/ARCHITECTURE.md` §Consumers.
- **A converted consumer** — current-idiom shape: a scaffolded project (`stream.py` + `pyproject.toml`)
  or an ordinary Python package — **is the model of the current authoring idiom and ordinary
  editable work.** An engine change that breaks one files a Linear backlog issue for that consumer and
  never blocks the engine change; fixing it in-stream is reserved for the rare case where the
  consumer is the deliberate canary of the in-flight work.
- File a Linear issue only when something blocks the milestone in flight. Non-blocking findings
  go in the PR description as a note, then we move on. Getting an MVP into users' hands beats completeness.
- **A held pre-pivot consumer** (`Cargo.toml` + `setup.sh` shape) keeps the old treatment:
  written against deleted machinery — the identity grammar, the schema layer, `streamlib.yaml`
  manifests, the package-as-distributable shape — so read it for *logic* only (how a codec was
  wired to the RHI, what a capture path must handle), never for form; breakage is expected, not
  a defect, and gets no issue. It converts or deletes only through §Consumers' rules, never in
  passing.

Working agreements are in `docs/working-agreements.md`; `docs/index.md` maps every docs home. Captured knowledge lives in `docs/learnings/`; design rationale in `docs/decisions/`. However, these may go stale and should be verified, not viewed as facts. It serves as a cache. Everything else is re-derived
from code at need — do not create summary docs of what code already shows.

## Reading the Python surface

`sdk/tatolab-stream/tatolab/stream/` is the reference for the Python surface. Its Protocols
(`_node_context_protocols.py`, `_gpu_protocols.py`, `_node_owned_window_protocols.py`,
`clock.py`) write down each contract a node is handed, and `_built_in_nodes.py` writes down
every built-in's config keys and ports — read them before reading Rust. A detour into `runtime/` to learn what a
built-in publishes on means you skipped it.

- **The types live in `tatolab.stream`.** pyo3 publishes no annotations, so `help()` on a
  native object shows no parameter or return types. Read the Protocol, not the REPL.
- **The reference cannot drift.** The conformance gate
  (`sdk/streamlib-python-wheel/tests/runtime_backed_protocol_conformance.py`) holds every
  native class and function a node is handed to its `tatolab.stream` Protocol or runtime-backed
  function, member for member, and every other one to its entry in
  `sdk/streamlib-python-wheel/python/tatolab/runtime/_engine.pyi`, which stubtest checks
  against the binary. `cargo xtask generate-built-in-node-classes --check` fails when
  `_built_in_nodes.py` differs from the runtime's built-ins, and pyright gates the callers. All
  run in CI. A new pyclass a node is handed is not done until its Protocol exists; one only the
  bootstrap uses needs its `_engine.pyi` entry.
- **A port is an `Input` or `Output` attribute of one family, and the attribute types the
  read.** `read(self.video)` narrows to the family's type (`ImageFrame`, `ChannelsBlock`, a
  `dict` for Records) and `write(self.video_out, …)` takes it. By name, `write(port_name,
  payload, timestamp_ns=None, starts_group=True)` copies any contiguous buffer and refuses
  anything else, under pyright and at runtime; `loan(port_name, payload_byte_len)` fills a
  sample in place; `read(port_name)` narrows to `PortPayloadBorrow | None`, a borrow with no
  copy, and `read(port_name, into=T)` to `T | None` through `T.from_bytes`. A Records write
  checks only that every declared name is present — a `str` where the wire wants an `int`
  reaches the runtime silently. Spell a record against the wire contract in
  `docs/plan/ARCHITECTURE.md`, never from memory.

## Non-negotiables
- All Vulkan calls live in the RHI (`runtime/streamlib-engine/src/vulkan/rhi/` +
  `runtime/streamlib-consumer-rhi/`). Nothing else touches `vulkanalia`. CI enforces.
- Logging is `tracing` only — no `println!`/`eprintln!` (CI enforces).
- macOS 26.0 is the minimum for everything we ship on macOS — every `macos-deployment-target`,
  `MACOSX_DEPLOYMENT_TARGET`, `LSMinimumSystemVersion`, and doc that names our floor. Never 15.0
  or anything lower, in this repo or in Tato Studio. A third-party binary's own `minos` and a
  dated research note stay as recorded.
- No `todo!()`/`unimplemented!()` in library code; no back-compat shims (pre-1.0).
- New Rust files carry the BUSL header, except in the vendored trees §Licensing lists, where
  a change is a recorded patch against upstream and never a drive-by edit. Never touch the
  licence files.
- Names pass the zero-context test: `LinkOutputDataWriter`, never `Writer`. Explicit beats short.
- Engine-wide defects get fixed at the engine layer, never bandaided in the consumer that
  surfaced them. Pattern migrations cover the engine tree only — consumers are never in a
  migration's scope; a broken *converted* consumer gets a Linear backlog issue, a held
  pre-pivot one just lags.
- Architecture is decided in `docs/plan/`, not per-issue. A missing decision goes to the
  owner — as a question or a stated assumption — rather than being inferred from existing
  code.
- Tests are always in scope and never need approval. Code drives tests, never the reverse.

## Before you push

- **Local gates:** spawn `dev-plugin:streamlib-local-ci-runner` — fmt, `cargo xtask check-all-source-gates`,
  clippy and nextest for the packages touched, pyright for Python under `sdk/`.
- **Review:** spawn `dev-plugin:streamlib-review-pr`, and `dev-plugin:rust-craftsmanship-reviewer` on Rust; fix every
  blocker and should-fix before the PR opens.
- **Live proof:** `/verify-live` for a GPU, camera or codec change, `/verify-audio` for an
  audio change; the report goes in the PR body.
- **PR title:** passes `scripts/check-conventional-pr-title.sh`; it or the branch names `TAT-n`.

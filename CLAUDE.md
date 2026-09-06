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
- `packages/streamlib-moq/vendor/moq-transport` — the MoQ wheel's moq-transport, MIT OR
  Apache-2.0 under Cloudflare's SPDX headers.

The exception is those paths and nothing else; BUSL is not relaxed anywhere a path is not
listed. Do not modify `LICENSE`, `LICENSES/`, or `docs/license/` without explicit approval.

---

StreamLib is a BUSL-1.1-licensed real-time streaming processing runtime like Nvidia holoscan.It is built like a game engine:
ONE core system per concern — extend the existing system, never build a parallel one. Search first.

## The router — how work happens

- All work enters through `/plan` — it reads the plan statuses and the tracker and says
  which skill is next.
- Source edits (`runtime/ sdk/ adapters/ xtask/`) belong inside `/implement` with an
  owner-confirmed ticket. Nothing prompts on one — like a doc edit, the distinction is the
  session's to apply.
- Plan *decisions* belong inside `/align`, `/propose-change`, `/ship-change`, or `/pivot`.
  Plan and doc *records* do not — see §Recording facts vs deciding. Nothing prompts on a
  doc edit; the distinction is the session's to apply, because a path guard can't see it.
- **Guardrails prompt; they never wall off.** Every path guard routes to the owner rather
  than refusing, so a scope written months ago can't strand work that has to land. The
  doctrine still decides what is *right* — a prompt is not permission to bend a rule.
  Guards cover only what is genuinely hard to reverse — the licence files. They are not a
  review queue for prose. The `rig-brake` hook is advisory: it notes a rig-consuming command
  to the model and to the owner, and it prompts only where the owner set a rule or a glob to
  `ask` in `.claude/rig-brake.json` or its local / user-level siblings
  (`.claude/scripts/rig-brake` edits them).
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

## Recording facts vs deciding

Writing down what is true is ordinary work. Deciding what we build is not. The lifecycle
skills exist to sharpen decisions, not to license prose.

- **A factual record needs no skill, no prompt, and no approval.** What a shipped change
  removed, renamed, or proved — a `REMOVED:` bullet, a superseded claim, a corrected file
  anchor, a stale reference, a doc describing code that no longer exists — is part of
  finishing the work. Write it in the same PR as the change it describes. Leaving the
  record wrong to avoid ceremony is the worse outcome, always.
- **A decision goes through the lifecycle.** Adding, retracting or reversing a `DECIDED` /
  `OPEN` entry, changing what we agreed to build, or settling a question the plan leaves
  open belongs in `/align`, `/propose-change`, `/ship-change`, or `/pivot`.
- The test: **could a careful reader derive it from the diff and the tree?** Then it is a
  fact — write it, and say in the PR body that you did. Does it commit us to something we
  have not agreed? Then it is a decision — bring it.
- Doc hygiene inside work you are already doing — fixing a claim your own change falsified —
  is never a separate ticket and never a question.

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
  assumed and where it would bite, and let the owner correct it. Reserve blocking for cases
  where proceeding either way is unsafe or would waste the work.
- **Batch** what genuinely must be asked into one round at the point of decision — never a
  drip of one-question stops.
- A finding is not a question. Non-blocking findings go in the PR description, then move on.

## Runtime-first, plan-first (MVP doctrine)

The runtime is the product; consumers lag behind it by design (the Holoscan / Next.js model:
framework repo first, holohub/examples follow releases). Architecture lives in ONE place —
`docs/plan/ARCHITECTURE.md` plus `docs/plan/architecture.excalidraw` — agreed with the owner
before implementation. Sessions implement the plan; they never make architecture.

- `examples/` and the consumer entries in `packages/` (everything except `test-fixtures`) are
  downstream consumers, **not contract sources** — converted or not. Never read one to infer what
  the engine guarantees; never bend a runtime design to fit their existing patterns. Contracts
  are stated in the engine and proven by engine tests and fixtures. Disposition per directory is
  decided in `docs/plan/ARCHITECTURE.md` §Consumers.
- **A converted consumer** — current-idiom shape: a scaffolded app (`app.py` + `pyproject.toml`)
  or an ordinary Python package — **is the model of the current authoring idiom and ordinary
  editable work.** An engine change that breaks one files tracked backlog at that consumer and
  never blocks the engine change; fixing it in-stream is reserved for the rare case where the
  consumer is the deliberate canary of the in-flight work.
- File an issue only when something blocks the current milestone. Non-blocking findings go in the
  PR description as a note, then we move on. Getting an MVP into users' hands beats completeness.
- **A held pre-pivot consumer** (`Cargo.toml` + `setup.sh` shape) keeps the old treatment:
  written against deleted machinery — the identity grammar, the schema layer, `streamlib.yaml`
  manifests, the package-as-distributable shape — so read it for *logic* only (how a codec was
  wired to the RHI, what a capture path must handle), never for form; breakage is expected, not
  a defect, and gets no ticket. It converts or deletes only through §Consumers' rules, never in
  passing.

Captured knowledge lives in `docs/learnings/`; design rationale in `docs/decisions/`. However, these may go stale and should be verified, not viewed as facts. It serves as a cache. Everything else is re-derived
from code at need — do not create summary docs of what code already shows.

## Reading the Python surface

`sdk/streamlib-python-wheel/python/streamlib/_engine.pyi` is the reference for everything the
wheel exports. The hand-written stub is where a built-in's config shape, its port names, and
each context method's contract are actually written down — read it before reading Rust. A
detour into `runtime/` to learn what a built-in publishes on means you skipped it.

- **The docs are stub-only.** A compiled class's runtime `__doc__` is a one-liner and `dir()`
  on it is empty, so `help()` under-reports the surface badly. Never conclude from a REPL that
  something is undocumented.
- **The stub cannot drift.** `stubtest` gates it against the real binary and pyright gates the
  callers, both in CI. A new pyclass is not done until its stub entry exists.
- **Typing is load-bearing on the read side and absent on the write side.** `read(port,
  into=T)` narrows to `T | None` and catches a wrong attribute; `write(port, bag)` takes
  `Mapping[str, Any]` and catches nothing — a typo'd key, a `str` where the wire wants an
  `int`, and a missing required key all reach the runtime silently. Spell a bag literal
  against the wire contract in `docs/plan/ARCHITECTURE.md`, never from memory.

## Non-negotiables
- All Vulkan calls live in the RHI (`runtime/streamlib-engine/src/vulkan/rhi/` +
  `runtime/streamlib-consumer-rhi/`). Nothing else touches `vulkanalia`. CI enforces.
- Logging is `tracing` only — no `println!`/`eprintln!` (CI enforces).
- No `todo!()`/`unimplemented!()` in library code; no back-compat shims (pre-1.0).
- New Rust files carry the BUSL header, except in the vendored trees §Licensing lists, where
  a change is a recorded patch against upstream and never a drive-by edit. Never touch the
  licence files.
- Names pass the zero-context test: `LinkOutputDataWriter`, never `Writer`. Explicit beats short.
- Engine-wide defects get fixed at the engine layer, never bandaided in the consumer that
  surfaced them. Pattern migrations cover the engine tree only — consumers are never in a
  migration's scope; a broken *converted* consumer gets backlog filed, a held pre-pivot one
  just lags.
- Architecture is decided in `docs/plan/`, never per-ticket. A missing decision stops work and
  goes to the owner; it is never inferred from existing code.
- Tests are always in scope and never need approval. Code drives tests, never the reverse.


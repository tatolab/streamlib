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
- Source edits (`runtime/ sdk/ adapters/ xtask/`) belong inside `/implement` with an
  owner-confirmed ticket — the hook prompts via `.claude/state/active-ticket.json`.
- Plan *decisions* belong inside `/align`, `/propose-change`, `/ship-change`, or `/pivot`.
  Plan and doc *records* do not — see §Recording facts vs deciding. Nothing prompts on a
  doc edit; the distinction is the session's to apply, because a path guard can't see it.
- **Guardrails prompt; they never wall off.** Every path guard routes to the owner rather
  than refusing, so a scope written months ago can't strand work that has to land. The
  doctrine still decides what is *right* — a prompt is not permission to bend a rule.
  Guards cover what is genuinely hard to reverse — source without a ticket, consumer
  trees, licence files. They are not a review queue for prose.
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
- They are the pre-pivot tree, **not a model for new code**. Each is written against the
  deleted identity grammar, the deleted schema layer, `streamlib.yaml` manifests and the
  package-as-distributable shape the wheel replaced — so reading one to learn how a processor
  is declared teaches the model we removed. Their *logic* still holds (how a codec was wired to
  the RHI, what a capture path must handle); read for that, never for form. Editing stays
  deny-ruled.

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


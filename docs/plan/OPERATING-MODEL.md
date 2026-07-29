# StreamLib Operating Model (adopted 2026-07-29)

The system that replaces per-ticket architecture, the legacy docs sprawl, and agent
self-decision. Three layers: **Truth** (one living plan), **Skills** (every operation
prescribed), **Enforcement** (gates, not vibes). Grounded in three research passes:
the mattpocock/skills catalog, this repo's own lifecycle inventory, and the 2026
spec-driven-development literature (spec-kit, Kiro, OpenSpec, measured failure modes).

Design constraints this must satisfy, from evidence in this repo:

- The loop retirement (#1650) proved **owner review is the bottleneck, not agent
  throughput**. Nothing here adds parallel agent passes; several things remove review
  surfaces.
- The docs system's failure is **absent ownership boundaries**, not staleness: six docs
  describe the package system with no division of labor, so supersessions land in one doc
  while diagrams three hundred lines up still show the old verb.
- Agents fight pivots (the node-modules fight) because **nothing legitimizes deletion**.
  The model needs a first-class rip-out mechanism, not more restraint rules.
- The measured gain from heavyweight spec ceremony is small (+3% composite in the one
  controlled study). Adopt the **artifact model** aggressively, the **ceremony** minimally.

---

## Layer 1 — Truth: what replaces the docs system

```
docs/plan/PRODUCT & ARCHITECTURE  (ARCHITECTURE.md)   ONE doc. The agreed system.
docs/plan/GLOSSARY.md                                 Ubiquitous language. Terms only.
docs/plan/diagrams/*.mmd                              Mermaid sources → generated .excalidraw
docs/plan/changes/<name>.md                           In-flight deltas (≤200 lines each)
docs/plan/changes/archive/YYYY-MM-DD-<name>/          Shipped deltas, folded in
docs/decisions/                                       ADRs, append-only (kept as-is)
docs/learnings/                                       Empirical cache (kept as-is)
docs/architecture/                                    RETIRED — distilled into the plan
```

### ARCHITECTURE.md — the single living document

Every section carries a status marker. This is the visibility mechanism: one glance shows
current state, target state, and what's in flight — the "current + goal + tickets"
linkage in one artifact.

- **SHIPPED** — true in the tree today. Carries a `<!-- verify: <glob or command> -->`
  marker so drift is detectable.
- **IN-FLIGHT (→ change-name)** — being built; links the delta in `changes/` and its
  tickets.
- **DECIDED** — agreed, not started. Build exactly this.
- **OPEN** — undecided. Building against it is forbidden; work stops and escalates.

Rules that keep it alive (from what survives agent maintenance vs. what rots):

1. **No rationale in the plan.** The doc may not argue "because" — rationale goes to an
   ADR and the plan links `[ADR-NNN]`. Rationale is what makes a doc un-updatable.
2. **No restating what code shows.** C4 levels 1–3 only (context / container / component);
   never code-level detail.
3. **Staleness is detected, never auto-repaired.** `/audit-drift` reports sections whose
   verify-target changed; a human-approved change fixes them.

### Two truths, one reconciliation

**Code is the authority on what IS. The plan is the authority on what we AGREED.**
Neither replaces the other:

- A **SHIPPED** section is a claim about code — when code says otherwise, the code is
  right about reality and the drift is the finding: either the code regressed (a bug
  ticket) or the agreement moved silently (an `/align` session). The owner picks which
  one moves; nothing auto-repairs.
- **DECIDED / IN-FLIGHT sections and changes** are claims about intent — code cannot
  contradict them, only lag them.
- The reconciliation instruments are `/snapshot-architecture` (the code-derived view,
  with a drift table against the plan) and `/audit-drift` (verify-marker staleness).
  The snapshot is descriptive and regenerated at will; it is never the decision source.

### Diagrams — Mermaid source, Excalidraw view

Excalidraw JSON is not agent-maintainable as a source of truth (opaque
`version`/`versionNonce`/`seed` fields, hand-computed coordinates, binding graphs that
dangle silently, unreviewable PR diffs). The pipeline instead:

- `docs/plan/diagrams/*.mmd` — Mermaid flowcharts, the committed source. Renders natively
  in GitHub issues/PRs and in the plan doc. Agent-editable, PR-reviewable.
- Generated `.excalidraw` exports via `@excalidraw/mermaid-to-excalidraw` (official
  library; flowcharts convert to native elements) — for whiteboarding and hand
  annotation. **Never round-tripped back.**
- If the top-level system diagram outgrows Mermaid layout (~25+ nodes), that one diagram
  graduates to D2 with an SVG build step. Recorded here so it isn't re-litigated.

### Changes — typed deltas, archived on merge

A change proposal (`changes/<name>.md`) is written as a **delta against the plan**, never
a restatement: sections marked `ADDED` / `MODIFIED` / `REMOVED`, ≤200 lines, deriving
≤5 tickets. Two markers with different powers:

- `[NEEDS CLARIFICATION]` — a factual gap; the agent resolves it by reading the repo.
- `[NEEDS DECISION]` — an architectural choice; the agent may NOT resolve it. It stops,
  states the options with a recommendation, and waits. This is the enforcement mechanism
  for "agents never decide architecture" — a prohibition alone fails; agents need a legal
  move to make instead.

When a change's tickets all merge, `/ship-change` folds the delta into ARCHITECTURE.md,
flips section statuses, and archives the file — gated: **anything marked REMOVED must no
longer grep in the tree, or the archive fails.** Half-migrations stop being a discipline
problem and become a blocked gate. This is the mechanism that prevents another
half-broken old-module-system.

### Tracker mapping — everything is architecture-driven

The GitHub tracker is a projection of the plan, never a second source:

- A **milestone** is a plan section or an active change — nothing else may be a milestone.
- A **ticket** exists only as (a) output of `/derive-tickets` from an approved change, or
  (b) a bug filed against SHIPPED behavior. A ticket that traces to neither is a
  candidate for closure.
- Tickets are living documents: when a plan section changes, `/reconcile-tracker`
  revises or closes the affected tickets in one owner-approved batch. Today's misaligned
  milestones and stale tickets go through the same skill as its first run.

### What happens to the existing 6,624 lines of docs/architecture/

Retired by consolidation change (its own tracked change with tickets, not a side effect):

| Cluster | Today | Becomes |
|---|---|---|
| Package/module system (6 docs, 2,367 L, densest supersession zone) | competing, unowned | ARCHITECTURE.md §Module system + at most one reference doc |
| Surface adapters (4 docs, 1,931 L) | overlapping | §Adapters + one authoring reference |
| RHI kernels (5 docs, one identical skeleton) | five copies of one template | one reference doc, five sections |
| Plugin ABI / cdylib (3 docs, 1,425 L) | mixed state+rationale | §Plugin ABI; `cdylib-reachability.md`'s decision-tree content moves to an ADR |
| `vendored-vulkanalia.md`, logging pair | fine | kept |
| Root `README.md` | worst-rotted doc in the tree | rewritten against the plan |

The five `agent-knowledge/` symptom indexes keep working: their rows point mostly at
`docs/learnings/` (kept); rows pointing at retired architecture docs are re-pointed in
the same consolidation change.

---

## Layer 2 — Skills: every operation prescribed

Structure copied from the pattern that works in mattpocock/skills: a few **model-invoked
primitives** composed by thin **user-invoked skills**, plus a **router** so nobody has to
memorize the catalog. Root virtue (their phrasing, adopted verbatim): *"a skill exists to
wrangle determinism out of a stochastic system — predictability of process, not output."*

### Primitives (model-invoked, the vocabulary underneath)

| Skill | What it prescribes |
|---|---|
| `grilling` | Relentless interview, ONE question at a time, each with a recommended answer. Facts are looked up in the repo, never asked; decisions are the owner's, never assumed. No action until shared understanding is confirmed. |
| `batch-grilling` | The frontier variant (per batch-grill-me): map the decision tree, ask the whole frontier each round — every question whose prerequisites are settled — numbered, each with a recommendation. Recompute after answers. Done when the frontier is empty. **This is the plan-session engine.** |
| `glossary` | Maintains GLOSSARY.md inline as terms crystallize. Challenges drift ("you defined X as…, but you seem to mean Y"). Zero implementation details — a glossary and nothing else. |
| `module-design` | The deep-modules vocabulary (module / interface / depth / seam / adapter, used exactly) + design-it-twice: 3 parallel design agents with different constraints, compared on depth/locality/seam placement, ending with one opinionated recommendation — never a menu. |

### User-invoked (the lifecycle, in order)

| Skill | Procedure (freedom level) | Replaces |
|---|---|---|
| `/plan` | Router. Reads ARCHITECTURE.md statuses + open changes + ticket frontier, says where you are and which skill is next. (reference, no procedure) | remembering the catalog |
| `/align` | `batch-grilling` + `glossary` over one plan section → OPEN→DECIDED edits + diagram update. Nothing else moves. (high freedom) | the debate you want to have once |
| `/propose-change` | Read-only recon by domain experts first (the measured fix for context-blind specs) → delta proposal with `[NEEDS DECISION]` blocks → **stop for owner approval**. (medium) | `draft-design` |
| `/derive-tickets` | Approved change → ≤5 tracer-bullet tickets: vertical slices, each demoable, each sized to one context window, blocking edges declared; wide refactors sequenced expand→migrate-in-batches→contract. Quiz the owner on the list until approved, then publish. (medium) | `file-issue` for planned work (kept for bugs) |
| `/implement` | Load ticket + its change + plan section → **plan gate**: any needed decision not DECIDED ⇒ stop with `[NEEDS DECISION]` → announce plan, owner confirms → build test-first at pre-agreed seams → gate battery via `local-ci-runner` → one review pass → PR (`Closes #N` per line). (low freedom at the gates, normal freedom in the code) | the external `amos-next` protocol, brought in-tree |
| `/ship-change` | Fold delta into plan, flip statuses, REMOVED-grep gate, regenerate diagrams + Excalidraw export, archive. Exact scripts, no prose latitude. (lowest freedom) | nothing — the missing piece |
| `/pivot` | Owner declares a direction change → plan edited FIRST → inventory of now-legacy code/docs/rules → a rip-out change with REMOVED sections → deletion tickets. (medium) | nothing — legitimizes the deletion agents currently resist |
| `/research` | Background agent, primary sources only, memo with citations. Produces no tickets and no code. (low) | ad-hoc research |
| `/diagnose` | Feedback-loop-first debugging: no hypothesis before a red-capable, fast, deterministic repro command exists and has been run once. 3–5 ranked hypotheses before testing any. Regression test before fix. Tagged debug logs (`[DEBUG-xxxx]`) for one-grep cleanup. (medium) | ad-hoc debugging; composes with the domain experts |
| `/audit-drift` | Verify-marker staleness report + dangling doc links + stale rule-path globs. Detection only, never repairs. (low) | nothing |
| `/reconcile-tracker` | Audits every GitHub milestone and open ticket against the plan: a milestone must map to a plan section or an active change; a ticket must trace to a change or be a bug against SHIPPED behavior. Anything that doesn't trace gets a proposed action — close / retitle / re-milestone / rewrite — presented as ONE batch the owner approves as a list, then executed via `gh`. Never acts item-by-item without the approved batch. (medium) | manual milestone/ticket cleanup |
| `/propose-rule` | The only way a rule is born or dies. Evidence in (a recurring review finding, a repeated owner correction, a shipped defect) → drafted rule text + the evidence + which existing rule or skill gate it overlaps → owner approves → lands in its own operating-model PR. Also proposes rule deletions when a skill gate supersedes prose. (low) | ad-hoc rule accretion |

### Understanding & convergence (the shared-language layer)

These four exist so the owner and Claude always hold an identical picture — every one
ends with a say-back loop, and none of them commits work:

| Skill | Procedure | Purpose |
|---|---|---|
| `/snapshot-architecture` | Read-only code survey → ONE living Claude Artifact (Mermaid diagrams, [VERIFIED file:line] vs [INFERRED] tags, drift table vs the plan, open questions) → redeployed to the same URL forever | The always-current picture of what the code actually is, from Claude's point of view, standing ready for correction |
| `/architecture-question` | Verify in code first → answer in labeled layers ([CODE] / [PLAN] / [INFERRED]) → surface drift found on the way → say-back close | "How does X work", answered with evidence, never from memory |
| `/reconcile-understanding` | Say-back gate → hunt the wrong belief in every home (memories, docs, plan, glossary, skills, snapshot, tickets) → one owner-approved fix batch → correction saved as a memory | Corrections that stick across sessions instead of evaporating |
| `/explore-idea` | Situate (incl. reverse-engineering a messy milestone's intent) → sharpen the fuzz via `grilling` → sketch 2–3 shapes with unknowns + cost class → **recommend the smallest start on the build ladder** (spike / prototype / MVP slice / full change) with an explicit deferred list and iteration path → say-back → explicit exit: graduate scoped to the starting rung, park with a memo, or kill with the reason | The sandbox for what-ifs too fuzzy for a change proposal — burns down unknowns AND right-sizes the entry point, so a weekend idea never silently becomes a quarter |

### Kept, consolidated, retired

- **Kept unchanged:** the nine live-ops CLI skills (`discover-running-nodes` …
  `teardown-running-node`) — already perfectly shaped (one skill = one CLI verb);
  `gh-stack`; `local-ci-runner`; `rust-craftsmanship-reviewer`; the five domain experts
  (with two charter fixes: `polyglot-ipc-expert` still mandates the repealed
  Python+Deno-together rule; `package-source-expert` aligns to the plan's module-system
  entry).
- **Consolidated:** `pr-review-gate` + `change-verifier` overlap heavily (both check test
  lock-in, scope, boundaries, naming) and each PR currently runs up to four review
  lenses. Owner review is the bottleneck — one merged `review-pr` lens (plus
  `rust-craftsmanship-reviewer` as the quality lens) replaces them.
- **Reconciled:** `verify-live` (loop runs the pipeline) vs `evidence-verifier` (never
  runs the pipeline) state opposite primary modes. One is chosen; the other's charter is
  rewritten to match. [NEEDS DECISION — recommend LOOP-RUN primary, handshake fallback,
  matching `verify-live`.]
- **Retired:** `draft-design` (premise — per-issue design — is now forbidden by
  docs-policy), `file-issue` for planned work, the external `amos-next` protocol
  (already broken in four places against this repo: deleted `.claude/workflows/` refs,
  repealed sweep step, drifted rule quotes, missing feedback files). The amos CLI
  survives as the graph/focus data layer that `/plan` and `/implement` read.

---

## Layer 3 — Enforcement

Nothing here reintroduces the retired automation loop — loop references in this document
are historical evidence only. The system is single-session, owner-gated, skill-routed.
And it lives entirely in-repo under `.claude/` (no user-level skills, no external repos),
so **every agent that opens this repo runs the same system**, regardless of who launched
it or how.

### Skill invocation is enforced, not hoped for

A model can't be *forced* to invoke a skill — so the system makes the side effects of
skipping one physically fail, in layers from soft to hard:

1. **Router text in CLAUDE.md** (always loaded): all work enters through `/plan`; source
   edits happen only inside `/implement` with a confirmed ticket; plan edits happen only
   inside `/align`, `/propose-change`, `/ship-change`, or `/pivot`.
2. **Sharp descriptions** on the model-invoked primitives so they trigger on phrasing
   without being asked.
3. **Hooks** (the hard layer): a PreToolUse hook rejects `Edit`/`Write` under `runtime/`,
   `sdk/`, `tools/`, `adapters/`, `xtask/` unless `.claude/state/active-ticket.json`
   exists — a marker only `/implement` writes, only after the owner confirms the
   announced plan. Skipping `/implement` doesn't produce sloppy work; it produces a
   blocked edit. (Owner escape hatch: create the marker by hand.)
4. **CI backstop**: the PR body must reference a ticket; `review-pr` flags any new public
   trait / module / cross-crate boundary the change proposal doesn't name.

### The plan is locked

Agents cannot quietly rewrite the decision source:

- `settings.json` puts `Edit(docs/plan/**)` on the **ask** list — every plan write
  surfaces a permission prompt the owner personally approves, in session, per edit.
- The PreToolUse hook additionally rejects plan writes unless one of the four
  plan-editing skills has set its marker — so even an approved edit can only happen
  inside `/align`, `/propose-change`, `/ship-change`, or `/pivot`.
- Git layer: `CODEOWNERS` on `docs/plan/**` + branch protection — no plan change merges
  without the owner's review, even from a session the owner wasn't watching.

### Rules have a lifecycle

Rules under `.claude/rules/` shrink to **invariants only** (licensing, naming, RHI
boundary, plugin ABI, comments). Process prose migrates into skill gates, which are
testable; a rule that a skill gate now enforces gets deleted via `/propose-rule`. New
rules enter only through `/propose-rule` — evidence, draft, owner approval, dedicated
PR — never accreted mid-session because something annoyed an agent once.

### Mechanical gates

- The deny rules on consumers (landed 2026-07-29) — agents cannot read or edit
  distributables and examples.
- `[NEEDS DECISION]` as a hard stop in `/propose-change` and `/implement`.
- The REMOVED-grep gate in `/ship-change` — scripts under `.claude/scripts/`, not prose.
- `review-pr` gains one check: any new public trait, module, or cross-crate boundary in
  the diff that the change proposal doesn't name is a finding. This makes "no inline
  architecture" enforceable rather than aspirational.
- The existing 14 xtask gates + lefthook battery, unchanged.
- `flow.md` unchanged: operating-model changes ship as their own PR; a session never
  edits the skills it is running.

## Numeric caps (hard numbers survive agent interpretation; prose doesn't)

- Change proposal ≤ 200 lines. Tickets per change ≤ 5 — exceeding means split the change.
- Scale gate, decided at entry: bug fix / refactor / test work → **no change artifact at
  all** (the default path); new behavior or changed contract → delta change; anything
  touching plugin ABI, RHI, IPC wire format, or the processor model → delta + ADR.

## Rollout (MVP-first; each wave usable before the next starts)

1. **Commit the pivot** — today's uncommitted rule/CLAUDE.md/settings edits + this
   document, one operating-model PR. (`docs/plan/` is currently untracked; nothing
   enforces a plan that isn't in git.)
2. **Land #1672** — consumers out; kills the build coupling.
3. **Wave 1 skills** — `grilling`, `batch-grilling`, `glossary`, `/align`, `/plan`:
   just enough to run the plan session.
4. **The plan session** — `/align` on §Product first (the MVP sentence), then module
   system, processor model, SDK parity, in that order. Everything after this is execution.
5. **Wave 2 skills** — `/reconcile-tracker` first (clean today's misaligned milestones
   and tickets against the fresh plan), then `/propose-change`, `/derive-tickets`,
   `/implement`, `review-pr` consolidation: the first real change flows through them.
6. **Wave 3** — `/ship-change`, `/pivot`, `/audit-drift`, then the docs-consolidation
   change (retiring `docs/architecture/`) as the first big change run through the new
   system — the system migrates the old docs, proving itself on its own bootstrap.

## Decisions for owner

1. The MVP sentence (§Product) — first item of the plan session; everything traces to it.
2. Consolidate the four review lenses into `review-pr` + craftsmanship — yes/no.
3. `verify-live` vs `evidence-verifier` primary mode — recommend session-runs-the-pipeline
   primary (today's "LOOP-RUN" vocabulary gets renamed `self-run`; no relation to the
   retired loop), owner-terminal handshake as fallback.
4. The three edit-denied docs (`logging-schema.md`, `testing-hardware.md`,
   `schema-identity-and-packaging.md`) — the last has four supersession blocks and can
   only rot while frozen. Unfreeze into the consolidation, or keep frozen?
5. Parked ticket #1624 (retire `.slpkg` for plain `.zip`) — becomes a plan decision in
   §Distribution; ~650 occurrences hang on it.
6. Bring the ticket lifecycle in-tree (replace external amos-next protocol with
   `/implement`) — recommend yes; it is the single highest-leverage move the inventory
   found.

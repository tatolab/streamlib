# StreamLib Operating Model (adopted 2026-07-29)

The system that replaces per-ticket architecture, the legacy docs sprawl, and agent
self-decision. Three layers: **Truth** (one living plan), **Skills** (how the owner likes
each operation reasoned about and decomposed — descriptive, never a gate), **Guardrails**
(CI, review and the owner's GitHub review — nothing that blocks or pauses an agent, a write to a GitHub issue excepted). Grounded in three research passes:
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
docs/plan/changes/<name>.md                           In-flight deltas (≤350 lines each)
docs/plan/changes/archive/YYYY-MM-DD-<name>/          Shipped deltas, folded in
docs/decisions/                                       ADRs, append-only (kept as-is)
docs/learnings/                                       Empirical cache (kept as-is)
docs/architecture/                                    RETIRED — distilled into the plan
```

### ARCHITECTURE.md — the single living document

Every section carries a status marker. This is the visibility mechanism: one glance shows
current state, target state, and what's in flight — the "current + goal + issues"
linkage in one artifact.

- **SHIPPED** — true in the tree today.
- **IN-FLIGHT (→ change-name)** — being built; links the delta in `changes/`, which its
  Linear project builds.
- **DECIDED** — agreed, not started. Build exactly this.
- **OPEN** — undecided. The decision is the owner's: work raises it (a `[NEEDS DECISION]`
  or a stated assumption) rather than settling it silently, and keeps going on what
  doesn't depend on it.

Rules that keep it alive (from what survives agent maintenance vs. what rots):

1. **No rationale in the plan.** The doc may not argue "because" — rationale goes to an
   ADR and the plan links `[ADR-NNN]`. Rationale is what makes a doc un-updatable.
2. **No restating what code shows.** C4 levels 1–3 only (context / container / component);
   never code-level detail.
3. **Staleness is detected, then fixed where it belongs.** `/hygiene` reports sections
   whose verify-target changed; the fix lands in a PR the owner reviews.

### Two truths, one reconciliation

**Code is the authority on what IS. The plan is the authority on what we AGREED.**
Neither replaces the other:

- A **SHIPPED** section is a claim about code — when code says otherwise, the code is
  right about reality and the drift is the finding: either the code regressed (a Linear
  `Bug` issue) or the agreement moved silently (an `/align` session). The owner picks which
  one moves; nothing auto-repairs.
- **DECIDED / IN-FLIGHT sections and changes** are claims about intent — code cannot
  contradict them, only lag them.
- The reconciliation instruments are `/snapshot-architecture` (the code-derived view,
  with a drift table against the plan) and `/hygiene` (the full counter-model sweep:
  doc/plan contradictions, retired-pattern endorsements, dead references, README rot).
  The snapshot is descriptive and regenerated at will; it is never the decision source.

### Diagrams — Mermaid source, Excalidraw view

Excalidraw JSON is not agent-maintainable as a source of truth (opaque
`version`/`versionNonce`/`seed` fields, hand-computed coordinates, binding graphs that
dangle silently, unreviewable PR diffs). The pipeline instead:

- `docs/plan/diagrams/*.mmd` — Mermaid flowcharts, the committed source. Renders natively
  in GitHub PRs and in the plan doc. Agent-editable, PR-reviewable.
- Generated `.excalidraw` exports via `@excalidraw/mermaid-to-excalidraw` (official
  library; flowcharts convert to native elements) — for whiteboarding and hand
  annotation. **Never round-tripped back.**
- If the top-level system diagram outgrows Mermaid layout (~25+ nodes), that one diagram
  graduates to D2 with an SVG build step. Recorded here so it isn't re-litigated.

### Changes — typed deltas, archived on merge

A change proposal (`changes/<name>.md`) is written as a **delta against the plan**, never
a restatement: sections marked `ADDED` / `MODIFIED` / `REMOVED`, ≤350 lines, deriving
as few tracer-bullet issues as it honestly needs (count is guidance, not a cap —
owner decision 2026-08-02). Two markers with different powers:

- `[NEEDS CLARIFICATION]` — a factual gap; the agent resolves it by reading the repo.
- `[NEEDS DECISION]` — an architectural choice the owner wants to make. The agent states
  the options with a recommendation and keeps working on what doesn't depend on it. This
  gives "the owner decides architecture" a concrete move to make instead of a
  prohibition.

When every issue of a change's Linear project is Done, `/ship-change` folds the delta into ARCHITECTURE.md,
flips section statuses, and archives the file, after checking that **anything marked
REMOVED no longer greps in the tree** (dev-plugin's `scripts/ship-change-removed-gate.sh`).
Residue it finds is unfinished work, finished before the archive claims the removal. This
is what prevents another half-broken old-module-system.

### Tracker mapping — everything is architecture-driven

Work is tracked in Linear (team Tatolab, issues `TAT-n`), and Linear builds the plan, never
restates or extends it:

- A **project** is a goal, usually one active change; its **milestones** are the phases,
  and the projects and milestones are the order of work.
- An **issue** is a piece of work `/plan-project` laid out from a change, or one
  `/file-issue` filed: a bug against SHIPPED behaviour, or a small feature or improvement
  that needs no change. One that lands in this repo is titled `streamlib: …`; a change
  spanning repos is a parent issue with one sub-issue per repo. Order between issues is a
  blocking relation.
- A PR links to its issue through the `TAT-n` in its branch name or title, and Linear moves
  the issue to In Review when the PR opens and to Done when it merges.
- GitHub Issues is the public front door only. Linear's sync copies each report into
  Triage, `/triage` works that inbox, and an agent never writes to a GitHub issue.

### What happens to the existing 6,624 lines of docs/architecture/

Retired by consolidation change (its own tracked change with tickets, not a side effect):

| Cluster | Today | Becomes |
|---|---|---|
| Package/module system (6 docs, 2,367 L, densest supersession zone) | competing, unowned | removed outright by the rip-out change, with the module system |
| Surface adapters (4 docs, 1,931 L) | overlapping | §Adapters + one authoring reference (ABI halves deleted by the pivot; consolidate what survives) |
| RHI kernels (5 docs, one identical skeleton) | five copies of one template | one reference doc, five sections |
| Plugin ABI / cdylib (3 docs, 1,425 L) | mixed state+rationale | removed outright by the rip-out change, with the plugin ABI |
| `vendored-vulkanalia.md`, logging pair | fine | kept |
| Root `README.md` | worst-rotted doc in the tree | rewritten against the plan |

The five `agent-knowledge/` symptom indexes keep working: their rows point mostly at
`docs/learnings/` (kept); rows pointing at retired architecture docs are re-pointed in
the same consolidation change.

---

## Layer 2 — Skills: how the owner reasons and decomposes work

Structure copied from the pattern that works in mattpocock/skills: a few **primitives**
composed by thin **lifecycle skills**, plus a **router** so nobody has to memorize the
catalog. Every skill can be invoked by the model or the owner. A skill describes the
owner's usual way of doing an operation and why each step exists; the agent is free to
adapt or skip steps when the situation calls for it. The skill carries the reasoning; the
agent carries the judgment.

### Primitives (the vocabulary underneath)

| Skill | What it prescribes |
|---|---|
| `grilling` | Relentless interview, ONE question at a time, each with a recommended answer. Facts are looked up in the repo, never asked; decisions are the owner's, never assumed. No action until shared understanding is confirmed. |
| `grilling`, batch mode | The frontier variant (per batch-grill-me): map the decision tree, ask the whole frontier each round — every question whose prerequisites are settled — numbered, each with a recommendation. Recompute after answers. Done when the frontier is empty. **This is the plan-session engine.** |
| `glossary` | Maintains GLOSSARY.md inline as terms crystallize. Challenges drift ("you defined X as…, but you seem to mean Y"). Zero implementation details — a glossary and nothing else. |
| `module-design` | The deep-modules vocabulary (module / interface / depth / seam / adapter, used exactly) + design-it-twice: 3 parallel design agents with different constraints, compared on depth/locality/seam placement, ending with one opinionated recommendation — never a menu. |

### The lifecycle, in its usual order

| Skill | Procedure (freedom level) | Replaces |
|---|---|---|
| `/whats-next` | Router. Reads Linear's projects and milestones and the open PRs, says where you are and which issue is next. (reference, no procedure) | remembering the catalog |
| `/align` | `grilling` in batch mode + `glossary` over one plan section → OPEN→DECIDED edits + diagram update. Nothing else moves. (high freedom) | the debate you want to have once |
| `/propose-change` | Read-only recon by domain experts first (the measured fix for context-blind specs) → delta proposal with `[NEEDS DECISION]` blocks → presented for the owner's approval before `/plan-project`. (medium) | `draft-design` |
| `/plan-project` | Approved change → a Linear project with milestones and tracer-bullet issues (as few as honestly needed; count is guidance, not a cap): vertical slices, each demoable, each sized to one context window, order as blocking relations; wide refactors sequenced expand→migrate-in-batches→contract. (medium) | `file-issue` for planned work (kept for single issues) |
| `/implement` | Load the Linear issue and its comments + its change + plan section → any needed decision not DECIDED is raised as `[NEEDS DECISION]` (a small, easily reversed one may be built as a stated assumption instead) → share the plan → build test-first at the named seams → fast local checks via `local-ci-runner` → review pass → PR whose branch name or title carries the `TAT-n`. (normal freedom) | the external `amos-next` protocol, brought in-tree |
| `/ship-change` | Fold delta into plan, flip statuses, REMOVED-grep check, regenerate diagrams + Excalidraw export, archive. Script-backed. (low freedom — the removal check is the point) | nothing — the missing piece |
| `/pivot` | Owner declares a direction change → plan edited FIRST → inventory of now-legacy code/docs/rules → a rip-out change with REMOVED sections → its Linear project of deletion issues. (medium) | nothing — legitimizes the deletion agents currently resist |
| `/research` | Background agent, primary sources only, memo with citations. Produces no issues and no code. (low) | ad-hoc research |
| `/diagnose` | Feedback-loop-first debugging: a red-capable, fast, deterministic repro command, run once, before building theories — or, when one genuinely can't be built, the access or artifact it would need named and the best available evidence used. 3–5 ranked hypotheses before testing any. Regression test before fix. Tagged debug logs (`[DEBUG-xxxx]`) for one-grep cleanup. (medium) | ad-hoc debugging; composes with the domain experts |
| `/hygiene` | The counter-model sniffer: doc statements contradicting DECIDED/SHIPPED plan entries, retired patterns still endorsed, references to deleted machinery, README rot, dead rule globs, stale memories. ONE table ordered by thrash risk, each finding routed to where it's best fixed. (low) | nothing — the missing smell test |
| `/triage` | Works Linear's Triage inbox of public reports synced from GitHub, and grooms the projects so issues match the code and the plan. (medium) | manual tracker cleanup |
| `/propose-rule` | How a rule is born or dies. Evidence in (a recurring review finding, a repeated owner correction, a shipped defect) → drafted rule text + the evidence + which existing rule or skill it overlaps → owner agrees → lands in its own operating-model PR. Also proposes rule deletions when a skill or CI check supersedes prose. (low) | ad-hoc rule accretion |

### Understanding & convergence (the shared-language layer)

These four exist so the owner and Claude always hold an identical picture — every one
ends with a say-back loop, and none of them commits work:

| Skill | Procedure | Purpose |
|---|---|---|
| `/snapshot-architecture` | Read-only code survey → ONE living Claude Artifact (Mermaid diagrams, [VERIFIED file:line] vs [INFERRED] tags, drift table vs the plan, open questions) → redeployed to the same URL forever | The always-current picture of what the code actually is, from Claude's point of view, standing ready for correction |
| `/architecture-question` | Verify in code first → answer in labeled layers ([CODE] / [PLAN] / [INFERRED]) → surface drift found on the way → say-back close | "How does X work", answered with evidence, never from memory |
| `/reconcile-understanding` | Say-back → hunt the wrong belief in every home (memories, docs, plan, glossary, skills, snapshot, Linear issues) → one fix batch the owner can strike lines from → correction saved as a memory | Corrections that stick across sessions instead of evaporating |
| `/explore-idea` | Situate (incl. reverse-engineering a messy project's intent) → sharpen the fuzz via `grilling` → sketch 2–3 shapes with unknowns + cost class → **recommend the smallest start on the build ladder** (spike / prototype / MVP slice / full change) with an explicit deferred list and iteration path → say-back → explicit exit: graduate scoped to the starting rung, park with a memo, or kill with the reason | The sandbox for what-ifs too fuzzy for a change proposal — burns down unknowns AND right-sizes the entry point, so a weekend idea never silently becomes a quarter |

### Kept, consolidated, retired

- **Kept unchanged:** the nine live-ops CLI skills (`list-running-streams` …
  `stop-running-stream`) — already perfectly shaped (one skill = one focused live-ops task);
  `gh-stack`; `streamlib-local-ci-runner`; `rust-craftsmanship-reviewer`; the three domain experts —
  `gpu-vulkan-expert`, `linux-media-expert`, and `polyglot-ipc-expert`, re-scoped to
  helper-process IPC.
- **Consolidated:** `pr-review-gate` + `change-verifier` overlap heavily (both check test
  lock-in, scope, boundaries, naming) and each PR currently runs up to four review
  lenses. Owner review is the bottleneck — one merged `streamlib-review-pr` lens (plus
  `rust-craftsmanship-reviewer` as the quality lens) replaces them.
- **Reconciled:** `verify-live` self-runs the pipeline as its primary mode, with the
  owner-terminal handshake as fallback, and `evidence-verifier`'s charter matches.
- **Retired:** `plugin-abi-expert` and `package-source-expert`, with their subjects;
  `draft-design` (premise — per-issue design — is forbidden by
  docs-policy), `file-issue` for planned work, the external `amos-next` protocol
  (already broken in four places against this repo: deleted `.claude/workflows/` refs,
  repealed sweep step, drifted rule quotes, missing feedback files). Linear is the
  graph of work that `/whats-next` and `/implement` read.

---

## Layer 3 — Guardrails

Nothing here reintroduces the retired automation loop — loop references in this document
are historical evidence only. The system lives in one plugin, `dev-plugin` in
[tatolab/tato-plugins-official](https://github.com/tatolab/tato-plugins-official), which
this repo's `.claude/settings.json` enables, plus this repo's `CLAUDE.md`, so **every agent
that opens this repo reads the same guidance**, regardless of who launched it or how — a single session or an
orchestrator running many.

### Guidance, not gates

Nothing in the operating model blocks or pauses an agent, with one exception: `permissions.deny`
refuses `gh issue` writes and the GitHub MCP issue-write tools, because work is tracked in Linear. Blocking hooks, skills locked
against model invocation, and "hard stop" steps strand work under an orchestrator, where
nobody is at a prompt, and teach sessions to route around the wall. What carries the
model instead:

1. **CLAUDE.md** (always loaded) describes the usual flow and the line between recording
   a fact (ordinary work, lands with the change) and making a decision (the owner's,
   raised rather than settled silently).
2. **Sharp skill descriptions** so a skill triggers on phrasing without being asked, and
   skill bodies that explain why each step exists.
3. **Hooks** only clean up or add context, never refuse or prompt. Worktrees are Claude Code's
   own (`claude --worktree`, subagent `isolation: worktree`), which clean up after themselves.
   The one permission prompt is an edit to the licence files.
   Rejected: a PreToolUse hook gating source edits on `.claude/state/active-ticket.json`
   (`plan-gate.sh`) — it prompted on every engine edit and taught sessions to click
   through (owner, 2026-08-15). Rejected: any hook that blocks, prompts on, or stops
   an agent's tool call, `rig-brake` included (owner, 2026-10-31).
4. **CI and review backstop**: `streamlib-review-pr` flags any new public trait / module /
   cross-crate boundary the change proposal doesn't name.

### The plan is reviewed, not locked against agents

- Git layer: `CODEOWNERS` on `docs/plan/**` + branch protection — a plan change merges
  after the owner's review. This reviews the diff, which is where a smuggled decision is
  actually visible, without stopping the agent that wrote it.
- Doctrine layer: `CLAUDE.md` §Recording facts vs deciding draws the line. A record of
  what shipped lands with the work; a `DECIDED` / `OPEN` entry moves with the owner's
  agreement, usually through `/align`, `/propose-change`, `/ship-change`, or `/pivot`.

### Rules have a lifecycle

The `streamlib-*` rule skills in dev-plugin stay **invariants** (licensing, naming, RHI boundary,
comments, placement). Process belongs in skills, written as reasoning; a rule a skill or
CI check now covers gets deleted via `/propose-rule`. New rules come through
`/propose-rule` — evidence, draft, owner agreement, dedicated PR — rather than accreting
mid-session because something annoyed an agent once.

### Mechanical checks

- The REMOVED-grep check in `/ship-change` — a script in dev-plugin's `scripts/`, not prose.
- `streamlib-review-pr` checks that any new public trait, module, or cross-crate boundary in the
  diff is named by the change proposal, making "no inline architecture" reviewable.
- The existing xtask gate suite, unchanged.
- `flow.md`: operating-model changes ship as their own PR.

## Numeric caps (hard numbers survive agent interpretation; prose doesn't)

- Change proposal ≤ 350 lines (owner decision 2026-08-07); the number is
  never met by dropping file:line citations or worked API spelling. Issue count per change
  is guidance, not a cap (owner decision 2026-08-02): as few tracer bullets as the change
  honestly needs.
- Scale tier, decided at entry: bug fix / refactor / test work → **no change artifact at
  all** (the default path); new behavior or changed contract → delta change; anything
  touching the RHI, the IPC wire format, the processor model, or the Python API's
  public contract → delta + ADR.

## Rollout (MVP-first; each wave usable before the next starts)

1. **Commit the pivot** — today's uncommitted rule/CLAUDE.md/settings edits + this
   document, one operating-model PR. (`docs/plan/` is currently untracked; nothing
   enforces a plan that isn't in git.)
2. **Wave 1 skills** — `grilling`, `batch-grilling`, `glossary`, `/align`, `/plan`:
   just enough to run the plan session.
3. **The plan session** — `/align` on §Product first (the MVP sentence), then module
   system, processor model, SDK parity, in that order. Everything after this is execution.
4. **Wave 2 skills** — `/reconcile-tracker` first (clean today's misaligned milestones
   and tickets against the fresh plan), then `/propose-change`, `/derive-tickets`,
   `/implement`, `review-pr` consolidation: the first real change flows through them.
5. **Wave 3** — `/ship-change`, `/pivot`, `/hygiene` (whose first full run seeds the
   kill-list), then the docs-consolidation
   change (retiring `docs/architecture/`) as the first big change run through the new
   system — the system migrates the old docs, proving itself on its own bootstrap.

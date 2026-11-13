# core-nodes-in-every-project

> **Asked for by the owner, 2026-11-13 (project thread).** Core Nodes replaces `tatolab-ops`:
> "when i merge everything will new projects get the latest project type with the new
> tatolab-core-nodes?" — then, on hearing it would not: "Begin it". PR #3663 recorded the owner's
> decision that Core Nodes replaces `tatolab-ops`, and left the delete to the change that carries
> Core Nodes into new projects. This is that change.

**The direction, in four sentences:**
1. The runtime unit carries the Core Nodes project, and every project the runtime opens or makes
   can use its nodes and its Program view without copying anything in.
2. `tatolab-ops` is deleted, and nothing carries an old project forward (pre-1.0, as
   self-contained-project-imports' Decided 9).
3. A new project's starter stream is drawn from Core Nodes and shown in its Program view.
4. The engine's one name for a node it knows, `Feedback`, follows Core Nodes.

## Current state (recon, `main` `cb198b4`)

- **Core Nodes is a project, not a module.** `packages/tatolab-core-nodes/` holds
  `tatolab-project.json` (name "Core Nodes"), `nodes/` (12 modules, 75 node types), `streams/`
  (three graphs), `views/program/` and a `pyproject.toml` depending on `tatolab-stream`, `numba`,
  `numpy` and `pillow`. Its node types are spelled by the project's own package,
  `nodes.color:Tint`, `nodes.animation:Lfo`; its view is the node type `Core Nodes:program`
  (`docs/plan/ARCHITECTURE.md:2394-2398`, a view is `<project name>:<view folder>`).
- **The runtime unit carries four first-party packages as wheels**:
  `FIRST_PARTY_PACKAGES_THE_RUNTIME_UNIT_CARRIES_RELATIVE_TO_WORKSPACE` =
  `sdk/tatolab-stream`, `packages/tatolab-compositor`, `packages/tatolab-ops`,
  `packages/tatolab-testpattern` (`xtask/src/stage_first_party_modules.rs:20-25`). Each load links
  each wheel into `.tatolab/first-party-imports/<wheel file name>` and declares it in the project's
  `pyproject.toml` as a requirement and a uv path source
  (`docs/plan/ARCHITECTURE.md:2250-2258`;
  `runtime/streamlib-project-repository/src/project_pyproject_import_sources.rs`). The owner's Mac
  showed it: the Core Nodes project's own `pyproject.toml` was rewritten to require all three
  carried modules.
- **A wheel carries no view.** Views are `views/<name>/` folders of a project or of an import
  (`ARCHITECTURE.md:886-889`, `:2394`). An import is copied into `imports/<folder>/`, and its
  node types are led by the folder, `imports/<folder>/<module>:<qualname>`
  (`ARCHITECTURE.md:2201`, `:2243-2245`). Nothing today resolves an import into the runtime
  unit instead of the project.
- **Core Nodes duplicates `tatolab-ops`.** It holds a copy of all 29 `tatolab-ops` function nodes
  and `ImageFile`, each with a category and description added (PR #3663 body). `tatolab-ops`'
  entry points are `tatolab.ops:<Name>` (`packages/tatolab-ops/pyproject.toml:25-`).
- **The engine names one `tatolab-ops` node**: `FEEDBACK_FUNCTION_NODE_TYPE =
  "tatolab.ops:Feedback"` (`runtime/streamlib-engine/src/core/compiler/compiler_ops/function_node_island.rs:91`),
  the plan says so (`ARCHITECTURE.md:2069-2070`). A `Feedback` from Core Nodes,
  `nodes.logic:Feedback`, closes no loop today (fd380dfd9 says so in the project's README).
- **Other `tatolab-ops` users in the engine tree**: `tests/stream-on-runtime/helper_placement_streams.py:11`
  (`Clamp`, `Gate`, `Lfo`, `Remap`), `tests/stream-on-runtime/pyproject.toml:33`, `:72`,
  `sdk/streamlib-python-wheel/python/tatolab/runtime/_function_node_compilation.py:993` (a message
  naming it), `sdk/tatolab-stream/tests/test_function_node_side_effect_check.py`,
  `release-version-files.json`, and test fixtures in the two Rust files above.
- **The starter** is one node, `tatolab.testpattern:TestPatternSource`, no links, no view
  (`sdk/tatolab-stream/scaffold_template/streams/main.graph.json`); its `pyproject.toml` depends on
  `tatolab-stream` alone and names Tatolab's index (`sdk/tatolab-stream/scaffold_template/pyproject.toml`).
- **Core Nodes is skipped as a module project**: `scripts/tatolab_module_projects.py` treats every
  `packages/*/tatolab-project.json` as a module project and leaves out one with no wheel (PR #3663
  findings).
- **Its engine test loads it from `packages/`**: `tests/stream-on-runtime/test_core_nodes_project.py`.

## What `tatolab-ops` taught about `uv sync` (binding on every option below)

Carrying `tatolab-ops` surfaced these; Core Nodes inherits each.

- **Declared, never installed behind the project's back.** A first-party module the venv has but
  `pyproject.toml` and `uv.lock` do not name is uninstalled, with its requirements, by the next
  `uv sync` — the runtime's or a person's (`ARCHITECTURE.md:2259-2261`, rejected 2026-11-10).
  So Core Nodes' Python is linked into `.tatolab/first-party-imports/` and declared as a
  requirement and a uv path source at each make, sync and lock, exactly as `tatolab-ops` is
  (`ARCHITECTURE.md:2250-2256`), whatever shape decision 1 picks. Its views alone may resolve in
  the runtime unit; its Python may not.
- **It must build as a wheel.** The carried modules are linked as
  `.tatolab/first-party-imports/<wheel file name>`. Core Nodes' `pyproject.toml` has no
  `[build-system]` today; it gains one.
- **Its package cannot be named `nodes`.** Core Nodes' modules are `nodes.color`, `nodes.animation`
  — and every project has its own `nodes/` package. On one interpreter the second `nodes` is
  refused or shadows the first (`ARCHITECTURE.md:2245-2247`). The package is renamed, which also
  settles part of decision 2.
- **Offline first, so its requirements must be carried.** Every sync tries `uv sync --offline`
  against the unit's carried wheels and uv's cache before going online
  (`ARCHITECTURE.md:344-352`). Core Nodes needs `numba`, `numpy`, `pillow` (and `llvmlite`), all
  already pinned in `xtask/runtime_unit_carried_packages.toml` (numpy 2.5.4, inside Core Nodes'
  `numpy>=2.1`). A requirement outside that set makes a new project wait on the network.
- **No dependency group rides along.** On the owner's Mac the project's own
  `[dependency-groups] test = ["pytest"]` failed the offline sync ("pytest was not found in the
  cache") and fell back online; PR #3663 removed it. The carried copy declares no group.
- **The rewrite shows in Git.** The runtime rewrites `pyproject.toml` with each carried module's
  requirement and versioned wheel path, so a release changes every project's `pyproject.toml`;
  the owner saw it as ` M packages/tatolab-core-nodes/pyproject.toml`. Swapping `tatolab-ops` for
  Core Nodes keeps that count the same.
- **Not an import of itself.** When the Core Nodes project is the one open, as on the owner's Mac,
  the runtime must not declare it as its own first-party import.
- **Old declarations go.** Each existing project's `pyproject.toml` names `tatolab-ops` as a
  requirement and source; the first load under the new runtime drops both, or `uv sync` fails on
  a link that no longer exists (`project_first_party_packages_moved_to_the_runtime_version.rs`
  already moves first-party names; it learns that one is gone).

## [NEEDS DECISION] 1 — How Core Nodes reaches a project

The fork this change turns on. A wheel brings nodes and no view; a project brings both but the
runtime only ever copies a project in.

- **A. A carried project import (recommended).** The runtime unit carries the Core Nodes project
  folder, as it carries samples, and every project resolves it as an import that lives in the
  runtime unit instead of `imports/` for its views — Decided 10's exception (first-party modules ride the
  runtime, never copied in) extended from wheels to one project. Its views resolve as for any
  import, so `Core Nodes:program` works unchanged; its Python joins the project's venv the way a
  carried wheel does today (built from the project and linked under
  `.tatolab/first-party-imports/`). New engine work: one carried-import resolver beside the
  existing `imports/<folder>` one, and naming for its node types (decision 2).
- **B. A core module wheel, Program view elsewhere.** Core Nodes becomes an ordinary package,
  `tatolab-core-nodes` with `tatolab.core_nodes`, replacing `tatolab-ops` in the carried list.
  Cheapest: it is today's machinery. But the Program view has no home in a wheel: Tato Studio
  would have to ship it itself, or the unit gains a second mechanism for carried views. The
  project's demo streams stop being something you open beside it.
- **C. Copy it in.** The scaffold copies Core Nodes into `imports/core-nodes/` of each new project.
  No engine work, but every project then holds its own frozen copy, which Decided 10 rejected
  for first-party code: a release no longer reaches existing projects.

Recommendation: **A**. It keeps Core Nodes a project — the thing you open in Studio to see the
demos — and keeps one rule for first-party code: it rides the runtime.

## [NEEDS DECISION] 2 — The names its node types carry

Whatever a project's graph document writes, it writes forever (kept-node-values keys by UUID,
but `type` is a string).

- **A. A stable first-party prefix (recommended):** `core:<Name>` — `core:Tint`, `core:Lfo`,
  `core:Feedback`, and the view `Core Nodes:program` as views are named today. Short, stable
  across the module layout, and what a person types in Add node.
- **B. The import grammar unchanged:** `imports/core-nodes/nodes.color:Tint`. No new rule, but the
  folder and module layout leak into every graph, and moving a node between modules breaks
  documents.
- **C. A Python package spelling** (only with decision 1B): `tatolab.core_nodes:Tint`, as
  `tatolab.ops:Tint` is today.

## [NEEDS DECISION] 3 — What the starter shows

- **A. A Core Nodes picture into Program (recommended):** `SolidColor` → `Noise` (driven by a
  `Timer`) → `Program`, so a new project opens on something moving in its view and shows the
  pathway: a picture node, a number node driving it, a view.
- **B. Keep `TestPatternSource`, add `Program`:** `TestPatternSource` → `Program`. Keeps the
  branded animation; `tatolab-testpattern` stays carried.
- **C. Unchanged:** the starter keeps its one node and no view.

If A, `tatolab-testpattern` stays a carried wheel for now (samples use it) and leaves the starter
only; retiring it is not in this change.

## ADDED

- **ADDED — the runtime unit carries the Core Nodes project.** Staged from
  `packages/tatolab-core-nodes` beside the carried wheels (shape per decision 1), its `tests/`
  and `.tatolab/` left out. A release carries its version, and every project gets that version at
  its next load.
- **ADDED — every project can use Core Nodes' nodes and Program view.** The node catalog lists
  them under their `category`; `add_node` takes their type names (decision 2); a project need not
  declare anything to have them.
- **ADDED — the starter draws from Core Nodes** (decision 3).
- **ADDED — an engine test that a new project's stream runs with Core Nodes and no copy of it.**
  `tests/stream-on-runtime/test_core_nodes_project.py` keeps proving the project's own three
  streams; one new test makes a project with `create_project`, plays its starter, and checks the
  Program view's input is fed.

## MODIFIED

- **MODIFIED — `ARCHITECTURE.md:2061-2071` (`tatolab-ops` is a first-party module).** Becomes: Core
  Nodes is the first-party set of nodes every project has, carried by the runtime unit and built
  the way a third party builds a project, so the runtime still ships no node of its own. Its
  groups replace the list there. The `Feedback` the engine knows is Core Nodes' (decision 2).
- **MODIFIED — `ARCHITECTURE.md:2250-2258` (the runtime unit carries its first-party modules).** The
  carried list loses `tatolab-ops` and gains Core Nodes in the shape decision 1 picks.
- **MODIFIED — `ARCHITECTURE.md:93-116` (the scaffold).** The starter per decision 3.
- **MODIFIED — `FEEDBACK_FUNCTION_NODE_TYPE`** names Core Nodes' `Feedback`, and the Core Nodes
  `Feedback` closes a loop as `tatolab.ops:Feedback` does today.
- **MODIFIED — `tests/stream-on-runtime/helper_placement_streams.py` and the suite's
  `pyproject.toml`** use Core Nodes' `Clamp`, `Gate`, `Lfo` and `Remap`.
- **MODIFIED — `scripts/tatolab_module_projects.py`** stops treating Core Nodes as a module project
  it skips; it is carried, not listed on the Marketplace.
- **MODIFIED — `packages/tatolab-core-nodes/README.md`** drops "It will replace `tatolab-ops`"
  and the copy note.

## REMOVED

- REMOVED: packages/tatolab-ops
- REMOVED: tatolab.ops:Feedback
- REMOVED: from tatolab.ops import
- REMOVED: tatolab_ops_as_a_first_party_module
  The Rust fixtures in `project_pyproject_import_sources.rs` and
  `project_venv_made_with_the_runtime_unit_uv.rs` name another carried module instead.

## Out of scope

- Retiring `tatolab-testpattern` and `tatolab-compositor` as carried wheels.
- A shader with no Image input, given the picture's size and the time (PR #3663 findings).
- Evaluating a compiled island inside the shader evaluator rather than the function node pool.
- Category grouping in tatolab-app's Add node — a consumer; it reads the catalog's `category`.
- The three `test_shader_node_fusion.py` failures on `main` (`await_loaded` reads `'streams': []`
  from a `start_playback` action) — a separate bug.

## Tickets (guidance, after the decisions)

1. Carry Core Nodes in the runtime unit and resolve it in every project (decisions 1, 2), with
   the engine test. Tracer bullet: a project made by `create_project` adds `core:Noise` and
   `Core Nodes:program` and plays.
2. Move `Feedback` and the engine suite onto Core Nodes; delete `tatolab-ops` and every REMOVED
   pattern.
3. The starter (decision 3), its scaffold checks and the nightly lock check.

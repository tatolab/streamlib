# core-nodes-in-every-project

> **Decided by the owner, 2026-11-13 (project thread).** The default nodes of a new project cannot
> build anything useful. `tatolab-core-nodes` is a Tato project, not a pip module; it replaces
> `tatolab-ops` and is always included in every project. It opens in Tato Studio like any project,
> so its nodes and views are built and test driven there. Add node shows a node under a human
> group, never a Python package or module name. Gate 1, the project itself, landed in #3663. This
> change is gate 2: "packaged and included like tatolab-ops for all new projects".

**The direction, in four sentences:**
1. The runtime unit carries the Core Nodes project, as it carries its first-party wheels and its
   samples, at its own version.
2. Every project the runtime loads has Core Nodes as an import, so its 75 nodes are in Add node
   and its Program view is in the view list of every project, new or old, with nothing written
   by the person.
3. `tatolab-ops` is deleted: its wheel, its package, its PyPI and index names, and the one name of
   it the engine knows.
4. Core Nodes stays a project a person opens from `packages/tatolab-core-nodes` to change it.

This amends the DECIDED `tatolab-ops` entry (`ARCHITECTURE.md:2061-2072`), the first-party clause
of the import entry (`ARCHITECTURE.md:2250-2256`) and the scaffold entry, and it is the first
project, not wheel, the runtime unit gives to every project.

## Current state (recon, `main` `cb198b4`)

- **The unit carries four wheels.** `FIRST_PARTY_PACKAGES_THE_RUNTIME_UNIT_CARRIES_RELATIVE_TO_WORKSPACE`
  is `tatolab-stream`, `tatolab-compositor`, `tatolab-ops` and `tatolab-testpattern`
  (`xtask/src/stage_first_party_modules.rs:20-25`), each built by `uv build --wheel` into the
  unit's `lib/tatolab/imports/`.
- **Every project links them.** Each time the runtime makes, syncs or locks a `.venv` it links
  each wheel into `.tatolab/first-party-imports/<wheel file name>` and writes it in
  `pyproject.toml` as a requirement and a uv path source
  (`runtime/streamlib-project-repository/src/project_pyproject_import_sources.rs:130-150`,
  `runtime/streamlib-engine/src/core/runtime/project_venv_made_with_the_runtime_unit_uv.rs`).
  A wheel's nodes reach the catalog through its `tatolab.nodes` entry points, as
  `installed_distribution` sources, named `tatolab.ops:Add`.
- **An import is a project copied into `imports/<folder>/`**, listed in the project file's
  `dependencies`. Its node types are led by its folder, `imports/<folder>/<module>:<qualname>`
  (`core/compiler/compiler_ops/project_node_types_listed_from_source.rs:45-92`), its views are
  listed after the project's own (`streamlib-project-repository/src/project_views.rs:57-75`), and
  its Python requirements join the project's lock through a uv path source with
  `package = false`. So an import already brings everything Core Nodes holds: nodes, views,
  streams and requirements.
- **The unit carries sample projects** as directories beside the lend
  (`SampleProjectTheRuntimeUnitCarries`, `new_project_scaffold.rs:63-80`), so a project
  directory in the unit is an existing shape.
- **Core Nodes** (`packages/tatolab-core-nodes`, #3663) holds 75 nodes in ten groups, each with
  `category=` and `description=`, two streams and the Program view. Its `nodes/` holds its own
  copy of `tatolab-ops`'s 29 function nodes and `ImageFile`. It requires `tatolab-stream`,
  `numba`, `numpy` and `pillow`, the same third-party set `tatolab-ops` requires, which the unit
  already carries as wheels (offline-new-projects).
- **The engine knows one `tatolab-ops` name**, `FEEDBACK_FUNCTION_NODE_TYPE = "tatolab.ops:Feedback"`
  (`core/compiler/compiler_ops/function_node_island.rs:91`), by which a loop through a Feedback
  node is let close. Core Nodes' own `Feedback` has another type name, so it closes no loop.
- **The catalog already serves `category` and `description`** per node type
  (`list_project_node_types`, `streamlib-control-contract/src/control_operations.rs:1239`).
- **`tatolab-ops` is named in** the unit's stage list, `release-version-files.json`, three
  workflows, `scripts/build_linux_runtime_release_tarball.sh`,
  `scripts/ci/runtime_unit_input_digest.sh`, `tests/stream-on-runtime/` (helper placement, create
  project, `pyproject.toml`, `uv.lock`), `sdk/tatolab-stream/tests/`, the wheel's
  `_function_node_compilation.py`, `README.md`, and the plan and decision docs.

## ADDED

- **The core project the runtime unit carries.** The unit carries the Core Nodes project as a
  directory beside the lend, `lib/tatolab/core-projects/tatolab-core-nodes/` in the Linux tarball
  and `Contents/Resources/core-projects/tatolab-core-nodes/` in the macOS helper bundle: its
  `tatolab-project.json`, `pyproject.toml`, `nodes/`, `views/` and `streams/`, and nothing its
  tests or a run of it wrote. `cargo xtask build-runtime` stages it; a unit without it fails
  the build.
- **Core Nodes is an import of every project, linked from the unit and never copied** (owner,
  2026-11-13, on the decision card "Choose how Core Nodes gets into every project": "Linked from
  the runtime"). At each load the runtime links the unit's copy at
  `.tatolab/core-imports/core-nodes` and treats it as an import of the project under the reserved
  folder `core-nodes`, after the imports the project file lists. The project file does not list
  it and history does not record it, as with the first-party wheels today. Every project, a
  year-old one included, gets each release's nodes and fixes with no step and no transaction, and
  one copy serves every project on the machine. A project folder zipped to a machine with
  another runtime version opens with that runtime's Core Nodes.
  Rejected: copying Core Nodes into `imports/core-nodes/` at create and replacing the folder at
  each release — a release would reach a project only through a transaction per project, and
  each project's history would hold files its person did not write (owner, 2026-11-13).
- **How a Core Nodes node is named.** **[NEEDS DECISION 4, built as (a) until the owner says otherwise (session, 2026-11-13)]** A stream document stores a node's
  type as a string, for good.
  - **(a) The import spelling, unchanged (recommended):** `imports/core-nodes/nodes.color:Tint`,
    led by the reserved folder as any import's type is. No new naming rule and no second
    resolver; moving a node between Core Nodes' files breaks stored documents, as it does for any
    import. A person never reads this string: Add node shows the group and the node's name.
  - **(b) A short first-party prefix:** `core:Tint`, `core:Lfo`, `core:Feedback`. Stable across
    Core Nodes' file layout, and one more type grammar the catalog, `add_node` and the load each
    resolve, held by the engine for one project.
  Either way `add_dependency` refuses the folder name `core-nodes`, naming who holds it.
- **Its requirements join the project's lock** through a uv path source on the link with
  `package = false`, as an import's do, so `numba`, `numpy` and `pillow` resolve from the wheels
  the unit carries and a new project still creates and runs with no network.
- **Its views are every project's.** `list_project_views` answers Program after the project's own
  views, its type `Core Nodes:program`, and the runtime serves its files through
  `read_project_file` under the reserved folder.
- **Its streams are reachable, not listed.** Core Nodes' `main` and `spotlight` are demo streams
  of the project opened on its own. Under a project that imports it they are an import's streams:
  usable as fragments, never in the project's stream list.
- **Opening Core Nodes itself.** `packages/tatolab-core-nodes` opened as a project is the listed
  project, so the runtime adds no import of Core Nodes to it, and its node types are its own
  (`nodes.color:Tint`).

## MODIFIED

- **The `tatolab-ops` entry** (`ARCHITECTURE.md:2061-2072`). "`tatolab-ops` is a first-party
  module of `@node.function` functions, bundled with the runtime" becomes: Core Nodes is a
  first-party project of function, shader and process nodes and views, carried by the runtime
  unit and an import of every project, built exactly the way a third party builds a project, so
  the runtime still ships no node of its own. The node list in the entry becomes Core Nodes'
  groups.
- **The one name the engine knows.** `FEEDBACK_FUNCTION_NODE_TYPE` becomes
  `imports/core-nodes/nodes.logic:Feedback` under a project that imports Core Nodes.
  **[NEEDS DECISION 2, built as (a) until the owner says otherwise (session, 2026-11-13)]** Core Nodes opened on its own names the node `nodes.logic:Feedback`, which
  that constant does not match. Options: (a) the engine knows Feedback by a mark its declaration
  carries, `@node.function(closes_a_loop=True)`, read from source as `category` is, so any
  project's node may close a loop and no type name is known to the engine (recommended); (b) the
  engine matches both spellings; (c) Feedback closes a loop only where Core Nodes is imported.
- **The function node compile entry** (`ARCHITECTURE.md:2053`, `:2059`): "`tatolab-ops`' included"
  reads "Core Nodes' included".
- **The import entry's first-party clause** (`ARCHITECTURE.md:2250-2256`): the unit carries its
  first-party wheels and its core project; `tatolab-ops` leaves the sentence.
- **The stage list** (`xtask/src/stage_first_party_modules.rs:20-25`) loses
  `packages/tatolab-ops`; a second stage step copies the core project.
- **`HoldFrame`.** Core Nodes carries it as copied from `tatolab-ops`. It stays, in the Logic
  group, with the behaviour the DECIDED entry states.
- **The starter stream.** **[NEEDS DECISION 3, built as (a) until the owner says otherwise (session, 2026-11-13)]** A new project's one stream is `TestPatternSource`
  alone (first-party-package-index). Options: (a) leave it (recommended for this change: the
  starter is a separate question from what Add node offers); (b) the starter becomes `SolidColor` into `Noise`,
  moved by a `Timer`, into Program, so a new project opens on something moving in a view;
  (c) `TestPatternSource` into Program.
- **Marketplace assembly** (`scripts/tatolab_module_projects.py`) reads every
  `packages/*/tatolab-project.json` as a module project. It names Core Nodes as the unit's, not
  the Marketplace's, and leaves it out by that rule instead of by its missing wheel.
- **Core Nodes' version.** Its `pyproject.toml` version and its `tatolab-stream` floor join
  `release-version-files.json`, so the project the unit carries states the unit's version.

## REMOVED

- REMOVED: `packages/tatolab-ops`
- REMOVED: `tatolab-ops` (outside `docs/plan/changes/archive/`, `docs/decisions/`, `CHANGELOG.md`
  and this file)
- REMOVED: `tatolab.ops:` (the same exceptions)
- REMOVED: `tatolab_ops`
- REMOVED: the "It will replace `tatolab-ops`" paragraph of `packages/tatolab-core-nodes/README.md`

## Assumed

- **No stream is carried over.** A stream document that names a `tatolab.ops:` type is refused
  at load as any unknown type is, naming the node. Nothing rewrites it: pre-1.0, no back-compat
  shim. The owner's own projects that use `tatolab-ops` nodes need their nodes placed again.
- **`tatolab-compositor` and `tatolab-testpattern` stay wheels the unit carries.** This change
  moves `tatolab-ops` only.
- **The names on PyPI and Tatolab's index.** `tatolab-ops` stops being published; what is already
  there stays. `tatolab-core-nodes` is never published to an index: a project is not a wheel.
- **One reserved folder name.** `core-nodes` is the only folder a project cannot import into.
- **Add node's groups are tatolab-app's.** The runtime already serves `category`; grouping by it
  and showing the project's name as the small source line is an app issue, filed against
  tatolab-app once this change is approved, and waits on nothing here.
- **`tests/stream-on-runtime/test_core_nodes_project.py` stays** where #3663 put it, now testing
  what the unit carries.

## Not in this change

Each was asked for by the owner in the project thread and is its own change: audio samples
handed to a view, so Program plays sound; a shader with no Image input, with the picture's size
and with time, so a generator needs no `size_from`; an import's name, description and cover
served to the app wherever the import is named.

## Tickets

Decision 1 is the owner's; 2, 3 and 4 were put to the owner in the project thread as assumptions
and are built as recommended. The cut, each a tracer bullet:

1. The unit carries the core project, and a project loads with it as an import: node types
   listed, a node placed and run, Program listed and served. `tatolab-ops` still ships.
2. Feedback closes a loop by its declaration (decision 2).
3. `tatolab-ops` is deleted everywhere the REMOVED list names, and the plan entries are rewritten.

## Consumers

- `examples/` and `packages/` hold no stream that names a `tatolab.ops:` type outside
  `tests/stream-on-runtime/` (helper placement), which moves to Core Nodes' names in ticket 3.
- tatolab-app reads node types and views from the local API only, so it needs no change to show
  Core Nodes' nodes; its recorded fixtures that name `tatolab.ops:` are refreshed from a runtime
  once ticket 3 lands.

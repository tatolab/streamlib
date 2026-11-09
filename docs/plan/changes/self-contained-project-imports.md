# self-contained-project-imports

> **Decided by the owner, 2026-11-09:** "Everything is always copied in. A project always works
> when you open it. It contains everything. Imported modules are themselves just projects. It's
> always the same exact directory layout. We always copy everything in. … We always reference
> things in projects and never move or copy other imports files around. … opening a project on
> different machines always produces the same exact state. This is the only way to make
> multiplayer work." And: git is "a trackable store for transactions like a transaction log", no
> remote; editing a module means opening it as its own project; `tatolab-ops` "is just another
> module"; `.venv` "is a cache / generated artifact" rebuilt from the lock; generator output stays
> stored. On sources: a project on GitHub's "only job is to copy the entire project into the
> imports akin to npm install", and adding a module is "the same add dependency (which I guess
> really is a copy project as dependency step)". The audit behind this change is the doc "Module
> and git design audit" (2026-11-09).

A pivot of project-and-assets' dependency, content and history clauses. The project core — the
graph document, UUIDs, transactions, undo and redo, one local history per project — stays.

**The direction, in five sentences:**
1. A project is self-contained: every file a load reads sits inside its directory, save the
   runtime's own first-party modules, so copying or zipping the folder carries everything; it opens
   under any runtime, and under the same one it opens to the same state anywhere.
2. Adding a module copies that project once, whole, into `imports/<module>/`; an update replaces
   the folder whole, a removal deletes it, and nothing is ever copied out of it or moved inside it.
3. Every reference to a module's stream, asset or node code resolves into its import folder at
   load, relative to the project that holds the reference, so a module's own imports stay nested
   at `imports/<module>/imports/<other>/`.
4. An asset is the one file at its visible path: a node reads that file, an import copies the
   source into the project once, and history records only the file's hash and size, as Git LFS
   records a pointer, never its bytes.
5. History is one local Git repository per project, the transaction log behind undo, redo and
   multiplayer; nothing pushes, fetches or clones it, and nothing else travels through Git.

## Current state (recon, `main` `d9c2cb8`)

- **Dependency histories are fetched.** `add_dependency` fetches every branch and tag of the other
  project into `.tatolab/dependencies/<uuid>/` (`fetch_every_branch_and_tag_from`,
  `a_dependency_history_fetched_into_place`, project_repository.rs) and reads its streams at a
  pinned commit; a clone re-fetches them (`dependency_history_fetched_with`).
- **Sources include Git remotes and pip packages.** `ProjectDependencySource` is a directory, a
  `package:<distribution>` found in `.venv`, or a `GitRemote` with a bearer token
  (project_dependency_source.rs; mcp_project_dependency_tools.rs).
- **A module's asset bytes are copied per commit** into `.tatolab/content/` with a record at
  `.tatolab/dependency-assets/<uuid>/<commit>.json` (`record_and_copy_the_dependency_assets_at`).
- **Using a stream pastes it.** A `tatolab.project:StreamFragment` node stores a copy of the held
  stream's nodes (stream_graph_document.rs:995), and four refresh paths keep the copies in step
  (`the_refresh_of_each_fragment_holding_a_stream`, `the_refresh_of_each_dependency_fragment_at`,
  `refresh_each_fragment_holding_an_edited_stream`).
- **Fork copies out of a module.** `fork_asset` copies a stream or asset in under a new UUID with
  `forked_from` and rewrites references (`fork_dependency_asset`).
- **Every imported file exists twice.** `import_asset` copies the bytes to
  `.tatolab/content/<sha256>` and again to the visible path; a node opens the hidden copy
  (`the_content_path_of_an_asset`); history records only the sidecar.
- **History can be pushed and fetched** over smart HTTP with a token (`push_main_to`,
  `fetch_main_from`, project_history_smart_http_transport.rs); only its own test calls them.

## MODIFIED

- **The project file** (project-and-assets §ADDED): `dependencies` lists, for each import, the
  module's project UUID, its folder under `imports/`, the source it was copied from, and the
  version it carried — its history's head commit when copied, informational only and never
  resolved.
- **Content** (project-and-assets §ADDED): an asset's bytes are the file at its path and nowhere
  else; nothing keeps an older version of them. The sidecar keeps the UUID, kind, content hash,
  size and import settings, so history knows a different file stood there at each commit.
  `import_asset` copies a source on the machine into place once; an
  upload a client streams lands in `.tatolab/uploads/`, a cache the import empties.
- **History** (project-and-assets §ADDED, Decided 4, 6 and 8): every commit records every file
  of the project as Decided 8 states — every file outside a hidden folder (`.tatolab/` and `.git`
  among them), `node_modules`, `__pycache__` and a Python virtual environment, and no symbolic
  link — except a media, scene or data asset's bytes, which history records as its sidecar alone.
  `imports/` is in history on the same terms. An undo of a delete or a replace restores the
  sidecar; the bytes come back only if the file is still at its path, and otherwise the asset reads
  missing, naming the file it needs. Git is
  the store engine only: one branch, `main`, no remote, no push, no fetch, no clone path.
- **Linked dependencies → imports** (project-and-assets §ADDED, Decided 3):
  - `add_dependency {source}` copies the module once into `imports/<folder>/`, wherever it comes
    from: a folder or a zip on this machine (a teammate's hand-off, dropped onto Tato Studio), or a
    download such as a GitHub archive, with no Git. The source is refused unless it holds a
    `tatolab-project.json` with a project UUID, naming what is missing. What is copied is the module's project: everything its history records
    (its own `imports/` nested inside), never its `.tatolab/` or a cache. The folder is named after
    the module and refused when another import holds that name or that project UUID.
  - `update_dependency {project, source?}` copies the module again, whole, from its recorded
    source or a new one, as one transaction; `list_dependencies` answers, with
    `check_for_newer`, whether the source's head differs from the recorded version.
    `remove_dependency` deletes the folder and is refused while a reference names it.
  - A module's Python is the project's: its folder is a uv path dependency of the project
    (`[tool.uv.sources] <name> = { path = "imports/<folder>", editable = true }`), written by the
    runtime, so `uv sync` rebuilds `.venv` from the lock and the module's node code imports from
    its folder (#3382 keys node types by module on top of this). An import whose requirements the
    project's lock cannot resolve beside the rest is refused in uv's own words, since a project
    has one interpreter.
- **Stream fragments are references** (project-and-assets §Fragments, graph-groups-and-nested-
  fragments): a `tatolab.project:StreamFragment` node holds only `{"stream": {"asset", "from"?}}`.
  The load reads the held stream's graph document from this project or the module's import and
  flattens it in place, each held node's UUID derived as UUIDv5 of the fragment node's UUID and the
  node's own, so two placements never share one and nothing is stored. A stream that holds itself,
  directly or through another, is refused at load by name. `@graph_generator` output stays stored
  as decided.
- **Asset references** (project-and-assets §The asset verbs): `{"asset", "from"}` resolves to the
  file at that asset's path in the module's import; a node opens it through
  `asset_content_path(asset, from_project)` as today.
- **Editing a module** is opening its folder, `imports/<folder>/` included, as its own project in
  its own window, the module as the project root; nothing is written back to the source it was
  copied from. Tato Studio's window for it is app work.
- **Tato Studio's "Get more"** adds a module or a Python requirement through the runtime's
  dependency verbs; the runtime is the only writer of `pyproject.toml`.

## REMOVED

- REMOVED: ProjectHistoryRemote
- REMOVED: push_main_to
- REMOVED: fetch_main_from
- REMOVED: runtime/streamlib-project-repository/src/project_history_smart_http_transport.rs
- REMOVED: runtime/streamlib-project-repository/tests/a_projects_history_round_trips_through_a_smart_http_remote.rs
- REMOVED: fetch_every_branch_and_tag_from
- REMOVED: PROJECT_DEPENDENCIES_DIRECTORY
- REMOVED: dependency_history_fetched_with
- REMOVED: a_dependency_history_fetched_into_place
- REMOVED: dependency_stream_at_commit
- REMOVED: ProjectDependencySource::GitRemote
- REMOVED: PYTHON_PACKAGE_DEPENDENCY_SOURCE_PREFIX
- REMOVED: the_project_directory_an_installed_package_ships
- REMOVED: PROJECT_DEPENDENCY_ASSET_RECORDS_DIRECTORY
- REMOVED: record_and_copy_the_dependency_assets_at
- REMOVED: the_dependency_assets_recorded_at
- REMOVED: fork_dependency_asset
- REMOVED: DependencyAssetFork
- REMOVED: the_fork_of_a_dependency_asset
- REMOVED: ProjectAssetForkOrigin
- REMOVED: FORKED_FROM_KEY
- REMOVED: the_refresh_of_each_fragment_holding_a_stream
- REMOVED: the_refresh_of_each_dependency_fragment_at
- REMOVED: refresh_each_fragment_holding_an_edited_stream
- REMOVED: ProjectContentStore
- REMOVED: PROJECT_CONTENT_STORE_DIRECTORY
- REMOVED: runtime/streamlib-project-repository/src/project_content_store.rs
  The `fork_asset` MCP tool and its description in mcp.rs go with these, and #3394 closes as moot.

## Decided (owner, 2026-11-09)

**Decided 1 — copy once, then reference.** Nothing a module holds is copied or moved after the
import; the one exception is a cache that can always be thrown away and rebuilt (`.venv`, a
compiled shader).

**Decided 2 — no fork.** Editing a module means opening it as its own project.

**Decided 3 — no `package:` source and no Git remote.** A module arrives as a project, from a
folder, a zip or a download, through the one `add_dependency`.

**Decided 4 — history holds pointers, never media bytes.** A project's media, scenes and data are
tens or hundreds of megabytes each, so history records that a different file stood at a path,
never the file, as Git LFS, Perforce workspaces and Unity's Plastic keep only the current bytes on
the machine. Keeping older versions is cloud storage's job, later.

**Decided 5 — one way in.** `add_dependency` is the one door for a module copied into a project;
`.venv` is a cache rebuilt from the lock.

**Decided 6 — generator output stays stored.** Whether a project needs a build step that walks it
and regenerates derived outputs, as Unreal's compile does, is open for later.

**Decided 7 — third-party Python is uv's.** Self-contained covers the project's own files and its
imports; a third-party wheel (numpy, torch) comes from uv's cache or the index when `.venv` is
rebuilt, and a sync that reaches neither fails the run in uv's words, as ComfyUI's does. Each step
of that sync is reported on the run's action so Tato Studio can show it downloading and installing.

**Decided 8 — guide, never block, an update over edits.** `update_dependency` on an import edited
since its copy answers which files were edited and asks the caller to confirm the overwrite with
`overwrite_edited_files`; with it, the folder is replaced whole.

**Decided 9 — no transition.** The content store was a rewrite pre-1.0: after ticket 2 a node
reads the visible file, and nothing carries an older project forward.

**Decided 10 — first-party modules ride the runtime, never copied in.** The runtime unit carries
its first-party modules, `tatolab-ops` among them, in an imports folder of its own, as ComfyUI
carries its core nodes and Unreal its engine plugins. A reference to one resolves there, never
into the project, so each runtime release gives every project its version; it is the one
exception to copying in, because we make them and guarantee them. The project file records the
runtime version that created it, as a `.uproject` records its engine, and a load under another
version says so in the project's log and never refuses or holds it: the version is a record,
not a lock. It replaces the PyPI requirement PR #3392 added. Each time the runtime makes or
syncs a project's `.venv`, it installs the first-party modules it carries into that `.venv`; a
project's `pyproject.toml` and `uv.lock` never name them, and the `.venv` is synced again when
the runtime or one of its modules changes.

## Assumptions to correct if wrong

- **Editing a module in place.** A transaction on a project open at `imports/<folder>/` is
  recorded in that module's own history and also as one commit on the enclosing project, the
  runtime being the one writer of both, so the enclosing project's history and change feed account
  for it; undo stays per window.
- **A private GitHub repository** is out of the first slice; a download of a public one needs no
  credential, and a private one returns with whatever sign-in Tato Studio gets.
- **A plain Python requirement** (numpy, torch) stays a `pyproject.toml` requirement the runtime
  writes through uv, beside modules, since `.venv` is a cache rebuilt from the lock.

## Tickets (as few tracer bullets as the change needs)

1. **Rip out the Git transport and the fetched-history dependencies.** Every REMOVED bullet but
   the content store's, `add_dependency` / `update_dependency` / `remove_dependency` /
   `list_dependencies` over `imports/`, stream fragments resolved at load, the uv path
   dependency, `LOCAL_API_MCP_SERVER_INSTRUCTIONS` and each dependency tool's description
   rewritten to the import verbs, and the plan, decisions doc and glossary corrected. Proof: a project with a module
   zipped, unzipped elsewhere and loaded reads the module's stream, asset and node code.
2. **One copy of each asset.** The content store goes; nodes read the visible file; history records
   each asset's sidecar, never its bytes; uploads land in `.tatolab/uploads/`; `LOCAL_API_MCP_SERVER_INSTRUCTIONS` and the
   asset tools' descriptions stop naming the content store. Proof: replacing a file in Finder is what a
   node reads, and history grows by a sidecar, not the file, when a 100 MB file is replaced. May land before or after 1.
3. **Tato Studio's "Get more" through the runtime** (tatolab-app): adding and updating modules,
   the overwrite prompt for an edited import, and a status indicator showing a run's download and
   install steps. Waits on 1.

## At ship

The fold rewrites the project entry in §Processor model to this change's shape and adds, inside it:

    Rejected: a dependency as another project's Git history fetched and pinned at a commit, its asset
    bytes copied in per commit, its streams pasted as stored copies kept in step by refreshes, and a
    fork copying an asset out of it — copies drift and make a project's state differ per machine
    (owner, 2026-11-09).

## Notes (not in scope)

- #3382 (a module's node code imports from its folder, keyed by module) builds on ticket 1.
- `run <url-or-zip>` (§Packages) downloads into a visible project folder, as a sample is created,
  when it is built.

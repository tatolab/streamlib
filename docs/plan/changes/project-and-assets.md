# project-and-assets

> **Decided by the owner, 2026-11-07 and 2026-11-08:** "I did change to a graph document and do
> not write directly into the file (that's the plan)"; the graph document "is supposed to be
> similar to touch designers file … and unreal … files … that store the actual data … we aren't
> using stream.py anymore"; "we will have assets we need to track … and be able to allow the api
> and mcp to do that too for example media files, usd, images … This is a full on studio"; a
> maintained stream used by another team "by default … should be linked and kept up to date (I can
> optionally update it) and only if I really want to fork it"; history "can't use our own local
> git, its gotta use some mechanism we package with the runtime … needs to be fully compatible and
> follow the same concepts of cloudflare artifacts … mindful there are different histories per
> project". The starting design is the owner's write-up *Stream storage plan: the graph as source
> of truth* (2026-11-07), which answers #3188.

A pivot. The plan moved first: §Processor model's "the graph is emitted, never authored" is
replaced by the project-and-assets entry, and every entry that loads a stream from `stream.py`
carries an `amended by project-and-assets` pointer. This file is the build and the rip-out that
follow. Rationale: `docs/decisions/project-and-assets.md`.

**The direction, in five sentences:**
1. A project is a versioned repository of assets, the way an Unreal project or a Houdini job
   folder is: a stream's graph is one kind of asset beside media, images, USD, FMUs and the rest,
   and a project has no `stream.py`.
2. Every asset and every node instance gets a UUID the runtime generates when it is created or
   placed; its config, state and links follow the UUID, a name is only a label, and every
   reference names a UUID.
3. The runtime is the project's one writer: every change, a graph edit or an asset import alike,
   is a transaction with an author and an inverse, applied whole or refused whole over the local
   API and MCP, and the unit of undo and history.
4. Each project has its own history, kept by a Git engine the runtime carries so nobody needs Git
   installed, in a form Cloudflare Artifacts accepts as a remote, so branches, merges and a hub
   that syncs without a runtime build on it later.
5. Another project's asset is used as a linked dependency pinned to a revision, updated when the
   person chooses and copied only when they fork it; Python stays the language of nodes, and a
   `@stream` function becomes a generator whose output is stored as a frozen fragment.

**A project after this change** (names assume decision 2's recommendation):

```text
robot-cell/
  tatolab-project.json                  # project UUID, name, dependencies with pinned revisions
  streams/arm-control.graph.json        # a stream asset: nodes by UUID, links, fragments
  media/intro.mp4.asset.json            # a media asset's sidecar: UUID, kind, content hash
  models/arm.usd.asset.json
  nodes/inverse_kinematics.py           # node code, plain files
  pyproject.toml  .python-version  .venv/
  .tatolab/history.git                  # this project's history, never the person's own .git
  .tatolab/content/<sha256>             # large binary content, addressed by hash
```

**Scale gate — this skill plus an ADR.** It changes the Python API's public contract, the local
API's addressing, the processor model's load path, and adds the project as a stored unit.

## Current state (recon, 2026-11-08, `main` `f8e9676`)

- **A load compiles Python.** `run`, `dev`, `run_project` and create run the compile entry with
  `--every-stream`
  (`runtime/streamlib-engine/src/core/compiler/compiler_ops/stream_function_compile_in_the_projects_interpreter.rs:46-47`,
  `sdk/tatolab-stream/tatolab/stream/_project_stream_compile_entry.py:10-13`).
- **The graph is already data.** `GraphSnapshot` reads a `graph` document's spec keys and skips
  its live keys (`runtime/streamlib-engine/src/core/graph_snapshot.rs:37-101`); a node's `id` is a
  live key (`:101`), the processor id of one run. Golden graphs load on every build
  (`runtime/streamlib-media-builtins/tests/every_golden_graph_loads.rs`).
- **The runtime keeps a graph per stream, out of sight.** `StreamRecord.graph` holds the graph
  compiled at the load in the machine state directory
  (`runtime/streamlib-engine/src/core/runtime/machine_state_directory.rs:42-67`); live edits are
  never written to it.
- **A stream's identity is its function.** `StreamLocation` is the project directory plus
  `module:qualname` (`runtime/streamlib-engine/src/core/runtime/stream_location_and_id.rs:30`).
- **Tools address nodes by name**, and a node's config names another node by name as a reference
  edge (`runtime/streamlib-engine/src/core/graph/config_node_reference_edges.rs`).
- **Nothing tracks assets.** A media file is a path in a node's config.
- **Cloudflare Artifacts** (docs read 2026-11-08, open beta since 2026-10-01): a standard Git
  smart-HTTP remote per repo; fetch on protocol v1 and v2, push on v1 `receive-pack` only; fork and
  import through the Workers binding; repo-scoped read or write tokens. Limits: 1 GB per repo,
  32 MiB per file, 64 MiB and 50,000 objects per push, no Git LFS. Large media cannot live in the
  history itself.

## ADDED

- **The project file.** `tatolab-project.json` holds the project's UUID, its name and its
  dependencies, each with a pinned revision. The runtime writes it; a load reads it.
- **Assets.** Every asset has a UUID, a kind and a path in the project. Kinds in the first slice:
  `stream` (a graph document), `media` (video, audio, image), `scene` (USD, glTF), `data` (an FMU,
  a LUT, model weights, a font). A stream asset carries its UUID inside its document; every other
  asset has a sidecar `<file>.asset.json` holding its UUID, kind, content hash, size and import
  settings, so each asset is one file in history and a move or rename keeps its UUID.
- **Content.** An asset's bytes live in `.tatolab/content/`, named by their SHA-256, and only the
  sidecar enters history, as Git LFS keeps a pointer. Node code (`nodes/*.py`, Slang) is plain
  files in history, named by import path as today.
- **Content never rides a transaction.** An import first copies the bytes into the content store
  (from a path on the machine, or from an upload a client streams to the store), and the
  transaction carries only the asset's UUID, kind and content hash, as Unreal and Unity keep
  content apart from the operations that reference it.
- **The asset verbs**, each a transaction: `import_asset {source, kind?, into}`, `source` a path on
  the machine or a finished upload's content hash (answers the new UUID), `list_assets`, `describe_asset`, `rename_asset`, `move_asset`, `delete_asset` (refused
  while a reference names it, listing them) and `set_asset_settings`. `graph`'s reference edges
  extend to assets: a config value `{"asset": "<uuid>"}` is a reference edge to an asset, checked at
  load as a node reference is. A node opens its asset's content through the runtime by UUID.
- **The graph document.** One per stream, canonical JSON (sorted keys, two-space indent, one
  trailing newline). Each node carries `id` (a UUID the runtime generates at insert, a spec key,
  never reused), `name` (a label), `type`, `kind`, `config`, `pins` (the value on each unlinked
  input) and `studio` (canvas position, group, comment, colour). A link end is `{node: <uuid>,
  port}`. It also holds variables, Graph Input and Output nodes and fragments. Live keys stay out:
  link state, counters, island placement. `graph` renders the document plus its live keys, so its
  output still loads. Saved: structure, config, pin values, studio fields. Never saved: data on
  links, runtime state, a stateful node's `State`.
- **Transactions.** Every change to a project is one: an id, an `author` (a person, an agent or
  the runtime), its parent, its operations and their inverse. The runtime applies it whole or
  refuses it whole in its own words, writes the files, records it in history and announces it on
  the project's change feed, which rides `/mcp/stdio` as notifications. A caller marks a graph
  transaction commit (the default) or live; live is applied to the running stream only and is gone
  on the next load. A continuous gesture is sent as one transaction when it ends.
- **The graph operations.** `add_node` (answers the UUID), `remove_node`, `connect`,
  `disconnect`, `set_node_config`, `set_pin_value`, `rename_node`, `set_studio_fields`, and
  `transact` batching any operations of a project. Each validates inside the transaction as today;
  rules that refuse only at start stay at start, so a graph can be wired one input at a time.
- **Undo and redo.** `undo {project, author}` applies the inverse of that author's latest
  transaction as a new one; `redo` reapplies it. Undo reverses only its own author's edits.
- **History.** Each project's history is a Git repository at `.tatolab/history.git`, written by a
  Git engine the runtime unit carries, never the person's own Git or `.git`. Every committed
  transaction is one commit on the current branch, its author the transaction's. The first slice
  keeps one branch, `main`; stock `git clone` of the history must read it, so an Artifacts remote
  can later take a push unchanged. Its operations follow Artifacts' programmatic model — create,
  fork, import, branch, tag, read a file or a commit by path — so a local project and a hosted
  one are driven the same way.
- **Linked dependencies.** `add_dependency {source, revision}` pins another project — a local
  path, an Artifacts remote or a package that ships a project — at a commit or tag; its history is
  fetched into `.tatolab/dependencies/<project uuid>/`. Its assets are used by reference,
  `{"asset": "<uuid>", "from": "<project uuid>"}`, and are read-only here except the inputs and
  promoted config a stream offers. `update_dependency {project, revision}` moves the pin as a
  transaction; the studio shows when a newer revision exists. `fork_asset` copies one asset into
  this project under a new UUID, recording the asset it came from, and rewrites this project's
  references to it.
- **Fragments.** `import_stream_generator {stream, generator, config}` runs a `@stream` function
  by import path in the project's interpreter and stores its output as a fragment: a read-only
  container with its own node UUIDs, whose Graph Input and Output nodes are its ports and whose
  extra parameters are its promoted config. A re-run refreshes it in place and keeps a node's UUID
  when the node's name in the function's output is unchanged, so links into the fragment survive.
  It flattens at compile under its reference's name; it is stored, never recompiled on a load.
- **`dev`.** Follows the project's log, re-loads a stream when its document changes on disk, and
  refreshes every fragment whose generator's module changed. A node code change respawns that node.

## MODIFIED

- **The graph is data** (§Processor model): there is a saver; the runtime writes the document.
- **Loading a project** (§Product): a load reads `tatolab-project.json` and loads each stream asset
  as its own stream. A document that will not parse, or names what the runtime refuses, records
  that stream `failed` with the reason. A load runs no project code except a generator a
  fragment's import or refresh asks for.
- **A stream's location and id** (§Product): a stream is its project's UUID plus its stream
  asset's UUID, so moving the project or renaming the document keeps its id; the id is
  `<project name>-<stream name>-<hash>`, the hash over the two UUIDs, the same cast as today.
- **The stream record** keeps interpreter, stopped, failed and crash count, and no graph.
- **The local API's addressing** (§Control plane): every tool names a node, an asset and a project
  by UUID; `graph` renders a node's `id` as a spec key. A port's address is
  `<machine>/<stream id>/<node uuid>/<port>`.
- **Creating a project**: the scaffold writes the project file, one stream asset, its node modules
  and the first commit of its history; no `stream.py`.
- **`list_project_streams`** becomes `list_assets` narrowed to `stream`, read without running code.
- **How a stream leaves**: `delete_asset` on a stream unloads it; `remove_project` stays.
- **lightweight-node-kinds §What Tato Studio reads**: the edit verbs take shader and function
  types as transactions on the graph document.

## REMOVED

- REMOVED: sdk/tatolab-stream/scaffold_template/stream.py
- REMOVED: COMPILE_ENTRY_EVERY_STREAM_FLAG
- REMOVED: EVERY_STREAM_DOCUMENT_KEY
- REMOVED: --every-stream
  The compile entry keeps one job: run one generator for a fragment's import or refresh.
- REMOVED: of_a_project_that_gave_no_stream
  A project with no `stream.py` stops being a failed record.
- REMOVED: The graph compiled at the load
  `StreamRecord.graph`'s doc line; the field goes and the start loads the document.
- REMOVED: list_project_streams

## Decisions

**Decided 1 — one file per asset (owner, 2026-11-08).** History, merges and partial fetches work
per file, so two people editing different streams never touch one file, and a teammate fetches
only the assets they need. Unreal moved to one file per actor for the same reason.

**Decided 2 — UUIDs generated by the runtime at creation (owner, 2026-11-08).** A node's or an
asset's UUID is made when it is placed or imported and follows it for life; names are labels.

**Decided 3 — linked by default, fork to own (owner, 2026-11-08).** Another project's stream or
asset is used pinned to a revision and updated when the person chooses; a fork makes it theirs.

**Decided 4 — history the runtime carries, one per project, Artifacts-compatible (owner,
2026-11-08).** No dependency on an installed Git; a project's history follows Artifacts' concepts
(repos, branches, tags, forks, imports) and can be pushed to it as a remote.

**[NEEDS DECISION] 5 — what the files are called.** Recommend `tatolab-project.json`,
`<name>.graph.json` for a stream and `<file>.asset.json` for every other asset's sidecar, with
`.tatolab/` for history, content and dependencies. Each is a name a person sees in Finder and in
Git, so it is the owner's call; nothing else in this file depends on the spelling.

**[NEEDS DECISION] 6 — the Git engine the runtime carries.**
- (a) `git2` over libgit2: mature fetch and push over smart HTTP, including the v1 `receive-pack`
  Artifacts takes; GPLv2 with the linking exception, so it links into a BUSL binary.
- (b) `gix` (gitoxide): pure Rust, no C build; reading and fetching are mature, pushing is not yet
  complete.

Recommend (a) for the first slice, behind one RHI-style seam so (b) can replace it once it pushes.
To verify in ticket 1: libgit2's push against an Artifacts remote.

**Deferred, on the same transactions and history:** the cloud hub (one Durable Object per project,
PartyKit-style, ordering transactions with no runtime), branches per person and merge rules, roles,
releases and variants, pushing content to cloud storage beside an Artifacts remote, presets and
"keep current values", the live layer for running programs, unlocking a fragment, a CRDT.

## Tickets (as few tracer bullets as the change needs)

Blocked on decisions 5 and 6, and on roadmap step 4 (#3214), because `exposed` leaves the spec
keys and the record there.

1. **A project is a repository of assets.** The project file, history in `.tatolab/history.git`
   through the carried Git engine with a commit per transaction, sidecars with UUIDs, the content
   store, the asset verbs over the local API and MCP, and the change feed. Proof includes stock
   `git clone` of the history.
2. **A stream is a graph asset.** The graph document with runtime-generated node UUIDs, every load
   reading stream assets, the scaffold writing a project, the stream id from UUIDs, `graph`
   rendering `id` as a spec key. Carries the `stream.py` REMOVED bullets. Waits on 1.
3. **Graph edits are transactions.** The graph operations and `transact`, commit or live, undo
   and redo per author, every tool addressing by UUID, asset references as reference edges. Waits
   on 2.
4. **A `@stream` generator becomes a frozen fragment.** `import_stream_generator`, the fragment
   in the document and in `graph`, UUIDs kept by name across a re-run, `dev` refreshing fragments.
   Carries the compile-entry REMOVED bullets. Waits on 2.
5. **Linked dependencies.** `add_dependency` from a local path or an Artifacts remote, read-only
   use of its assets, `update_dependency` and `fork_asset`. Waits on 2.
6. **A project's node types and generators, listed without running code** (#3152, rewritten):
   each `@node` class, function and shader node and `@stream` generator in a project and its
   dependencies, read from source, over the local API and MCP. Waits on 1.

## Notes (not in scope)

- Converted consumers under `examples/` and `packages/` that ship a `stream.py` get backlog at the
  consumer once ticket 2 lands; a package afterwards ships a project as package data.
- Tato Studio (tatolab-app) removes its `stream.py` failed-record handling, added in its #156, in
  the app change that follows ticket 2, which changes how a stream gets its id.

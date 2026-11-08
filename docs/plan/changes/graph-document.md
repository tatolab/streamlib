# graph-document

> **Decided by the owner, 2026-11-07:** "I did change to a graph document and do not write
> directly into the file (that's the plan)"; "with the new graph document it will always be
> stable because it's supposed to be similar to touch designers file … and unreal … files … that
> store the actual data, the old approach relied on making apps that were dynamic via stream.py
> and that goes away cause we aren't using stream.py anymore". The design is the owner's write-up
> *Stream storage plan: the graph as source of truth* (2026-11-07), which answers #3188.

A pivot. The plan moved first: §Processor model's "the graph is emitted, never authored" is
replaced by the graph document entry, and every entry that loads a stream from `stream.py`
carries an `amended by graph-document` pointer. This file is the build and the rip-out that
follow. Rationale: `docs/decisions/graph-document.md`.

**The direction, in five sentences:**
1. A stream's source of truth is its graph document, a canonical JSON file in its project
   folder; the runtime loads a stream from it, and a project has no `stream.py`.
2. The runtime is the document's one writer: every edit is a transaction of operations, applied
   whole or refused whole in the runtime's own words, with an author and an inverse, so it is
   the unit of undo and history.
3. Every node has a stable id that never changes; its name is an editable label, and links, the
   local API's tools, viewers, positions and history all name a node by its id.
4. Python stays the language of nodes; a `@stream` function is a generator whose output is placed
   in the graph as a frozen, read-only fragment, and nothing ever writes a graph edit back into
   Python.
5. Single player first: one runtime, saved positions, undo and redo over MCP; the cloud hub,
   branches, roles, releases and the live layer for running programs are later work on the same
   transactions.

**What a project holds after this change** (names assume the recommendations below):

```text
my-project/
  tatolab-project.json        # the project and its streams, by document path
  streams/main.graph.json     # one graph document per stream, canonical JSON
  nodes/halftone_lens_effect.py
  pyproject.toml  .python-version  .venv/
```

**Scale gate — this skill plus an ADR.** It changes the Python API's public contract (`stream.py`
and `stream_builder` stop being how a stream is written), the local API's node addressing and the
processor model's load path.

## Current state (recon, 2026-11-08, `main` `f8e9676`)

- **A load compiles Python.** `run`, `dev`, `run_project` and create run the project's compile
  entry with `--every-stream`
  (`runtime/streamlib-engine/src/core/compiler/compiler_ops/stream_function_compile_in_the_projects_interpreter.rs:46-47`,
  `sdk/tatolab-stream/tatolab/stream/_project_stream_compile_entry.py:10-13`), which imports
  `stream.py` and compiles every `@stream` (`_stream_graph_builder.py:97-103`).
- **The graph is already data.** `GraphSnapshot` reads the spec keys of a `graph` document and
  skips its live keys (`runtime/streamlib-engine/src/core/graph_snapshot.rs:37-101`); a node's
  `id` is a live key today (`:101`), the processor id of one run. Golden graphs under
  `runtime/streamlib-media-builtins/tests/golden_graphs/` load on every build
  (`every_golden_graph_loads.rs`).
- **The runtime already keeps a graph per stream, out of sight.** `StreamRecord.graph` holds the
  graph compiled at the load, in the machine state directory, and the start re-loads it
  (`runtime/streamlib-engine/src/core/runtime/machine_state_directory.rs:42-67`). Live edits are
  never written to it.
- **A stream's identity is its function.** `StreamLocation` is the project directory plus
  `module:qualname` (`runtime/streamlib-engine/src/core/runtime/stream_location_and_id.rs:30`).
- **Tools address nodes by name.** `add_node` answers the name the node got, and `remove_node`,
  `connect` and the viewer read take names (`ARCHITECTURE.md` §Control plane, the local API speaks
  the graph's words).
- **The scaffold writes `stream.py`** (`sdk/tatolab-stream/scaffold_template/stream.py`).

## ADDED

- **The project file.** `tatolab-project.json` names the project and lists its streams, each by
  its graph document's path relative to the project. The runtime writes it when a stream is added
  or removed; a person or an agent may edit it, and a load reads it.
- **The graph document.** One per stream, canonical JSON (sorted keys, two-space indent, one
  trailing newline) so a Git diff reads well. It grows from today's spec keys: `stream`, `nodes`,
  `links`, reference edges, variables, Graph Input and Output nodes, and `fragments`. Each node
  carries `id` (stable, a spec key, never reused), `name` (an editable label), `type`, `kind`,
  `config`, `pins` (the value on each unlinked input) and `studio` (position, group, comment,
  colour). A link end is `{node: <id>, port}`. The live keys stay out of it: link state, counters,
  island placement, where a node runs. `graph` renders the document's keys plus its live keys, so
  its output still loads. The golden graphs gain document goldens beside them; none is edited.
- **What is saved.** Structure, config, pin values and studio fields are saved. Data on links,
  runtime state and a stateful node's `State` never are.
- **Transactions.** Every change to a stream is a transaction: an id, an `author` the caller names
  (a person, an agent or the runtime), its parent transaction, its operations and their inverse.
  The runtime applies it whole or refuses it whole, naming why in its own words, then writes the
  document and announces it on the stream's change feed. It is the unit of undo and history.
- **The operations.** `add_node`, `remove_node`, `connect`, `disconnect`, `set_node_config`,
  `set_pin_value`, `rename_node` and `set_studio_fields`, alone or batched in one `transact` call.
  Each validates as today inside the transaction (an unknown `type`, a family mismatch at
  `connect`, the kind linking rules, an unknown config key). Rules that refuse only at start — a
  required input unlinked, a get with no setter — stay at start, so a graph can be wired one input
  at a time. A studio-field change never touches the running stream.
- **Commit or live.** A caller marks each transaction. Commit (the default) is applied to the
  running stream and written to the document. Live is applied and not written, and is gone on the
  next load. The live layer for running programs (committing a live change later, "keep current
  values", "revert to saved") is deferred.
- **Undo and redo.** `undo {stream, author}` applies the inverse of that author's latest
  transaction as a new transaction; `redo` reapplies it. Undo reverses only its own author's
  edits. A continuous gesture (a drag, a slider) is sent as one transaction when it ends.
- **The change feed.** One per stream, carrying each applied transaction, so the studio, agents
  and the CLI see each other's edits; it rides the existing `/mcp/stdio` connection as
  notifications.
- **Fragments.** `import_stream_generator {stream, generator, config}` runs a `@stream` function
  (by import path, `my_project.rigs:camera_rig`) in the project's interpreter and places its output
  as a fragment: a container in the document with its own nodes and ids, read-only in the
  workspace, whose Graph Input and Output nodes are its ports and whose extra parameters are its
  promoted config. Re-running it refreshes the snapshot in place. A fragment flattens at compile
  under its reference's name as a subgraph does today; it is stored, never recompiled on a load.
  `graph` renders each fragment with its generator, config and nodes.
- **`dev`.** Follows the project's log, re-loads the stream when its document changes on disk, and
  refreshes every fragment whose generator's module changed. A node code change respawns that
  node as today.

## MODIFIED

- **The graph is data** (§Processor model): there is a saver now. The runtime writes the
  document; `graph` renders it with its live keys beside.
- **Loading a project** (§Product): every load reads `tatolab-project.json` and loads each listed
  document as its own stream. A document that will not parse or names what the runtime refuses
  records that stream `failed` with the reason, as a failed compile does today. `run` never runs
  project code except a generator a fragment's import or refresh asks for.
- **A stream's location** (§Product): the project directory plus the document's path
  (`streams/main.graph.json`); the stream id is `<folder>-<document stem>-<hash>`, the same cast
  and hash. A moved project is still a new stream.
- **The stream record** keeps what the runtime owns — interpreter, stopped, failed, crash count —
  and no graph; the document is the graph.
- **The local API's node addressing** (§Control plane): every tool names a node by its `id`;
  `add_node` answers the new node's `id`; `graph` renders `id` as a spec key. A node's name is
  set by `rename_node` and never addresses anything. A port's address is
  `<machine>/<stream id>/<node id>/<port>` (decision 1).
- **Creating a project**: the scaffold writes `tatolab-project.json`, one document holding the
  starter graph, and its node modules; no `stream.py`.
- **`list_project_streams`** answers from `tatolab-project.json` without running code: each
  stream's document path, stream id and state.
- **`rm` and how a stream leaves**: `remove_stream {stream}` removes a stream from the project file
  and unloads it, leaving its document on disk; `remove_project` stays.
- **lightweight-node-kinds §What Tato Studio reads**: the edit verbs take shader and function
  types as transactions on the document.

## REMOVED

- REMOVED: sdk/tatolab-stream/scaffold_template/stream.py
- REMOVED: COMPILE_ENTRY_EVERY_STREAM_FLAG
- REMOVED: EVERY_STREAM_DOCUMENT_KEY
- REMOVED: --every-stream
  The compile entry keeps one job: run one generator for a fragment's import or refresh.
- REMOVED: of_a_project_that_gave_no_stream
  A project with no `stream.py` stops being a failed record; a project file listing no stream is
  a project with no streams.
- REMOVED: The graph compiled at the load
  `StreamRecord.graph`'s doc line; the field goes with it and the start loads the document.

## Decisions

**Decided 1 — references use stable ids; names are display labels (owner, 2026-11-07).** Links,
tools, viewers, positions and history name a node by its id. Assumed unless overruled: this covers
a port's address too, so a rename never cuts off a reader.

**[NEEDS DECISION] 2 — what the files are called, and whether generators keep `@stream`.**
- (a) `tatolab-project.json` plus `streams/<name>.graph.json`, the decorator stays `@stream`.
- (b) a `[tool.tatolab]` table in `pyproject.toml` lists the streams, documents as in (a).
- (c) one `.tatolab` file holding the project and every stream, as a `.toe` does.

Recommend (a): the runtime owns the files it writes, so it never rewrites the person's
`pyproject.toml`, and one file per stream keeps diffs and merges per stream. `@stream` keeps its
name because it still writes a graph as code, and renaming it touches every module for no gain.

**[NEEDS DECISION] 3 — how a re-run keeps a fragment's node ids stable.** The first slice must
answer it, so links into a fragment and saved positions survive a new snapshot.
- (a) An id is derived from the fragment's id and the node's name in the generator's output, so
  the same name gets the same id on every run; a node the generator renames is a new node.
- (b) The runtime matches the new snapshot to the old by type and link shape and keeps ids where
  it matches.
- (c) Generators assign ids explicitly.

Recommend (a): names are already unique within a generator's output, the rule is one line, and
(b) guesses.

**[NEEDS DECISION] 4 — where history lives in the first slice.**
- (a) The runtime appends each transaction to `<project>/.streamlib/history/<stream id>.jsonl`;
  undo and redo read it and survive a restart. The person commits the document with their own Git.
- (b) The runtime makes a Git commit in the project's repository per transaction or per gesture.

Recommend (a): the runtime never writes a person's Git history, a transaction log is what a hub
replays later, and Git commits of the document stay the person's, as a `.toe` in a repository is.

**[NEEDS DECISION] 5 — a package's stream: copied into a project, or referenced.**
- (a) Copied: opening a package's stream copies its document into the project; a package update
  never changes it. A fragment from a package is referenced by import path, as a node type is.
- (b) Referenced, so a package update reaches it.

Recommend (a), as the write-up's §Publishing has it: a stream a person opened is theirs to edit,
and a reference would make it read-only.

## Tickets (as few tracer bullets as the change needs)

Blocked on decisions 2 to 4 and on roadmap step 4 (#3214), because `exposed` leaves the spec keys
and the record there, and #3213 reshapes the graph's interface nodes.

1. **A stream loads from its graph document.** The project file and the document format with
   stable ids, `pins` and `studio`; every load reads them; `graph` renders `id` as a spec key; the
   scaffold writes a document; the location and stream id from the document's path; document
   goldens. Carries the `stream.py` REMOVED bullets except the compile entry.
2. **Edits are transactions on the document.** The eight operations and `transact`, commit or
   live, the change feed, undo and redo with an author, every tool addressing a node by id, the
   history of decision 4. Waits on 1.
3. **A `@stream` generator becomes a frozen fragment.** `import_stream_generator`, the fragment
   container in the document and in `graph`, ids per decision 3, `dev` refreshing fragments.
   Carries the compile-entry REMOVED bullets. Waits on 1; independent of 2.
4. **A project's node types and generators, listed without running code** (#3152, rewritten): for
   a project and its imports, each `@node` class, function and shader node, and `@stream`
   generator, read from source without running it, over the local API and MCP. Waits on 1.

## Notes (not in scope)

- Deferred by the write-up: the cloud hub, branches and merge rules, roles, releases and variants,
  presets and "keep current values", the live layer for running programs, unlocking a fragment, a
  CRDT, exporting a graph as a generated listing.
- Converted consumers under `examples/` and `packages/` that ship a `stream.py` get backlog at the
  consumer once ticket 1 lands; a package ships graph documents as package data afterwards.
- Tato Studio (tatolab-app) removes its `stream.py` failed-record handling, added in its #156, in
  the app change that follows ticket 1, since that ticket changes how a stream gets its id.

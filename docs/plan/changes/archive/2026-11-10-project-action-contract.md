# project-action-contract

> **Decided by the owner, 2026-11-09.** Every change to a project is one codified, strictly typed and
> versioned action, processed one way whether it comes through MCP, the CLI, Tato Studio or an SDK
> ("it should all work the same"), centralised in the runtime so multiplayer can build on it. Each
> recorded action carries a format version and a caller's idempotency key. A replaced asset goes
> through a `replace_asset` action, and replacing a file outside the runtime is unsupported, with no
> folder watcher. `create_stream` and `delete_stream` verbs are added. A stream's display name moves
> into the project as an action. The run log moves into `.tatolab/logs/`. Project files are listed
> and read through runtime verbs, and a save is a recorded `write_project_file` action. A project is
> renamed through a recorded action and duplicated or deleted through runtime verbs. Collapsing a
> selection into a subgraph and unlocking a generator's frozen output are graph actions. Tato Studio
> reads no project file itself. Studio's feedback, confirmed by the owner: a refusal comes back as
> structured data in the runtime's own words, and a client mints one idempotency key per user
> gesture. The app-side record is tatolab-app `docs/engine-guarantee.md` (tatolab/tatolab-app#201).

This change finishes what project-and-assets started: that change made every edit a transaction
with an author and an inverse. This one closes the vocabulary into one versioned action contract,
adds the actions Tato Studio needs so it never touches a project file, and moves the last piece of
project state out of the runtime's own records.

**Scale gate — this skill plus an ADR.** It changes the local API's public contract and the
on-disk transaction format.

## Current state (recon, `main` `a4574c2`)

- **A transaction has no format version and no caller key.** `ProjectTransaction` holds
  `transaction`, `author`, `parent_commit`, `operations`, `inverse_operations`, `undoes` and
  `redoes` (`runtime/streamlib-project-repository/src/project_transaction.rs:364-381`). A retried
  call applies twice.
- **Requests and records use different words.** `ProjectAssetChangeRequest` has `RenameAsset` and
  `MoveAsset` (`project_transaction.rs:70-90`), and both record `ProjectAssetOperation::RelocateAsset`
  (`:95-110`). The MCP graph tools take argument structs of their own
  (`runtime/streamlib-api-server/src/mcp_stream_graph_tools.rs:584-1039`), so MCP, the CLI and an
  SDK each spell an edit differently from the record.
- **Stream documents have operations and no verbs.** `ProjectStreamDocumentOperation` has
  `CreateStreamDocument` and `DeleteStreamDocument` (`project_transaction.rs:235-250`); no tool
  answers to them.
- **A stream's display name is the runtime's own record.** `StreamRecord.display_name`
  (`runtime/streamlib-engine/src/core/runtime/machine_state_directory.rs:43`), set by
  `rename_stream` (`runtime/streamlib-api-server/src/mcp.rs:649-671`), lives in the machine state
  directory and not in the project.
- **The run log sits outside `.tatolab/`.** `<project>/.streamlib/logs/project.jsonl`
  (`runtime/streamlib-runtime-client-contract/src/runtime_log_file_paths.rs:10-12`,
  `runtime/streamlib-engine/src/core/logging/project_log_route.rs:5`).
- **No verb lists, reads or writes a project file.** The repository walks files internally
  (`runtime/streamlib-project-repository/src/project_asset.rs:265`, `:308`). Tato Studio's view
  bridge reads and writes them on its own.
- **No verb renames, duplicates or deletes a project.** `remove_project` forgets a project's
  streams and deletes no file (`mcp.rs:625`). Building them is filed as #3406.
- **The graph's fragment operation is only a refresh.** `StreamGraphOperation::RefreshFragment`
  (`runtime/streamlib-project-repository/src/stream_graph_document.rs:166`). A fragment is a
  `@graph_generator`'s frozen output (§Processor model).

## ADDED

- **The action contract.** One Rust type per action family is the contract. MCP tools, CLI verbs
  and the SDK take an action of that type and nothing else, and the runtime parses and applies
  every action through one path. A recorded transaction's envelope gains `format_version`, an
  integer the runtime writes and reads back, and a newer runtime reads and undoes every older
  version. It also gains `idempotency_key`, a string the caller mints. A second action carrying a
  key the project's history already holds answers the first transaction instead of applying
  again. A client mints one key per user gesture, so a slider drag that commits on release is one
  key and one transaction (owner, 2026-11-09). The key is optional: an action sent without one applies, and a
  retry of it can apply twice (owner, 2026-11-10).
  A graph action names the stream it changes by its stream asset's UUID, which is the same on
  every machine the project is on, never by a runtime's stream id (owner, 2026-11-10).
- **Structured refusals.** A refused action answers a typed refusal: the action, the field or link
  it refused, and the runtime's own words. A client shows it beside what caused it and never
  rewrites it (owner, 2026-11-09). A project's change feed carries each refused action beside the
  applied transactions, tagged `refused`, with its author, the operations it asked for, its
  `mode`, its key, the refusal and when it was refused. The runtime keeps the latest 100 of
  each project in memory since it started, never in history, and announces each one through the
  same `notifications/resources/updated` (owner, 2026-11-09).
- **Stream verbs.** `create_stream {project_directory, path, display_name}` and
  `delete_stream {project_directory, stream_asset}` record `CreateStreamDocument` and
  `DeleteStreamDocument`. Creating a stream loads it with the project's other streams, and deleting
  one unloads it (owner, 2026-11-09).
- **The display name is project data.** A stream's display name is a field of its graph
  document, changed by `set_stream_display_name {stream_asset, display_name}`, a recorded action,
  so it travels with the project and undoes like any edit (owner, 2026-11-09).
- **Replacing an asset.** `replace_asset {asset, source}` swaps an asset's bytes and keeps its
  UUID, recording the new hash and size on its sidecar. A file replaced outside the runtime is
  unsupported, and the runtime watches no folder (owner, 2026-11-09).
- **Project files.** `list_project_files {project_directory, under?}` and
  `read_project_file {project_directory, path}` are verbs, the read answering the file's raw bytes
  so an image or any binary file reads as well as text, which is how Tato Studio serves a view's
  built files; and
  `write_project_file {project_directory, path, content}` is a recorded action whose inverse
  restores the earlier content, so a node-code save undoes. A path outside the project, or inside
  `.tatolab/`, is refused (owner, 2026-11-09).
- **Projects.** `rename_project {project_directory, name}` is a recorded action on
  `tatolab-project.json`. `duplicate_project {project_directory, into}` and
  `delete_project {project_directory}` are verbs, not transactions, because they act on the
  project and its history whole. Delete stops the project first, and #3406 builds both
  (owner, 2026-11-09).

  Owner, 2026-11-10: a rename changes the name alone, never the folder, which open windows,
  recents and terminals hold. A duplicate carries the project's whole history, as a copied
  repository does, under a new project UUID and the name `<name> copy`, the duplicate recorded
  as a commit on that history; its `.venv` and the rest of `.tatolab/` stay behind. A delete
  forgets the project's streams as `remove_project` does and then moves the folder to the
  platform's Trash (`NSFileManager` on macOS, the freedesktop.org home Trash on Linux), so it
  can be put back by hand.
- **How a project is shown.** A project's `description` and `cover` are fields of
  `tatolab-project.json`, so they travel with the project, and the cover names an image the
  project holds as a media asset, by its UUID; a frame taken from a Viewer is imported as an
  asset first. `set_project_description {project_directory, description}` and
  `set_project_cover {project_directory, cover}` are recorded actions, as `rename_project` is,
  and a cover asset cannot be deleted while it is the cover. When a project was last edited is
  the time of the newest commit on its history, a run never counting, a reading of history and
  no field anyone sets. `list_assets` serves the three beside the name, as `project_description`,
  `project_cover` and `project_last_edited_at` (owner, 2026-11-10; #3423).
  Renaming a project rewrites, in the same transaction, the `type` of
  every node of a view type the project brings, so one undo restores both
  (owner, 2026-11-10).
  `import_project {source, project_directory}` is a verb, not a transaction: it unpacks the
  `.zip` of a project, as `add_dependency` reads one, into a `project_directory` that does not
  exist yet, under a new project UUID, so the same zip imported twice is two projects whose
  streams never share an id, and commits it as the first commit of a history of its own; it
  runs nothing (session, 2026-11-10).
- **Subgraph actions.** `collapse_selection_into_subgraph {nodes, name}` moves the named nodes and
  the links between them into a new subgraph node, rewiring the links that crossed into
  `GraphInput` and `GraphOutput` nodes. `unlock_generator_fragment {fragment}` replaces a
  generator's frozen fragment with its nodes and links as plain, editable nodes of the enclosing
  graph, after which no refresh reaches them. Both are graph actions with inverses
  (owner, 2026-11-09).

  A collapsed subgraph is an **authored fragment**: a fragment node with no generator, whose inside
  is edited like any graph and which no refresh touches. A subgraph therefore comes three ways: a
  generator's frozen output, another stream placed by reference through its Graph Input and Output
  nodes, and an authored fragment. Owner, 2026-11-09: "subgraphs aren't limited to generators, they
  also extend to using a stream as a node".
  Rejected: a collapsed selection written out as a Python generator — it writes graph back into
  code, which the plan rejects (owner, 2026-11-09).

## MODIFIED

- §Processor model, the project-and-assets entry: "every change … is a transaction" gains the
  format version and the idempotency key, and the subgraph entry admits an authored fragment
  beside a generator's output and a referenced stream.
- §Product, the stream actions: `rename_stream` becomes `set_stream_display_name`, a recorded
  action. `create_stream` and `delete_stream` join the tools.
- §Product, the stream's location, id and display name: the display name lives in the graph
  document, so a re-load keeps it because the document keeps it.
- §Control plane: every tool's arguments are the action contract's types. `rename_asset` and
  `move_asset` become one `relocate_asset {asset, to_path}`, so the request and the record share a
  name (session, 2026-11-09).
- The project log: `<project>/.tatolab/logs/project.jsonl`. The history's own walk already skips
  `.tatolab/`, so the log stays out of history.

## REMOVED

- REMOVED: ProjectAssetChangeRequest
- REMOVED: rename_stream
- REMOVED: project_directory.join(".streamlib")

  `StreamRecord.display_name` also goes. It has no unique spelling to grep, so the ticket's own
  test proves the record no longer carries it.

## Consumers

Tato Studio (tatolab-app) drives every edit through the one SDK and drops its own file reads.
That is backlog at tatolab-app (#202–#211 name it) and never blocks this change.

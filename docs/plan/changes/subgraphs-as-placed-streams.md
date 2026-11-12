# subgraphs-as-placed-streams

> **Decided by the owner, 2026-11-11 (project thread).** A subgraph a person makes is a stream
> placed by reference, as After Effects' Pre-compose makes a composition of layers. Collapse saves
> the selection as a new stream of the project and places it where the nodes were. Stepping inside
> edits that stream, and every stream placing it takes the change. A stream another stream of the
> project places runs only where it is placed. Unlock copies a placed stream's nodes back into the
> parent as plain nodes. The inline authored subgraph is retired; a graph generator's frozen output
> stays as it is. The owner chose these on decision cards and approved the plan text as written.

## Current state (recon, `main` `66b2277`)

- **Collapse makes an inline fragment.** `the_fold_of_nodes_into_an_authored_fragment` builds a
  `tatolab.project:AuthoredFragment` node holding the selection's nodes and links, with a
  `GraphInput` or `GraphOutput` inside for each crossing link
  (`runtime/streamlib-project-repository/src/stream_graph_fragment_fold.rs:22`, `:73-257`).
  `collapse_selection_into_subgraph` sends it
  (`runtime/streamlib-api-server/src/mcp_stream_graph_tools.rs:795`).
- **An edit at a level is made inside the inline fragment.** `the_edit_at_the_level` wraps the edit
  in `StreamGraphOperation::EditInsideFragment`, carrying outside links over a renamed port, and
  refuses a placed stream (`stream_graph_fragment_level.rs:34-106`, `:160-190`;
  `stream_graph_document.rs:245`).
- **A placed stream is a reference resolved at load.** A `tatolab.project:StreamFragment` node holds
  `{"stream": {"asset", "from"?}}`. Each load copies the named stream's nodes into the placing
  stream under UUIDv5(fragment node, own UUID) (`stream_fragment_references.rs:186-247`,
  `:358-363`). A committed edit re-places every loaded stream that holds a placed stream
  (`runtime/streamlib-api-server/src/stream_graph_transaction_executor.rs:222-279`).
- **A project's own stream can already be placed**, by `add_node` with type
  `tatolab.project:StreamFragment` and no `from` (`stream_tools_over_a_real_engine_tests.rs`, the
  test placing a stream of the same project). There is no dedicated verb; `use_dependency_stream`
  always writes `from`.
- **Every stream asset loads as its own stream.** The project scan reads every `streams/` document
  and skips `imports/` (`runtime/streamlib-project-repository/src/project_asset.rs:268-278`;
  `runtime/streamlib-engine/src/core/runtime/stream_actions_of_this_runtime.rs:163`, `:240`). A
  placed stream therefore runs once per placement plus once on its own.
- **`unlock_generator_fragment`** unfolds a generator's fragment into plain nodes and refuses a
  placed stream (`mcp_stream_graph_tools.rs:829`; `stream_graph_fragment_fold.rs:258`).

## ADDED

- **Collapse saves a stream.** `collapse_selection_into_subgraph` writes the selection as a new
  stream asset of the project, `streams/<name>.graph.json` named for the subgraph, holding the
  selected nodes and their links and a `GraphInput` or `GraphOutput` for each link that crossed
  the selection. It then places that stream where the nodes were, with a `StreamFragment` node
  whose ports are those Inputs and Outputs, the crossing links moved onto them. It is one graph
  action with an inverse that removes the stream asset again. A name a stream asset already holds
  is refused, naming it.
- **A placed stream runs only where it is placed.** Loading a project skips each stream asset
  another stream of the project places, as it skips an import's streams. A stream placed nowhere
  loads as before. `dev` re-places each stream that places a stream whose document changed on disk.
  The rule holds after every committed edit too, not only at a load: a stream running as its own
  that an edit places is unloaded as its own and runs only inside the stream placing it, and a
  stream whose last placement an edit removes loads as its own stream, following the project's
  playback (session, 2026-11-12).
- **An edit at a level edits the placed stream.** An edit whose `level` ends at a placed stream is
  made on that stream's own document, the nodes it names read from the placing graph's UUIDv5 ids
  back to the placed stream's own. Every loaded stream placing it is then re-placed. History records
  the edit as an operation on the placed stream's asset, so undo reverses it there.
- **Port links follow in every placing stream.** An edit inside a placed stream that renames an
  Input's or Output's port carries each link onto that port in every stream of the project placing
  it to the new name, and one that removes the port takes those links with it, the inverse
  restoring each. Every stream it touches changes in the one transaction.
- **`unlock_subgraph`** turns a generator's fragment or a placed stream back into plain nodes of the
  graph placing it. A placed stream's nodes come back as a copy under fresh UUIDs, its reference
  dropped. The placed stream's asset stays, and it loads on its own again once nothing places it.

- **An inline subgraph is refused at load.** A graph document holding an `AuthoredFragment` node is
  refused, naming the node. Pre-1.0 there is no conversion; such documents exist only from
  collapses made since #3459.

## MODIFIED

- `unlock_generator_fragment` is renamed `unlock_subgraph`, in the MCP tool set, the control
  contract, `StreamGraphAction` and its CLI verb, and its description covers both kinds.
- `the_edit_at_the_level` resolves a path through placed streams. A path through a generator's
  output is refused as today, until it is unlocked.

## REMOVED

- REMOVED: AUTHORED_FRAGMENT_NODE_TYPE
- REMOVED: tatolab.project:AuthoredFragment
- REMOVED: the_fold_of_nodes_into_an_authored_fragment
- REMOVED: an_authored_fragment_of_this_level
- REMOVED: EditInsideFragment
- REMOVED: apply_the_edit_inside_a_fragment
- REMOVED: unlock_generator_fragment
- REMOVED: UnlockGeneratorFragment
- REMOVED: runtime/streamlib-project-repository/tests/an_edit_reaches_the_inside_of_an_authored_fragment.rs
  Its cases move to a test of edits inside a placed stream: add, connect and configure, a name
  clash per level, a removed Output, a renamed port, a nested path, the refusals.

## Consumers

- **Tato Studio (tatolab-app).** It already sends `level` and steps in by subgraph id, so those
  fields do not change. The tool `unlock_generator_fragment` is renamed `unlock_subgraph` with the
  old name removed, so Studio calls `unlock_subgraph`. It stops treating a placed stream as
  read-only and drops the `authored` kind:
  stepping into a placed stream edits it, and Unlock shows on it. Properties gains editing of a node
  inside a subgraph at its level. It is filed as a tatolab-app issue that waits on this change.

## Tickets (tracer bullets)

1. **A placed stream runs only where it is placed.** Covers the load skip and `dev`'s re-place.
   Proof: a project where `main` places `look` loads one stream, and `look`'s nodes run once.
2. **Collapse saves a stream and places it; `unlock_subgraph`.** Covers the new collapse, the
   rename, unlocking a placed stream, the `AuthoredFragment` removals and the refusal at load.
   Proof: collapse, undo, redo and unlock against a real engine.
3. **An edit at a level edits the placed stream.** Covers edits through placed streams, port links
   followed in every placing stream, history on the placed stream, and the `EditInsideFragment`
   removals. Proof: edits inside a stream placed twice reach both placements and undo exactly.

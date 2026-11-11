# kept-node-values

> **Decided by the owner, 2026-11-11 (project thread).** Each node value has a lifetime: `kept`
> values survive Stop, Play, close and a runtime restart; `reset` values are forgotten at Stop.
> The state board becomes the one node value store, keyed by node UUID, and holds any
> serializable value. Memory is the truth at full rate; one writer per project flushes kept
> values to `.tatolab/` on a debounce. The first slice makes a top-level `GraphInput` a kept
> value by default, so a show's controls stay where the person set them. Explored in
> `docs/research/explorations/node-values-that-survive-stop.md`; replaces tatolab/streamlib#3486's
> "uncommitted live edits" framing.

## Current state (recon, `main` `93c2ba6`)

- **A Graph Input's value lives in its processor.** `ProjectViewNode::for_a_top_level_graph_input`
  seeds `current_value_of_each_output` from the config's `default`
  (`runtime/streamlib-engine/src/core/processors/project_view_node.rs:142-176`), a write replaces
  it (`core/runtime/view_node_output_written_by_a_panel.rs:57-76`), and a late reader is handed it
  (`project_view_node.rs:258`). The processor is rebuilt at each Play, so the value returns to the
  `default` at Stop, a reload and a restart.
- **The state board** keeps a stateful function node's state per stream, by node name, as a record
  of numbers (`core/runtime/function_node_states_carried_across_loads.rs:22-28`, `:91`), carried
  across helper restarts, reloads and a runtime restart in the stream's record
  (`core/runtime/loaded_stream.rs:296`, `:965`, `:1421`). Stop forgets it (playback-controls,
  owner, 2026-11-11).
- **A live graph edit is a debounce, already committed on release.** Tato Studio's Properties
  sends `mode: "live"` while a field moves and one `commit` when it lets go
  (tatolab-app `packages/tato-app-core-views/src/properties/graph_edits_in_the_open_project.ts:86-105`).
  Nothing about it changes here.

## ADDED

- **A node value and its lifetime.** A node value is a serializable value the runtime keeps for
  one placed node instance, keyed by the node's UUID: a function node's `State` or a top-level
  `GraphInput`'s value. Each has a lifetime: `kept` (survives Stop, Play, close and a runtime
  restart) or `reset` (forgotten at Stop). Two placed instances never share one; a rename keeps it
  (owner, 2026-11-11).
- **Defaults.** A function node's `State` is `reset`, as stateless nodes and today's nodes
  expect. A top-level `GraphInput` is `kept` (owner, 2026-11-11). A `GraphInput`'s config takes an
  optional `lifetime: "kept" | "reset"`; `GraphInputConfig` gains `lifetime: NotRequired[...]`.
- **Any serializable value.** A kept value is the value the node's family takes, saved as JSON: a
  Records object, an Array number or list, a Channels number, block or bool (owner, 2026-11-11).
  Function-node state keeps its numeric slots on the board's wire; only its saved form is JSON.
- **The project database.** Each project has one embedded database, `.tatolab/project.redb`
  (redb: one file, ACID and crash-safe, pure Rust), owned by one engine module and written only by
  the runtime. It is the project's backing store for runtime-kept data: each concern is a named
  table in it, never a file or a store of its own (`ProjectStore`, `ProjectStoreTableName`). It
  holds no media, which stays as files in the project (owner, 2026-11-11: "start with redb and
  evaluate how it goes"; "design it in a centralized way that if anyone else runs into needing
  state as well they can use this store").
- **`ctx.store()`, a node's own keys.** A node's runtime context hands it `store()`, a
  `StoreScopedToThisNode`: `read`, `write` and `remove` of keys under the node's UUID in the
  store's `node_store` table, so no node reaches another's, as a Durable Object reaches only its
  own storage (owner, 2026-11-11, the name `store()`). A kept `GraphInput` keeps its value there
  under `value`. Python nodes are handed it with ticket 2, with its Protocol in `tatolab.stream`.
- **Memory at full rate, a debounced writer to disk.** A write changes the value in memory at
  once and readers take every change, at whatever rate a control moves. One writer per held
  project commits the latest value of each changed kept key at most once per debounce window,
  every value changed in that window batched into one write transaction, so the disk never runs
  at a control's rate and never sees concurrent writes. The writer runs off every processing and
  request path: a control write never waits on disk. Stop, `close_project` and a clean runtime
  shutdown flush at once. A crash loses at most one window. Nothing reaches the graph document or
  Git history (owner, 2026-11-11).
- **Restore.** A kept node reads its keys through `ctx.store()` at setup; Play hands each kept value
  to its node before the first frame, so a `GraphInput` publishes the kept value instead of its
  `default`. A key whose node no longer exists, or whose value its family no longer takes, is
  dropped with a log record naming it.
- **`graph` shows a kept value while stopped.** A `GraphInput`'s `current_value` reads from the
  store, so a stopped project's controls still show where they stand, with its `lifetime`.

## MODIFIED

- live-graph-inputs, "a write is live data … gone on the next load": a write to a `kept` Input
  is kept as above; a `reset` Input behaves as written there. A write is still never a graph edit,
  in the document or in history.
- playback-controls, "Stop forgets function-node state": Stop forgets every `reset` value; a
  `kept` value stays.
- The state board (lightweight-node-kinds) is keyed by node UUID instead of node name, and its
  carried state is held in the project's node value store, not the stream's record.
- §Processor model: the function-node-state and Graph Input entries say the above.
- Glossary: **Node value** (a value the runtime keeps per placed node, `kept` or `reset`).

## REMOVED

- REMOVED: by_function_node_name
- REMOVED: and the value is gone on the next load

## Assumed

The owner approved the proposal with these open; they are the session's recommendation, built
as stated until the owner says otherwise (session, 2026-11-11).

1. **The debounce window is 250 ms.** Short enough that a crash loses a blink, long enough that a
   60 Hz slider writes four times a second at most.
2. **A write to a stopped project's `kept` Input is accepted** and stored, so a person can set up
   a show's controls before pressing Play; a `reset` Input is still refused as `playback_stopped`.
3. **Save as default stays a commit.** Keeping a value as the Input's `default` is a
   `set_node_config` a client sends; this change adds no verb for it.
4. **Only Graph Inputs in the first ticket.** Function-node `State` moving to UUID keys and the
   project store, with a declared `kept` lifetime, is the second ticket.

## Tickets

1. **Kept Graph Input values** — the project store and its debounced writer, `ctx.store()` in
   Rust, a kept `GraphInput` restoring its value at setup and committing at teardown, `lifetime`
   on `GraphInputConfig`. Ticket 1b follows with: a stopped project's `graph` reading the store,
   writes to a stopped kept Input (Assumed 2), the store closed with `close_project` once that verb
   lands, and dropping the keys of a node the graph no longer holds. Engine tests over a real engine:
   a kept Input written then Stop, Play reaches its readers with the written value; a reset Input
   returns to its `default`; 1,000 writes in one second commit at most four write transactions and
   the database holds the last; a restart restores the kept value.
2. **Function-node state in the store** — the board keyed by UUID, carried state moved from the
   stream record to the store, `kept` declarable on `State`.

## Consumers

Tato Studio's Perform controls (tatolab-app #209) read `lifetime` and `current_value` from
`graph`; showing which controls are kept is app backlog, filed once ticket 1 ships.

# Node values that survive Stop

Exploration, 2026-11-11, `main` `93c2ba6`. Status: **open with the owner**, not yet graduated. It
came out of tatolab/streamlib#3486 ("live changes, not saved").

## The idea

A person at a show plays a project and sets the lights with Perform's controls. They press Stop,
and the lights jump back to the Graph Input's `default`. That is the defect in the person's eyes,
and #3486's "uncommitted live edits" was the wrong framing of it:

- A **live graph edit** (`mode: "live"`) is a debounce. Tato Studio's Properties sends `live` while
  a field is dragged and one `commit` on release
  (tatolab-app `packages/tato-app-core-views/src/properties/graph_edits_in_the_open_project.ts:86-105`),
  so a finished edit is already in the document and in history. It needs no list and no verbs.
- A **Perform control's value** is not a graph edit. `write_graph_input` hands it to a running
  `GraphInput`, and it is gone on the next load
  (`runtime/streamlib-api-server/src/mcp_project_view_tools.rs:173-176`). Since playback-controls,
  Play builds fresh processors, so it is also gone at Stop.

What is missing is a way to say whether a value is **stateful** (kept through Stop, saved with the
project) or **stateless** (reset to a known default at Stop). The owner's shape: something like
Durable Objects' per-id storage or reactive values, a store a value can always be written to and
read from by its id, the node's UUID.

## What already exists

- **The state board** (`docs/plan/changes/lightweight-node-kinds.md`, "The state board";
  `runtime/streamlib-engine/src/core/runtime/function_node_states_carried_across_loads.rs`). A
  stateful function node (`Lfo`, `Lag`, `Random`'s seed) keeps its state there. It is per stream
  and keyed by node name, one entry per placed instance, so two `Lfo`s never share a phase; a
  rename loses the state. It is carried across a helper restart, a promotion, a reload and a runtime restart, kept in the
  stream's record and shown by `graph`. Its values are numbers in a typed record.
- **Stop forgets function-node state** (playback-controls, owner, 2026-11-11).
- **A Graph Input holds a current value in memory**, renders it in `graph` as `current_value`,
  and hands it to a late reader (live-graph-inputs, Assumed 1-2).

So a per-node value store already exists, and the doctrine is to extend it rather than build a
second one next to it.

## Shapes considered

1. **Overload the Graph Input** with keep/reset, persistence and save-as-default. Rejected by the
   session: it is a second state system, and the next node that needs kept state would have to
   build a third.
2. **A new project value store** beside the state board. Rejected by the session for the same
   reason: two stores keyed by node, one for function state and one for inputs.
3. **Extend the state board into the one node value store (recommended).**
   - Key it by node UUID instead of name: one entry per placed instance, as a person placing two
     `Lfo`s expects, that follows its node through a rename (owner, 2026-11-11).
   - Each value declares a **lifetime**: `kept`, saved with the project and restored at Play, a
     runtime restart and a crash; or `reset`, forgotten at Stop as today.
   - A `GraphInput` is a stateful node whose state is its value. `write_graph_input` writes that
     state, and the node publishes whenever it changes. Its `default` is the state before any
     write, and its config declares the lifetime.
   - A function node declares the lifetime of its `State` the same way, so "Stop forgets
     function-node state" becomes the `reset` lifetime and stops being a rule for every node.

## Open forks for the owner

1. **Where kept values are saved — settled.** Under `.tatolab/`, outside Git history. Memory is
   the truth at full rate: a DJ dragging a slider changes the value as often as it moves, and
   readers take each change. One writer per project flushes the latest value of each changed key to
   the file on a debounce, the way a log writer batches, so writes never run at the slider's rate
   or race each other; Stop, close and a clean shutdown flush at once. A crash loses at most one
   debounce window. "Save as default" stays a separate `set_node_config` commit (owner,
   2026-11-11).
2. **The default lifetime — settled.** A function node's `State` resets at Stop, as stateless
   nodes and today's nodes expect; a Graph Input keeps (owner, 2026-11-11).
3. **Values the board cannot hold — settled: any serializable value, as JSON (owner, 2026-11-11).** The board holds numbers. A Records Graph Input holds
   strings and objects too. Either the store grows to hold any family value, or only numeric
   values can be kept in the first slice.

## Smallest start

An **MVP slice**: a Graph Input's lifetime and kept values only. Key the board by UUID, add the
lifetime to `GraphInput`'s config, save `kept` Input values to the project store, and restore them
at Play and at the runtime's start. Engine test: write a kept Input, Stop, Play, and the readers
see the written value; do the same with a reset Input, and they see the `default`.

Deferred: function nodes' `State` lifetime, Records values (if fork 3 goes that way), Studio
showing which controls are kept, and save-as-default in Studio.

## Exit

Graduated to `docs/plan/changes/kept-node-values.md` with every fork settled (owner, 2026-11-11). It amends playback-controls'
"Stop forgets function-node state" and live-graph-inputs' "gone on the next load", so the delta
is a change proposal against both.

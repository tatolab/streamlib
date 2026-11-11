# live-graph-inputs

> **Decided by the owner, 2026-11-11.** A stream's top-level `GraphInput` runs: it is a source
> node with one `value` output, linked to every port that reads it. A local API verb names the
> stream by its stream id and the `GraphInput` by its name and writes a value of its family. The
> node publishes it to every reader. The last write wins, nothing is recorded, and the value is
> gone on the next load. Until the first write, readers see the `default` the `GraphInput`'s config
> holds, or nothing when it holds none. Tato Studio's Perform layout drives its controls through
> this verb (tatolab/streamlib#3485, tatolab/tatolab-app#209).

## Current state (recon, `main` `81849a1`)

- **A top-level `GraphInput` runs nothing.** `with_graph_interface_resolved` takes every interface
  node and each link touching one out of the graph the runtime runs
  (`runtime/streamlib-engine/src/core/graph_interface_resolution.rs:135-275`), keeping what each
  declares in `GraphInterfaceAsDrawn` so `graph` renders it back (`:345-393`). It is the last pass
  of `with_graph_only_nodes_resolved` (`core/graph_snapshot.rs:415-440`), after fragments
  flatten their own interface (`core/graph_fragment_flattening.rs:184-207`). A top-level Input's
  readers are left with an unlinked input nothing can feed.
- **An interface node takes only `port` and `family`.** Any other config key is refused
  (`graph_interface_resolution.rs:559-573`); `GraphInputConfig` mirrors it
  (`sdk/tatolab-stream/tatolab/stream/_graph_interface_nodes.py`).
- **A live, unrecorded write exists for view nodes only.** `write_studio_view_output`
  (`runtime/streamlib-api-server/src/mcp_project_view_tools.rs:134-160`) hands a value to a
  running view node (`core/runtime/view_node_output_written_by_a_panel.rs:23-63`), which encodes
  it per family and publishes it (`core/processors/project_view_node.rs:137-184`, `:214-233`).
  The seam refuses any node not of a view type (`view_node_output_written_by_a_panel.rs:84`).
- **Pins cannot carry it.** A process node's pin is a Channels number or bool
  (`core/compiler/compiler_ops/input_port_pin_values.rs:141-170`), a pin is recorded graph
  state, and a pin never wakes a reactive reader.
- **No late reader is resent anything.** A channel keeps no history for a reader linked after a
  write (`iceoryx2/output.rs:1155`); `has_a_reader_the_last_write_missed` (`:589`) tells a writer
  one is waiting.

## ADDED

- **A top-level `GraphInput` is a running node.** At the stream's top level, interface resolution
  keeps each `GraphInput` and its links in the graph the runtime runs. It runs as a reactive source
  processor with one output, `value`, of the family its config declares. `GraphOutput` and every
  interface node inside a fragment still resolve out exactly as now: a fragment's Input is where
  the placing graph's link enters, and an Output is where a port leaves.
- **Writing a Graph Input.** `write_graph_input {stream, input, value}` names the stream by its
  stream id and the `GraphInput` by the port name its config declares, and hands `value` to the
  node, which publishes it on `value` stamped with stream time now. The value is read per family
  exactly as `write_studio_view_output` reads one: a Records value is a JSON object, an Array value
  a number, a list of numbers or an object of such attributes, a Channels value a number or a
  block. Many clients may write one Input and the last write wins. A write is live data, never
  recorded in the graph document or the project's history, and gone on the next load. Refused for
  a stream that is not running, a name no top-level `GraphInput` declares, a value its family does
  not take, and an Image or Packets Input. The result names the stream, the input and its family.
- **A Graph Input's default.** A `GraphInput`'s config takes an optional `default`, a value of its
  family read as a write is, refused at load when its family does not take it. The node publishes
  it when it starts, so a reader sees it until the first write and again after every load.
  `GraphInputConfig` gains `default: NotRequired[Any]`.
- **One seam for values written from outside.** The view node's write path becomes the engine's
  one path for a node whose outputs a client writes: one family encoder and one engine call, whose
  two node types are a project view and a top-level `GraphInput`. No second write path is built
  beside it.

## MODIFIED

- §Processor model & scheduling, the interface entry ("A graph's interface is its Input and Output
  nodes, at every level"): a stream's top-level Inputs run as source nodes a client writes; its
  Outputs, and every interface node inside a fragment, run nothing.
- §Control plane & observability: `write_graph_input` joins the tools beside
  `write_studio_view_output`.
- `graph` renders a top-level `GraphInput` exactly as now — its config, its one `value` output of
  the declared family, its `graph_interface` component and its drawn links — so Tato Studio finds
  each Input and its family where it does today. It now also carries the node's running state.

## REMOVED

- REMOVED: An interface node runs nothing.
- REMOVED: so no panel writes its outputs

## Assumed

The owner approved the proposal with these four open; each is the session's recommendation,
built as stated until the owner says otherwise (session, 2026-11-11).

1. **A reader linked or restarted after a write gets the current value.** The node holds its
   current value and resends it to a reader the last write missed
   (`has_a_reader_the_last_write_missed`), so a restart never silently empties a control. It lives
   in the shared seam, so view nodes gain it too.
2. **Studio reads the current value through a viewer on `value`,** the existing `open_viewer`,
   which 1 makes see the last value; `graph` carries no written value.
3. **A `default` on a fragment's `GraphInput` is refused,** naming the node: a fragment's Input
   runs nothing, so a default there would be a separate change.
4. **No control-kind hint here.** Studio renders a control by family until slider, toggle and
   colour hints are agreed.

## Consumers

Tato Studio's Controls view in Perform (`packages/tato-app-core-views/src/controls/`) waits on
this verb and reads each Input from `graph` as it does now. That is tatolab-app #209, backlog that
never blocks this change.

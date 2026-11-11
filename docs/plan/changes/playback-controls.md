# playback-controls

> **Direction from the owner, 2026-11-11 (#3542).** `start_project`, `stop_project`,
> `pause_project` and `resume_project` are **playback** controls, the transport of Unreal's
> play-in-editor or DaVinci Resolve, never the project's lifecycle. Start (Play) starts the
> flow and the clock; Stop ends it and resets the clock; Pause holds it where it stands;
> Resume carries it on. None of them changes whether the project is open, readable or
> editable. A playback verb that takes away the ability to build in the project is the defect.
>
> **Decided by the owner, 2026-11-11 (project thread).** Borrow from game engines: Stop is
> Unreal ending play-in-editor — every processor instance and every piece of run state is
> removed, and Play always starts fresh like a level, keeping only what a node itself wrote to
> persist between runs. The project's lifecycle is its own pair of verbs, `open_project` and
> `close_project`; the playback verbs are `start_playback`, `stop_playback`, `pause_playback` and
> `resume_playback`, and nothing that loads, reads or refreshes a project ever happens inside
> one. Pause is a distinct, recorded state that pauses everything, the clock included — a
> later timeline view drives these controls. Function-node state is forgotten at Stop.

This reverses a DECIDED entry: "`stop_project` (unload every stream of the project; record each
`stopped`)" (owner, 2026-11-09) and "The project verbs are Docker's container verbs" (owner,
2026-11-10). It is a pivot of §Product's stream-action entries and touches the local API's
public contract, so it carries an ADR.

## Current state (recon, `main` `5164481`)

- **Stop unloads.** The queued `Stop` body (`runtime/streamlib-engine/src/core/runtime/stream_actions_of_this_runtime.rs:1292-1333`)
  records each stream `stopped` (`:1501-1513`) and then `unload_a_stream_an_action_took_back`
  (`:3410`) takes it out of the stream table.
- **Every build tool needs a loaded stream.** The one chokepoint is
  `Runner::loaded_stream_of_the_id` (`runtime/streamlib-engine/src/core/runtime/runtime.rs:1051-1062`),
  reached through `runtime_operations_of_the_stream_a_call_names`
  (`operations_on_the_streams_loaded_in_this_runtime.rs:171-177`). `graph` refuses at
  `runtime/streamlib-api-server/src/mcp.rs:463`, the node catalog at `handlers.rs:320`, and
  `transact` and every single-op edit at `mcp_stream_graph_tools.rs:1679-1684`, because it applies
  each operation to the live graph first (`:1789-1796`) and only then to the document.
- **Undo, redo and the transactions feed already work stopped.** They are project-scoped; undo
  skips stream assets with no loaded stream (`mcp_stream_graph_tools.rs:1985-1989`).
- **The engine has a loaded-not-started stream, and no production path leaves one there.**
  `LoadedStreamInThisRuntime::new` starts at `RuntimeStatus::Initial` with a compiler graph and no
  `RuntimeContext` (`loaded_stream.rs:96-105`, `:264`). Edits made in it are queued and committed
  at `start()` (`graph_change_listener.rs:46-50`, `loaded_stream.rs:586-587`). Tests build one
  (`a_recorded_stream_loaded_without_its_start`, `stream_actions_of_this_runtime.rs:3869`), and on
  it `graph` answers and `open_viewer` refuses "is not running, so its port … writes nothing yet"
  (`output_port_viewer.rs:309-314`).
- **`LoadedStreamInThisRuntime::stop` empties the graph and is one-way.** It queues
  `RemoveProcessor` for every node (`loaded_stream.rs:680-689`), and `start()` refuses a stream
  that shut down: "a stream that shut down is loaded again rather than restarted" (`:538-544`).
- **`list_streams` reads state from loadedness.** A loaded stream is `running` or `paused`
  (`stream_actions_of_this_runtime.rs:1577-1581`); `stopped` comes only from a record, with
  `node_count: null` (`:1587-1600`, `machine_state_directory.rs:147-155`).
- **A restart skips stopped records** (`stream_actions_of_this_runtime.rs:1689-1695`, `:1825-1826`),
  so a stopped project comes back unloaded.
- **Pause holds processors, not the clock.** `pause()` closes each processor's pause gate
  (`loaded_stream.rs:1552-1576`, `:1403-1437`); `StreamClock` has no hold, so a real or scaled
  clock keeps advancing. Pause is never recorded (`stream_actions_of_this_runtime.rs:500-503`).
- **Function-node state is carried across loads** on purpose
  (`function_node_states_carried_across_loads.rs:4-12`), recorded at a stream's end
  (`loaded_stream.rs:881-892`).
- **A viewer's not-running answer is prose** (`output_port_viewer.rs:309-314`, an `Error::Link`).

## ADDED

- **A project the runtime holds, and its playback — two axes.** The runtime **holds** a
  project: its repository is open and each stream asset that parses is a loaded stream, its
  graph model in memory with its nodes, links, config, resolved node types and catalog. Holding
  is entered by `open_project`, `create_project` and the runtime's start for every recorded
  project, and left by `close_project`, `remove_project` and `delete_project`. Separately,
  each held project has a **playback state**: `stopped`, `playing` or `paused`, with `preparing`
  and `stopping` while a queued action works.

  | playback | graph model, types, catalog, history | processors, helper nodes, runtime context, GPU surfaces, devices | stream clock |
  |---|---|---|---|
  | stopped | held | none | none; the next Play starts at zero |
  | playing | held | running | running |
  | paused | held | held, gated | held where it stands |

- **The lifecycle verbs.** `open_project {project_directory}` / `tatolab open` is the one
  operation that brings a project into the right state to build in: a queued stream action that
  runs, in order, the small project operations it is made of — sync its dependencies, make its
  venv (`uv sync`), read its documents, describe its types — and holds every stream asset
  stopped. Each of those is a verb of its own an agent or Studio can call alone, and
  `reload_project` / `tatolab reload` runs them again on a held project in any playback state,
  keeping that state. Opening reports each step on its action, as a load does today, so Studio
  opens the window at once and shows a loading state from the action's progress.
  `close_project` / `tatolab close` stops playback when it plays and lets the project go,
  recorded closed so a restart leaves it closed; it deletes nothing — the venv, synced
  dependencies, caches and files stay. `remove_project` closes and forgets.
- **The playback verbs never load.** `start_playback`, `stop_playback`, `pause_playback` and
  `resume_playback` (`tatolab play`, `stop`, `pause`, `resume`) act on a held project only and
  never read a document, make a venv, describe a type or load a stream; a project not held is
  refused naming `open_project`.
- **Stop ends the flow and keeps the project.** `stop_playback` tears down every processor
  instance, helper node instance, GPU surface, device claim, function-node state and the clock,
  and leaves each stream's graph model in place, recorded `stopped`. A camera, a microphone and a
  GPU are free after Stop exactly as today. What a node wrote itself to persist between runs (a
  file, a project asset) is untouched.
- **Play runs what was built, fresh.** `start_playback` of a stopped project instantiates fresh
  processors from the held graph, edits made while stopped included, on a new clock — a level
  started again.
- **Pause pauses everything.** `pause_playback` gates every processor and holds the stream clock;
  `resume_playback` carries both on. Pause is recorded, so a restart brings the project back
  paused.
- **Everything a person builds with answers for a stopped project.** `graph` (nodes read
  `state: null` with no metrics, and the result carries `playback`), `transact` and every
  single-op edit, `undo` / `redo`, the transactions feed, the node catalog, `list_project_node_types`,
  studio fields, groups, notes, generators, subgraphs and dependency streams. A stopped edit is
  validated against the held graph exactly as a playing one is and committed to the document
  and the history the same way; it compiles at the next Play.
- **`list_streams` reports playback.** `stopped` now means held and not playing, with a
  non-null `node_count`. The spellings of `ListedStreamState` stay.
- **A not-running answer is structured.** `open_viewer`, `write_studio_view_output` and
  `write_graph_input` on a node that is not running answer the existing
  `structuredContent.refusal` envelope (`ProjectActionRefusalOnTheWire`,
  `local_api_wire_contract.rs:798-818`) with `not_running_because`: `playback_stopped`,
  `playback_paused`, `preparing` or `node_failed`, the runtime's words kept beside it. Studio
  shows a calm placeholder for the first three. [Decided §6]
- **Playback survives a restart.** The runtime's start holds every recorded project not closed
  and restores its recorded playback: playing, paused or stopped.
- **A failed stream stays editable.** A stream whose code fails to describe is still held from
  its document, its unknown types as placeholder nodes keeping their saved config and ports,
  shown `failed` with the reason; a document that will not parse is the only stream not held.
- **Edits and Play are ordered.** A Play reads the held graph after every edit committed before
  it was queued, so an edit landing while a Play prepares is either in that run or refused with
  the reason, never committed and missing from the run.
- **Glossary.** **Playback** (stopped / playing / paused) and **Hold** (a project the runtime
  holds, readable and editable) are added; **Stream**, **Project** and **Stream action** are
  reworded so no definition says a stop unloads.

## MODIFIED

- §Product, `tatolabd` at start (ARCHITECTURE.md:125-130): "re-loads every recorded stream
  neither stopped nor failed" → holds every recorded project not closed and restores its
  recorded playback.
- §Product, "How a stream is loaded and kept" (:232-239): `tatolab open` holds a project,
  `tatolab close` lets it go, `tatolab play` / `stop` / `pause` / `resume` drive its playback,
  and `tatolab rm` forgets it.
- §Product, "The stream actions, as built" (:260-271): the tool list carries `open_project`,
  `close_project`, `reload_project`, `remove_project` and the four `*_playback` verbs;
  `list_streams` states as above.
- §Product, "Every loaded stream persists" (:310-316): only `close` and `rm` let a held stream
  go; `stop` ends its playback.
- §Product, "Loading a project loads every stream in it" (:364-370): every load names
  `open_project`, `reload_project` and `create_project`; the project *plays* and stops as a whole.
- §Product, "Every load and unload is a queued stream action" (:413-479): retitled "Every
  open, reload, close and removal is a queued stream action". The playback verbs are not loads:
  each acts on the held project at once, never behind a load. The refusal table (:431-443)
  becomes the playback table — `start_playback` of a playing project refused naming
  `reload_project`, of a paused one naming `resume_playback`; `pause_playback` /
  `resume_playback` of a stopped one refused; every playback verb on a project not held refused
  naming `open_project`. The Docker sentence goes. Pending marks (:464-468): a pending close
  finishes as closed.
- §Processor model, stream clocks (:1564-1587): the clock is built at each Play and dropped at
  Stop, so Stop resets it, and Pause holds it where it stands.
- §Processor model, function-node state carried across loads
  (`docs/plan/changes/archive/*lightweight-node-kinds*`, ARCHITECTURE.md §Processor model).
  Stop forgets it.
- §Media I/O, the capture permission re-ask (:2620-2625): Stop then Play asks again, unchanged
  in effect.
- §Control plane, the tool list and "`stop_project` ends a client's work" (:3986-3993):
  `stop_playback` ends a client's playback; `close_project` frees the project.
- The MCP descriptions of the project verbs (`mcp.rs:643`), `list_streams` (`mcp.rs:739`), the
  graph tools (`mcp_stream_graph_tools.rs:548`), the server prompt (`mcp_prompts.rs:187`), the
  `ListedStreamState::Stopped` doc (`local_api_wire_contract.rs:188`), the CLI's stop and streams
  text (`runtime/tatolab-cli/src/stream_actions_on_the_runtime.rs:284-339`), and the
  `stop-running-stream` and `list-running-streams` skills.

## REMOVED

- REMOVED: `unload every stream of the project`
- REMOVED: `Docker's container verbs`
- REMOVED: `is loaded again rather than restarted`
- REMOVED: `stopped stream is not loaded` — and every wire text that answers "is not loaded"
  for a stream that is held and stopped.
- REMOVED: `re-loads every recorded stream neither stopped nor failed`
- REMOVED: `start_project`
- REMOVED: `stop_project`
- REMOVED: `pause_project`
- REMOVED: `resume_project`

Replaced by one `Rejected:` line on the stream-actions entry: "Rejected: Stop as the unload of
the project, the Docker container's verb — a stopped project could not be read or edited, so
pressing Stop took the editor down with the flow (owner, 2026-11-11, #3542)."

## Decided

1. **A stopped project keeps only the graph model and its described types** — no processor,
   helper instance, GPU surface, device claim or run state (owner, 2026-11-11).
2. **`open_project` / `close_project` are the lifecycle; the playback verbs are renamed and never
   load** (owner, 2026-11-11). Open is one queued action composed of separate small operations
   (dependency sync, `uv sync`, read, describe), each callable alone and reused by reload, with
   its progress shown as an action; close only reaches the closed state and erases nothing
   (owner, 2026-11-11). A failed stream held editable from its document follows from "the
   project is always there to build in" (#3542).
3. **Pause is recorded and comes back paused** (owner, 2026-11-11).
4. **Pause holds the clock** (owner, 2026-11-11).
5. **Stop forgets function-node state** (owner, 2026-11-11).
6. **The stopped preview answer** is a structured refusal with `not_running_because`, never a
   success with an empty viewer (session, 2026-11-11).

## Tickets (tracer bullets)

1. **Stop keeps the project held.** `LoadedStreamInThisRuntime` gains a stop of its playback that
   tears every instance down and keeps its graph, and a start after it; the project's Stop uses
   it with no unload and no document read; `list_streams`, `graph`, `transact` and the catalog
   answer for a stopped project; Stop forgets function-node state.
2. **The verbs split.** `open_project`, `close_project` and the four `*_playback` verbs on the
   wire, MCP and the CLI; `start_project`, `stop_project`, `pause_project` and `resume_project`
   deleted; the restart restores recorded playback and closed projects stay closed.
3. **Pause pauses everything.** The clock hold, and pause recorded.
4. **Stopped is told apart.** The structured viewer refusal, failed streams held from their
   document, the words (MCP descriptions, server prompt, skills, glossary).

Studio's side (tatolab-app #202: drop the start-to-draw, the viewer wait-out, the
close-stops dialog; re-record the stopped fixtures) is downstream backlog and never blocks these.

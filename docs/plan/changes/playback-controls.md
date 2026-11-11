# playback-controls

> **Direction from the owner, 2026-11-11 (#3542).** `start_project`, `stop_project`,
> `pause_project` and `resume_project` are **playback** controls, the transport of Unreal's
> play-in-editor or DaVinci Resolve, never the project's lifecycle. Start (Play) starts the
> flow and the clock; Stop ends it and resets the clock; Pause holds it where it stands;
> Resume carries it on. None of them changes whether the project is open, readable or
> editable. A playback verb that takes away the ability to build in the project is the defect.

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
  is entered by `create_project`, `start_project`, `open_project` (below) and the runtime's start
  for every recorded project, and left only by `remove_project` / `delete_project`. Separately,
  each held project has a **playback state**: `stopped`, `playing` or `paused`, with `preparing`
  and `stopping` while a queued action works.

  | playback | graph model, types, catalog, history | processors, helper nodes, runtime context, GPU surfaces, devices | stream clock |
  |---|---|---|---|
  | stopped | held | none | none; the next Play starts at zero |
  | playing | held | running | running |
  | paused | held | held, gated | held where it stands |

- **Stop ends the flow and keeps the project.** `stop_project` / `tatolab stop` tears down every
  processor, helper node instance, GPU surface, device claim and the clock, and leaves each
  stream loaded as a graph model at `RuntimeStatus::Initial`, recorded `stopped`. A camera, a
  microphone and a GPU are free after Stop exactly as today.
- **Play runs what was built.** `start_project` / `tatolab start` of a stopped project builds
  fresh processors from the held graph, edits made while stopped included, on a new clock.
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
  shows a calm placeholder for the first three. [assumption — see Decisions §6]
- **Playback survives a restart.** The runtime's start holds every recorded project and plays
  each one that was playing; a stopped one comes back held and stopped. [paused — Decisions §3]
- **`open_project {project_directory}`** holds a project stopped without playing it, so Studio
  opens a never-started project to build in it. [NEEDS DECISION §2]
- **`reload_project` of a stopped project** re-reads its documents into the held model and keeps
  it stopped (today refused, `stream_actions_of_this_runtime.rs:2584-2599`).
- **Edits and Play are ordered.** A Play reads the held graph after every edit committed before
  it was queued, so an edit landing while a Play prepares is either in that run or refused with
  the reason, never committed and missing from the run.
- **Glossary.** **Playback** (stopped / playing / paused) and **Hold** (a project the runtime
  holds, readable and editable) are added; **Stream**, **Project** and **Stream action** are
  reworded so no definition says a stop unloads.

## MODIFIED

- §Product, `tatolabd` at start (ARCHITECTURE.md:125-130): "re-loads every recorded stream
  neither stopped nor failed" → holds every recorded project and plays those recorded playing.
- §Product, "How a stream is loaded and kept" (:232-239): `tatolab stop` stops playback and
  keeps the project held; `tatolab rm` is the only verb that lets go of one.
- §Product, "The stream actions, as built" (:260-271): `stop_project` ends playback;
  `list_streams` states as above; `open_project` added.
- §Product, "Every loaded stream persists" (:310-316): only `rm` / `remove_project` ends a
  held stream; `stop` ends its playback.
- §Product, "Loading a project loads every stream in it" (:364-370): the project *plays* and
  stops as a whole.
- §Product, "Every load and unload is a queued stream action" (:413-479): retitled "Every load,
  playback change and removal is a queued stream action". The `stop` bullet (:426-430) as
  ADDED. The refusal table (:431-443) keeps `start` of a playing project naming
  `reload_project` and of a paused one `resume_project`, `pause` / `resume` of a stopped one
  refused; drops "`reload` of a stopped … names `start_project`". Pending marks (:464-468): a
  pending stop finishes as stopped-and-held.
- §Processor model, stream clocks (:1564-1587): the clock is built at each Play and dropped at
  Stop, so Stop resets it. [Pause holding it — Decisions §4]
- §Processor model, function-node state carried across loads
  (`docs/plan/changes/archive/*lightweight-node-kinds*`, ARCHITECTURE.md §Processor model).
  [Decisions §5]
- §Media I/O, the capture permission re-ask (:2620-2625): Stop then Play asks again, unchanged
  in effect.
- §Control plane, the tool list and "`stop_project` ends a client's work" (:3986-3993): Stop
  ends a client's playback; `remove_project` frees the project.
- The MCP descriptions of `stop_project` (`mcp.rs:643`), `list_streams` (`mcp.rs:739`), the
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

Replaced by one `Rejected:` line on the stream-actions entry: "Rejected: Stop as the unload of
the project, the Docker container's verb — a stopped project could not be read or edited, so
pressing Stop took the editor down with the flow (owner, 2026-11-11, #3542)."

## Decisions

The issue settles the model. These forks are left; each has a recommendation the tickets build
on unless the owner says otherwise.

1. **What a stopped project keeps in memory.** (a) The graph model and its described types; the
   interpreter that described the project's Python types exits once they are described, so no
   helper process, GPU surface or device is held. (b) Also keep helper processes warm for an
   instant Play. (c) Nothing; serve reads from the document. **Recommend (a):** (b) leaves the
   last run behind and holds memory on low-power devices, (c) loses live validation and the
   Python catalog.
2. **Never-started and failed projects.** Studio today starts a project just to draw it
   (tatolab-app `tatolab_commands.ts:285-303`). (a) An `open_project` verb holds a project stopped;
   a stream whose code fails to describe is still held from its document, its unknown types as
   placeholder nodes keeping their saved config and ports, shown `failed` with the reason and
   editable. (b) First contact (any read or edit) holds a project implicitly. (c) Failed streams
   stay unheld as today. **Recommend (a):** (c) locks a person out of the fix, (b) makes a read
   start an interpreter. A document that will not parse stays unheld and failed.
3. **A paused project across a restart.** (a) Record paused and bring it back paused. (b) Keep
   today's rule: paused is never recorded and comes back playing. **Recommend (a):** a live
   output that starts by itself after a restart is the worse surprise; the issue asks that the
   playback state survive.
4. **Pause holds the clock.** The issue defines Pause as holding the clock where it stands; today
   a real or scaled clock keeps running through a pause. (a) `StreamClock` gains a hold, so stream
   time stops at Pause and carries on at Resume. (b) Leave the clock running. **Recommend (a):**
   a held composition whose time jumped at Resume is not paused.
5. **Function-node state at Stop.** (a) Stop forgets it, so Play starts clean; a reload and a
   helper restart keep it. (b) Stop keeps it, as today. **Recommend (a):** the clock resets at
   Stop, and state from the last run is what "nothing left over" names.
6. **The stopped preview answer** (assumption, not a question): a structured refusal with
   `not_running_because`, never a success with an empty viewer. A viewer that opens on a stopped
   node and goes live at Play needs a channel that does not exist yet.

## Tickets (tracer bullets)

1. **Stop keeps the project held.** Stop tears processors down and leaves each stream loaded at
   `Initial`, recorded stopped; `start()` of a stopped stream runs it fresh; `list_streams`,
   `graph`, `transact`, the catalog and the viewer refusal answer for it; edits and Play are
   ordered. Proven by the issue's "Done means" script against a real runtime.
2. **Held at restart, opened without playing.** The restart holds stopped records;
   `open_project`; failed-but-parseable streams held and editable; paused recorded; `reload` of a
   stopped project.
3. **Pause holds the clock; Stop forgets function-node state.**
4. **Words.** MCP descriptions, CLI text, the server prompt, skills, the glossary.

Studio's side (tatolab-app #202: drop the start-to-draw, the viewer wait-out, the
close-stops dialog; re-record the stopped fixtures) is downstream backlog and never blocks these.

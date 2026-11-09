# project-level-run-controls

> **A project runs and stops as a whole**, decided by the owner on 2026-11-09 in the engine
> guarantee spec's open questions (item 9, its recommendation accepted): start, stop and pause
> apply to the project, every stream in it runs when it runs, and per-stream start and stop go
> away. Reactive nodes still wake only on upstream data. Closing a running project's window in
> Tato Studio prompts that the project will stop; background running is left out for now (item
> 10). That prompt is the app's, recorded in tatolab-app; this change is the runtime's half.

**After this change:**
- `stop_project {project_directory}` (`tatolab stop [--dir DIR]`) queues one stop on the
  project's queue that unloads every loaded stream of the project and records each recorded one
  `stopped`, so the runtime leaves the project stopped across its restarts.
- `run_project` (`tatolab run`) is the start: it runs a stopped project again and retries its
  failed streams, as every run already replaced each stream's record.
- `start_stream` and `stop_stream`, and the `tatolab start` verb, are gone. A stream is still
  its own record — id, display name, failure, function-node state — so `rename_stream`,
  `set_stream_clock_scale`, `graph` and `logs` still name one stream.

The word "stream" stays everywhere else: the spec thread's rename of a project's stream to a
graph is one later sweep, and this change names only its new verb for the project.

## Recon (2026-11-09, `main` `a4574c2`)

- `Runner::stop_stream` and `Runner::start_stream` queued a `Stop` or `Start` action on one
  stream through `queue_a_stream_action_on_one_stream`
  (`runtime/streamlib-engine/src/core/runtime/stream_actions_of_this_runtime.rs`); the start
  re-read the record's graph document and loaded it alone.
- `run_project` writes a fresh `StreamRecord::of_a_running_stream` for each stream asset, keeping
  only the display name, pending marks and function-node states, so a run already clears
  `stopped`, `failed_because` and the crash count.
- The local API served `stop_stream` and `start_stream` (`streamlib-api-server/src/mcp.rs`), the
  wire contract carried `StopStreamToolResult` and `StartStreamToolResult`, and the CLI had
  `tatolab stop [STREAM]` and `tatolab start [STREAM]`.
- Tato Studio's Stop already stopped every stream of the open project one `stop_stream` at a
  time; nothing in Studio called `start_stream` outside its mock and tests.

## ADDED

- **ADDED (§Product, DECIDED):** A project runs and stops as a whole. `stop_project
  {project_directory}` and `tatolab stop [--dir DIR]` queue one stop of every stream of the
  project, each record running once the actions queued before it run carrying a pending stop;
  a stream already stopped or failed is left as it is, and a stream that fails before the stop
  reaches it has its mark taken off. Refused with nothing queued: a relative directory, a
  project whose removal is queued, and a project none of whose streams runs. Owner, 2026-11-09.

## MODIFIED

- **MODIFIED (§Product, "How a stream is loaded and kept", "The stream actions, as built",
  "Every loaded stream persists", "Loading a project loads every stream in it", "A load is never
  refused", "Every load and unload is a queued stream action"):** `stop_project` replaces
  `stop_stream`; `run_project` replaces `start_stream` as the start and as the retry of a failed
  stream; the pending marks are a stop and a remove only.
- **MODIFIED (§Control plane & observability):** the served stream actions are ten, with
  `stop_project` in place of `stop_stream` and `start_stream`; the CLI's stream verbs lose
  `start`.
- **MODIFIED (camera and microphone permission):** a stream re-loaded at the runtime's start
  asks for its device through a `stop_project` then `run_project` of its project.

## REMOVED

- REMOVED: StartStreamToolResult
- REMOVED: StopStreamToolResult
- REMOVED: start_the_stream_finishing_its_pending_start
- REMOVED: queue_a_stream_action_on_one_stream
- REMOVED: is_failed_once_its_pending_stream_actions_run

## Not in this change

- **Pause.** The owner named pause beside start and stop; the runtime has no pause of a stream
  or a project today, and building one is its own change.
- **One project run in place of per-stream records.** Each stream keeps its record, because its
  id, display name, failure and function-node state are its own; only running and stopped are
  set project-wide, by the only two verbs left that set them.

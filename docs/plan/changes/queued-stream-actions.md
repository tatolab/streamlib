# queued-stream-actions

> **Approved by the owner, 2026-10-30:** "I approve the pr merge it then /derive-tickets" (PR #2985).
> Ticketed as #2986, #2987, #2988, #2989, #2990, #2991.
>
> **Amended by the owner, 2026-10-31:** "Yes rm removes the entire project from the runtime, we
> don't delete streams from the runtime since that would make no sense to have code in the
> project marked as a @stream but not run it". `remove_stream` becomes `remove_project`.

Builds §Product's "Every load and unload is a queued stream action" and the project-log clauses
of "A load is never refused", with every `amended by queued-stream-actions` pointer in §Product
and §Control plane (ADR `docs/decisions/queued-stream-actions.md`, aligned in #2984).

Today every stream tool is a synchronous engine call. A load makes the venv (a sync bounded at
240s), compiles one function, loads and starts inside the reply. Clients wait up to 480s, and the
re-load at start syncs under the engine-wide lock. Each loaded stream writes its own log.

**After this change:**
- **Actions.** `run_project`, `start_stream`, `stop_stream`, `remove_project` and the re-load at
  start queue a stream action on the project's serial queue and answer its action id at once.
  `describe_stream_action` reads one, and `cancel_stream_action` ends one early.
- **Loads take a project.** `run_project {project_directory}` loads every `@stream`. `run` and
  `dev` name no function and no display name.
- **Listing.** `list_streams` shows `preparing` and `stopping`, with the action's step.
- **Records.** A queued stop or start writes a pending mark on its stream's record, and a queued
  remove one on every record of its project; the action finalises it.
- **`rm` removes a project.** `remove_project {project_directory}` unloads every stream of the
  project and forgets every record of it; no tool removes one stream.
- **Attributes.** `rename_stream` and `expose_port` never wait behind an action.
- **Logs.** One log per project, every record carrying `action`, and `stream` when a stream
  wrote it. `logs` takes the project.
- **Timeouts.** The two long client timeouts and their const-asserts are gone; the venv sync
  has no bound.

**Scale gate — this skill; the ADR exists.**
- **Contracts changed:** the local API (stream tool arguments and results, two new tools,
  `logs`, the listing states); the CLI's `run`, `dev`, `start`, `stop`, `rm`, `logs` and
  `streams`, plus a new `cancel`; the stream record (a pending mark, schema 3 → 4); and the
  JSONL routing and fields (`docs/logging-schema.md`).
- **Not touched:** the RHI, the iceoryx2 wire, the processor model, `@stream` and the
  authoring API, the stream id, and the compile document.

**Precondition.** §Product and §Control plane & observability are DECIDED, and every entry
this builds carries `[queued-stream-actions]` or `amended by queued-stream-actions`.

## Current state (recon, 2026-10-30, `main` `9b6d6b72a`)

Paths are under `runtime/`. S = `streamlib-engine/src/core/runtime/stream_actions_of_this_runtime.rs`.

- **Synchronous actions.** Every action is a `Runner` method (S:405). The MCP server runs each
  on `spawn_blocking` and awaits it without a server timeout (`streamlib-api-server/src/mcp.rs:175-189`).
  - `run_stream` (S:453):
    - makes the venv outside the lock (S:481);
    - compiles one function (S:504-512) through `compile_the_stream_function_in_the_projects_interpreter`
      (`compiler_ops/stream_function_compile_in_the_projects_interpreter.rs:182`);
    - takes the lock (S:514), then replaces or newly loads (S:1257-1330).
  - `stop_stream` (S:581), `start_stream` (S:662, venv at S:663), `remove_stream` (S:733),
    `rename_stream` (S:760) and `expose_port` (S:855) each take the lock.
  - `list_streams` (S:794) takes no lock.
- **The lock.** `StreamActionsOfTheEngine.one_stream_action_at_a_time: Mutex<()>`, one per
  runtime (S:292-299). There is no queue, job registry or action worker; the nearest machinery
  is `Runner::tokio_handle()` (`runtime.rs:811`).
- **The re-load at start.** `reload_each_recorded_stream_listed` (S:957) takes the lock per
  record (S:972) and makes the venv inside it (S:1033). tatolabd calls it synchronously
  (`tatolabd/src/machine_stream_hosting.rs:147-149`).
- **Bounds.** S:331-403 holds the hold constants and four const-asserts against
  `STREAM_LOAD_TOOL_CALL_TIMEOUT` (480s) and `STREAM_ACTION_WITHOUT_A_LOAD_TOOL_CALL_TIMEOUT`
  (360s) (`streamlib-runtime-client-contract/src/local_api_wire_contract.rs:22-35`).
  - The CLI picks them per verb (`tatolab-cli/src/stream_actions_on_the_runtime.rs:63-74`).
  - `dev` uses the load timeout (`stream_followed_and_reloaded_on_every_save.rs:315-321`).
  - A Python mirror holds 480.0 (`tests/stream-on-runtime/test_project_venv_made_by_the_runtime_unit_uv.py:26`).
  - Ordinary verbs wait 30s (`tatolab-cli/src/local_api_mcp_tool_client.rs:32`).
- **The venv.** `project_venv_made_with_the_runtime_unit_uv.rs` has the probe bound of 10s
  (:29), `PROJECT_VENV_SYNC_BOUND` 240s (:33) and `PROJECT_VENV_MAKING_BOUND` (:36).
  - It syncs through `run_a_child_process_to_its_exit_within` (`compiler_ops/processor_interpreter_spawn_host.rs:1037-1070`),
    interrupted by `is_the_machines_shutdown_requested` (:87).
  - It removes a half-made `.venv` (:178-183). No per-project lock exists.
- **Shapes.**
  - `RunStreamRequest {project_directory, stream_function, display_name}` (S:70-82) and
    `StreamRunOutcome` (S:86-95).
  - `StreamListingState::{Running, Stopped, Failed}` (S:189-199).
  - `StreamRecord`, schema 3, `deny_unknown_fields`, has no pending notion
    (`machine_state_directory.rs:23-59`).
  - The engine trait lists every action (`operations_on_the_streams_loaded_in_this_runtime.rs:38-113`).
    It is implemented by `Runner` (:116), the stub (`streamlib-api-server/src/control_plane_stub_support.rs:240`)
    and a test engine (`stream_tools_over_a_real_engine_tests.rs:343`).
- **Wire.**
  - `RunStreamToolArguments {project_directory, stream_function, name}` (`mcp.rs:318-331`) and
    `RunStreamToolResult{,Stream}` (contract :87-121).
  - `ListedStreamState` (:199-218).
  - `LogsToolArguments {stream, after, count}` (`mcp.rs:244`) and `LogsToolResult {stream,
    stream_instance, records, next_after, records_no_longer_held}` (contract :50-73).
  - `LoadedStreamInstance` (:37-41).
  - `CONTROL_TOOL_NAMES` is pinned (`mcp.rs:1188-1207`, test :1605).
- **Logs.**
  - One route per loaded stream: `LoadedStreamLogRoute::open_in_project_directory`
    (`streamlib-engine/src/core/logging/loaded_stream_log_route.rs:126-135`), opened in
    `LoadedStreamInThisRuntime::new` (`loaded_stream.rs:222-227`).
  - Files are `<project>/.streamlib/logs/<runtime_id>-<stream>-<started_at_millis>.jsonl`
    (`streamlib-runtime-client-contract/src/runtime_log_file_paths.rs:25-50`).
  - A record takes `stream` from its thread's route (`logging/worker.rs:354-366`). Helper
    threads carry the route (`compiler_ops/subprocess_bridge.rs:612-628,857-876`).
  - `RuntimeLogEvent` has no `action` (`runtime_log_event.rs:93-168`).
  - The venv's and compile's stderr run on no route, so they land in the runtime's own log
    (`worker.rs:356`). The compile's lines also come back as `compile_warnings`.
  - `logs` reads a loaded stream's in-memory history only, 4096 records, numbered per load
    (`logging/loaded_stream_log_record_history.rs:13,78-104`; `operations_on_the_streams_loaded_in_this_runtime.rs:162-173`).
- **CLI.**
  - `run`'s `TARGET` takes `<file>.py[:<fn>]` or `<module>:<fn>`, with `-f`, `--dir` and
    `--name` (`stream_actions_on_the_runtime.rs:99-115`). It sends them as `stream_function` and `name` (:171).
  - `run` exits 1 on a failed stream (`RUN_EXIT_CODE_WHEN_A_STREAM_IT_LOADED_IS_FAILED`, :284).
  - `streams` prints `ID NAME STATE …` (:450-504).
  - `logs` has an on-disk mode keyed `RUNTIME_ID-STREAM` under `<STREAMLIB_HOME>/.streamlib/logs`
    (`runtime_logs_verb.rs:62-76,371-470`). Otherwise it pages the `logs` tool (:240-365).
  - `dev` follows the first stream's `logs` (`stream_followed_and_reloaded_on_every_save.rs:346,427-496`).
  - Help is pinned (`tests/tatolab_new_and_help.rs:66`).
- **Integration suite.** The suite directory is one project of many `*_streams.py` modules.
  - The harness loads one function per test as `tatolab run <module>:<qualname> [--name]`
    (`tests/stream-on-runtime/stream_runs_on_tatolabd.py:77-118`).
  - It parses `run`'s running line and exit 0 (`conftest.py:146-155,375-406`).
- **Not on main:** `--every-stream` and `list_project_streams` (#2973), `create_project`
  (#2974), and every clause of `failed-load-logs.md` (#2979, #2980, #2981).

## ADDED

- **The project's stream action queue.** `StreamActionsOfTheEngine` gains
  `stream_action_queue_of_each_project: Mutex<HashMap<PathBuf, ProjectStreamActionQueue>>`,
  keyed by the resolved project directory.
  - Each queue runs its actions one at a time, in order, on one worker the engine's tokio
    runtime spawns on first use; the worker runs each action's existing synchronous body
    through `spawn_blocking`.
  - Queues never wait on each other. The one-action lock stays engine-wide and is held only
    for an action's load and unload steps, never for the venv or the compile.
  - The existing bodies (`run_project`, `start_stream`, `stop_stream`, `remove_project` and the
    re-load of one project) become the actions' steps and gain a `StreamActionProgress` they
    report their step to.
- **A stream action.** `StreamAction {action_id, project_directory, kind, state, step,
  streams, failed_because, queued_at}`:
  - `kind` is `Load`, `Start`, `Stop`, `Remove` or `ReloadAtStart`.
  - `state` is `Queued`, `InProgress`, `Done`, `Failed` or `Cancelled`.
  - `step` is `MakingTheVenv`, `Compiling`, `Starting` or `Stopping`.
  - `action_id` is `<runtime_id>-<n>`, so no id repeats across restarts.
  - The engine keeps every queued and running action, and the last 64 finished ones per
    project; an older id is answered as no longer held, and its records stay in the log.
- **Cancel.** `cancel_stream_action(action_id)` drops a queued action. A running load or start
  stops at its current step: the venv sync, the compile and `load_stream_held_as_unless_a_machine_shutdown_is_requested`
  (`runtime.rs:892-965`) already poll a shutdown interrupt, which becomes "shutdown requested
  or this action cancelled", so a running sync is killed and its half-made `.venv` removed.
  Streams it had not reached stay as recorded. A stop or remove whose teardown has started is
  past cancelling: cancel answers so, and the action ends `done`, its mark finalised.
- **Pending marks.** `StreamRecord.pending_stream_actions: Vec<PendingStreamActionOnTheRecord>`,
  each `{action_id, kind}` with kind `Stop`, `Remove` or `Start`, in queue order — a `Remove`
  mark on every record of its project;
  `STREAM_RECORD_SCHEMA_VERSION` goes to 4, and a schema-3 file reads as having none.
  - Queuing appends the action's mark. Only that action finalises it (records `stopped`,
    forgets the record, or clears `stopped` and `failed`), and only its cancel removes it.
  - The re-load at start finishes every record's marks in order before it queues loads.
- **The record update.** `StreamRecordsInTheStateDirectory::update(stream_id, edit)` reads,
  edits and writes a record under a lock held only for that, so concurrent writers never lose
  each other's fields (`write` replaces the whole record from a caller's snapshot today,
  `machine_state_directory.rs:456`). Every record change goes through it.
- **Engine trait.** `OperationsOnTheStreamsLoadedInThisRuntime` gains `describe_stream_action`
  and `cancel_stream_action`; `run_project`, `start_stream`, `stop_stream` and `remove_project`
  return `StreamActionQueued {action_id, project_directory}`; the stub and test engine follow.
- **Last compile per project.** Every compile records its project's streams (each function and
  its docstring), or why `stream.py` did not import, in memory. `list_project_streams` (#2973)
  answers from that record:
  - with a load queued or under way and no compile yet, `preparing` and no streams;
  - with neither, that the project is not loaded.
- **Tools.**
  - `describe_stream_action {action}` returns `DescribeStreamActionToolResult {action, project_directory,
    kind, state, step, streams: [{stream, display_name, state}], failed_because}`.
  - `cancel_stream_action {action}` returns `CancelStreamActionToolResult {action, state}`.
  - Both go in `local_api_wire_contract.rs`, and `CONTROL_TOOL_NAMES` gains both.
- **The project log.** `ProjectLogRoute` replaces `LoadedStreamLogRoute`.
  - Its owner is `Project(project_directory)` or `TheRuntimeItself`, and every loaded stream
    and every action of the project shares one route.
  - The thread-local entry carries the route plus the emitting stream id and action id, and
    `write_one` stamps `stream` and `action` from it.
  - An action enters the route before its first step, so uv's and the compile's stderr lines,
    labelled as today, land in the project log with `action` and no `stream`.
  - Files are `<project>/.streamlib/logs/project.jsonl`, rotated as `project.<seq>.jsonl`, and
    appended across the runtime's runs. Only one runtime writes, since there is one per machine.
  - Rotation and retention apply per project with today's tunables (`logging/writer.rs`,
    `logging/config.rs:23-33`).
- **`RuntimeLogEvent.action: Option<String>`**, present on every project-log record — a
  running stream's records carry the id of the action that loaded it — and absent only in the
  runtime's own log. `schema_version` goes 1 → 2, and readers take both.
- **Reading the project log from disk.** `project_log_records_after(project_directory, stream,
  action, after, count)` in the contract crate reads the rotated segments by ascending `seq`,
  then the active one, through `runtime_log_files_reader.rs`'s rotation-aware reader moved
  beside it.
  - The cursor is opaque, `<seq>:<line>`, and survives restarts.
  - A cursor whose segment retention deleted resumes at the oldest held, with
    `records_no_longer_held: true`.
  - Per-stream segments written before this change are neither migrated nor read, and stay on
    disk; there is no back-compat shim pre-1.0.
- **CLI.**
  - `tatolab cancel <action>` calls `cancel_stream_action` and prints the action's new state.
  - `streams` shows `preparing` and `stopping` in STATE, with a `<id> <step>` line under the
    table, as it shows a failed stream's reason.

## MODIFIED

- **`Runner::run_stream` becomes `Runner::run_project`**, taking `RunProjectRequest
  {project_directory}` and queuing a `Load`. The project's entry is always `stream.py`.
  - The load compiles every `@stream` through #2973's `--every-stream`, and loads or fails each
    as today's bodies do.
  - A missing directory or a folder that is not a project fails the action, with its reason in
    the runtime's own log. An id another location holds fails it in the project log.
  - `start_stream` of a record with no graph still compiles that record's own location
    (S:1626-1670); that internal single-function compile stays and is no load argument.
- **The re-load at start** queues one `ReloadAtStart` per project with a record neither stopped
  nor failed, after finishing every pending mark. It loads only those records.
  - tatolabd's call (`machine_stream_hosting.rs:147-149`) queues and returns.
  - The venv is made outside the lock.
- **`rename_stream` and `expose_port`** stop taking `one_stream_action_at_a_time`
  (S:766, S:862) and write through `update`. `expose_port`'s live cut runs as today when the
  stream is loaded; otherwise the ruling applies at load.
- **`remove_stream` becomes `remove_project {project_directory}`** (`tatolab rm [--dir]`, the
  working directory's project unless `--dir` names one). Its `Remove` action unloads every
  loaded stream of the project and forgets every record of it, and deletes no file. A stream
  of the project that is not recorded is not an error; a project with nothing loaded or
  recorded ends the action `done` with no streams.
- **Listing.**
  - `StreamListingState` and `ListedStreamState` gain `Preparing` and `Stopping` (wire
    `preparing` and `stopping`).
  - `StreamListing` and `ListStreamsToolResultStream` gain `action` and `step`, set while an
    action works on the stream.
- **Wire.**
  - The tool `run_stream` is renamed `run_project`: `RunProjectToolArguments
    {project_directory}`, `RunProjectToolResult {action, project_directory}`.
  - `StartStreamToolResult` and `StopStreamToolResult` become `{action, stream}`;
    `RemoveProjectToolResult` is `{action, project_directory}`.
  - `LogsToolArguments` is `{project_directory, stream?, action?, after?, count?}`, `after` the
    opaque cursor. `LogsToolResult` is `{project_directory, records, next_after,
    records_no_longer_held}`.
- **The tool descriptions and `LOCAL_API_MCP_SERVER_INSTRUCTIONS`** (`mcp.rs:92,320-331,473,614,676`):
  - every load and unload is queued and answers an action id, which `describe_stream_action`
    follows;
  - a load takes the project; `logs` takes the project.
- **The CLI.**
  - `run` takes `--dir` only (`TARGET`, `-f`, `--name` go) and prints `<action> queued: load of <project>` and `follow it:
    tatolab logs -f`. It exits 0 once queued, and 1 only for a malformed request or an
    unreachable runtime.
  - `start` and `stop` print `<action> queued: <verb> of <stream>`, and `rm` prints
    `<action> queued: remove of <project>`.
  - Every stream verb uses the 30s ordinary timeout.
  - `logs [--dir] [--stream <stream>] [--action <action>] [-f]` takes the working directory's
    project unless `--dir` names one.
  - `dev` queues a `Load` per save and follows the project log, each record under the
    project's folder name with its stream in the record's bracket. After a runtime crash it
    waits for the runtime and follows again.
- **`docs/logging-schema.md`**:
  - §Routing: one log per project, its records stamped with `stream` and `action`.
  - §Files and rotation: `project.jsonl`, rotation and retention per project.
  - §Fields: `action` and `schema_version` 2.
- **The integration harness.** `stream_runs_on_tatolabd.py` gives each suite stream function a
  one-stream project in a temporary directory: a `stream.py` whose one `@stream` delegates to
  the suite function, and `.venv` linked to the suite's venv.
  - It loads with `tatolab run --dir`, sets a display name with `tatolab rename`, and waits on
    `describe_stream_action` until the action is done or failed.
  - `conftest.py`'s `await_loaded` and `failed_stream` read the action and `list_streams`
    instead of `run`'s output and exit code.
- **`README.md`** and the CLI's `long_about` texts (`tatolab-cli/src/main.rs:118-127`) describe
  queued actions and `run`'s exit 0.

## REMOVED

- REMOVED: STREAM_LOAD_TOOL_CALL_TIMEOUT
  The client contract's 480s wait, its doc comment, the CLI's use and the Python mirror's name
  (`STREAM_LOAD_TOOL_CALL_TIMEOUT_SECONDS`).
- REMOVED: STREAM_ACTION_WITHOUT_A_LOAD_TOOL_CALL_TIMEOUT
- REMOVED: PROJECT_VENV_SYNC_BOUND
  The sync has no bound; uv's own network timeouts end a stalled download.
- REMOVED: PROJECT_VENV_MAKING_BOUND
- REMOVED: LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_AHEAD_IN_THE_QUEUE_IN_MILLISECONDS
  With the hold constants below and the four const-asserts at S:380-403.
- REMOVED: LONGEST_HOLD_OF_THE_ONE_STREAM_ACTION_LOCK_BY_THE_RELOAD_OF_ONE_RECORD_IN_MILLISECONDS
- REMOVED: LONGEST_LOAD_BEHIND_ONE_QUEUED_STREAM_ACTION_IN_MILLISECONDS
- REMOVED: LONGEST_START_BEHIND_ONE_QUEUED_STREAM_ACTION_IN_MILLISECONDS
- REMOVED: RUN_EXIT_CODE_WHEN_A_STREAM_IT_LOADED_IS_FAILED
- REMOVED: LoadedStreamLogRoute
  Renamed `ProjectLogRoute`; the file goes with it.
- REMOVED: runtime/streamlib-engine/src/core/logging/loaded_stream_log_route.rs
- REMOVED: runtime/streamlib-engine/src/core/logging/loaded_stream_log_record_history.rs
  `logs` reads the project log from disk; no in-memory history.
- REMOVED: loaded_stream_log_instance_name
  With `loaded_stream_log_path` below: the per-stream segment naming.
- REMOVED: loaded_stream_log_path
- REMOVED: LoadedStreamInstance
  The wire's per-load instance; the action id names a load.
- REMOVED: stream_instance
- REMOVED: compile_warnings
  The compile op's report field and the wire's: the compile's lines are in the project log.
- REMOVED: RunStreamToolResult
  With `RunStreamToolResultStream`, `RunStreamToolArguments` and `RunStreamRequest`: the tool
  is `run_project`.
- REMOVED: RunStreamToolArguments
- REMOVED: RunStreamRequest
- REMOVED: requested_entry_file
  The CLI's `-f`; a project's entry is always `stream.py` (owner, 2026-10-30).
- REMOVED: requested_stream_target
- REMOVED: Omit it for the sole
  The `stream_function` argument's description.
- REMOVED: remove_stream
  With `RemoveStreamToolResult`: `rm` removes the project (owner, 2026-10-31).
- REMOVED: RemoveStreamToolResult
- REMOVED: tatolab_run_of_a_suite_stream_function
  The harness's `run <module>:<qualname>`; replaced by the one-stream project.

## Seams

- **#2973 (every `@stream`, `list_project_streams`).** It lands first. Its load naming one
  function, its `--name` refusal and its `list_project_streams` making the venv are removed
  here, so trimming them from #2973 before it is built saves building them twice.
- **#2974 and tatolab-app#100** wait for this change. `create_project` is a queued `Load` of the
  scaffold, answering `{action, stream, project_directory}`, with no create timeout.
- **`failed-load-logs.md`.** Its per-load segment, `stream_instance` and `rm` clauses (#2979,
  #2980) and #2916 are overtaken by the project log, and are retired when this change is
  approved. Its no-`@stream` clause (#2981) stands.

## Test plan

- **Engine:**
  - two projects' queues run side by side; one project's actions run in order;
  - a cancel drops a queued action and kills a running stub `uv` (a `sleep`), removing the
    half-made `.venv`;
  - stop then start of one stream queue two marks, each finalised by its own action in order;
    a cancelled one leaves the record as it was; a fresh `Runner` finishes the marks a crash
    left, in order; cancelling a stop whose teardown started answers so and ends `done`;
  - a rename during a load keeps both the new display name and the load's record change;
  - the re-load at start of a project with a running and a stopped record loads only the
    running one;
  - `rename_stream` and `expose_port` answer while a project's sync runs;
  - the 65th finished action of a project evicts the oldest;
  - a schema-3 record reads.
- **Logging:**
  - two streams and an action of one project write one `project.jsonl`, each record carrying
    its `stream` and `action`;
  - a stub sync's stderr lines carry `action` and no `stream`;
  - `project_log_records_after` filters by stream and by action, resumes across a rotation and
    a restart, and flags records no longer held.
- **api-server:**
  - `CONTROL_TOOL_NAMES` pins the two tools;
  - `run_project` answers `{action, project_directory}` at once while a stub sync sleeps;
  - `describe_stream_action` walks `queued`, `in_progress` (`making_the_venv`) to `done`;
  - `list_streams` shows `preparing`;
  - `logs` by project, stream and action.
- **CLI:**
  - `run` prints the queued line and exits 0, and exits 1 only for an unreachable runtime;
  - `cancel` prints the state;
  - `streams` prints `preparing` with its step;
  - `logs --action` filters;
  - `dev` follows the project log across a save;
  - the help pin gains `cancel`.
- **Integration** (`tests/stream-on-runtime/`, GPU-free):
  - the suite passes on the one-stream-project harness;
  - `tatolab run` of a project whose `.venv` is deleted returns at once; `tatolab streams` shows
    `preparing`, then `running`, and `tatolab logs --action` shows uv's lines;
  - `tatolab cancel` during that sync leaves no `.venv`.

## Out of scope

- An idempotency key, persisted actions and OpenTelemetry field names (rejected in the ADR).
- Pruning `project.*.jsonl` beyond today's retention; `create_project` (#2974); tatolab-app#100.

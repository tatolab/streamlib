# failed-load-logs

Builds the `[failed-load-logs]` clauses of §Product's "A load is never refused for what a
project's code or environment does" entry (ADR `docs/decisions/failed-load-logs.md`, aligned in
#2977). It sits beside `create-project.md`, which stays the change for the rest of that entry;
the seams with its open tickets are named under "Seams" below.

Today a failed stream is listed with `failed_because` and has no log a client can read:
`logs` serves a loaded stream's in-memory records only, a compile that fails never opens a
segment, `rm` leaves every segment on disk, and a `stream.py` that defines no `@stream` is
refused.

**After this change:**
- **A failed load has a log.** Its records — what the load logged, then one error record with
  its traceback or reason — are written to its own segment, named as any load's is.
- **`logs` reads a failed stream from disk** until the stream is loaded again, across the
  runtime's restarts. `logs -f` and `dev` print its records and wait.
- **`rm` deletes the stream's segments** with its record.
- **A `stream.py` with no `@stream` is a failed project**, not a refusal.

**Scale gate — this skill; the ADR exists.**
- **Contracts changed:** the `logs` tool (a stream that is not loaded), `run_stream`'s result
  (`stream_instance` on a failed stream), the spelling of `stream_instance`, `remove_stream`'s
  effect on disk, the compile document (a fifth answer), and `docs/logging-schema.md`'s routing.
- **Not touched:** the RHI, the iceoryx2 wire, the processor model, `@stream` and the rest of
  the authoring API, the stream id, the stream record (no field added; schema stays 3), and the
  JSONL record shape (`schema_version` stays 1).

**Precondition.** §Product and §Control plane & observability are DECIDED; the §Product entry
carries `[failed-load-logs]`.

## Current state (recon, 2026-10-29, `main` `39031084d`)

Paths are under `runtime/` unless marked otherwise.

- **A load's segment.** `LoadedStreamInThisRuntime::new` takes a `LoadedStreamTag` and opens
  `LoadedStreamLogRoute::open_in_project_directory(runtime_id, stream_id, project_directory)`
  (`streamlib-engine/src/core/runtime/loaded_stream.rs:221-226`). The route writes
  `<project>/.streamlib/logs/<runtime_id>-<stream>-<started_at_millis>.jsonl`
  (`streamlib-runtime-client-contract/src/runtime_log_file_paths.rs:21-46`), rotates and retains
  segments (`docs/logging-schema.md` §Files and rotation), and holds the last 4096 records in
  memory (`streamlib-engine/src/core/logging/loaded_stream_log_record_history.rs`).
- **The load's tag.** `LoadedStreamTag::next_in_this_process` is a process-wide counter from 1
  (`loaded_stream.rs:50-70`); the wire's `stream_instance` is its decimal
  (`streamlib-api-server/src/mcp.rs:910`). A restarted runtime counts from 1 again.
- **`logs`.** `log_records_of_the_stream_a_call_names` answers a loaded stream alone
  (`streamlib-engine/src/core/runtime/operations_on_the_streams_loaded_in_this_runtime.rs`,
  `:162-173`); the tool text says "one loaded stream's log records" (`mcp.rs:473`).
  `LogsToolResult{stream, stream_instance, records, next_after, records_no_longer_held}`
  (`streamlib-runtime-client-contract/src/local_api_wire_contract.rs:50-64`).
- **A failed stream's reason.** `Runner::run_stream` records a compile that gave no graph, a
  refused load or start, a function no longer defined, and an entry that did not import
  (`stream_actions_of_this_runtime.rs`, `record_and_load_one_stream_a_run_compiled`,
  `fail_every_record_of_a_project_whose_entry_did_not_import`). Only a refused load or start
  ever had a route; its records reached its segment, and the reason did not.
- **`run_stream`'s failed stream** answers `stream_instance: null`
  (`local_api_wire_contract.rs`, `RunStreamToolResultStream`).
- **`rm`.** `Runner::remove_stream` forgets the record and unloads
  (`stream_actions_of_this_runtime.rs:668`); no segment is deleted.
- **The CLI.** `tatolab logs` reads a loaded stream through the tool, and an on-disk log by
  `RUNTIME_ID-STREAM` through `RuntimeLogInstanceSegmentsReader`
  (`tatolab-cli/src/runtime_log_files_reader.rs:247`), rotation-aware. `dev` prints a failed
  stream's `failed_because` as a note and waits for a save
  (`tatolab-cli/src/stream_followed_and_reloaded_on_every_save.rs`,
  `run_the_stream_over_the_connection`).
- **No `@stream`.** `select_stream_function` raises `_no_stream_defined_refusal`
  (`sdk/tatolab-stream/tatolab/stream/_project_stream_entry_execution.py:150,182`), so the
  compile entry exits 1 and `run_stream` is refused.

## ADDED

- **One load identity across restarts.** `stream_instance` is spelled from the load's segment,
  `<runtime_id>-<started_at_millis>`, for a running load and a failed one alike, so no later
  load — after a restart included — is given a failed load's instance. It stays opaque on the
  wire, compared for equality only. `LoadedStreamTag` stays the engine's in-process key.
- **A failed load's segment.**
  `LoadedStreamLogRoute::open_for_a_failed_load(runtime_id, stream_id, project_directory)` opens
  the segment a load would, for a stream that never reached `LoadedStreamInThisRuntime`. Through
  it the run writes the compile's standard-error lines, as `stream-function-compile` records,
  then one `ERROR` record whose message is the reason, then closes it, flushed and `fdatasync`ed.
  - A compile that gave no graph, a function no longer defined, an entry that did not import,
    and one that defines no `@stream` each write a failed load's segment for every stream they
    record failed — the project's `stream.py` record (`<folder>-<hash>`) included.
  - A refused load or start writes the same `ERROR` record through the load's own route before
    the route closes, so its segment ends on the reason.
  - The record's `stream` is the stream id and `runtime_id` the runtime's, as every routed record.
- **Reading a stream's log from disk.** `stream_log_segments_on_disk(project_directory, stream_id)`
  in `streamlib-runtime-client-contract`. It lists `<project>/.streamlib/logs/` for names
  `<runtime_id>-<stream>-<digits>.jsonl` and their rotated `.<seq>.jsonl`, and keeps a name
  whose first record's `stream` is the stream id, since a pinned runtime id may hold dashes.
  It groups them by `<runtime_id>-<started_at_millis>`, newest last.
  - `logs` on a stream that is not loaded and is recorded failed reads its newest group:
    rotated segments by `seq`, then the active one, numbered from 1 in that order.
    `stream_instance` is the group's `<runtime_id>-<started_at_millis>`, and
    `records_no_longer_held` is 0.
  - A stream that is neither loaded nor failed is refused as today.
- **Deleting a stream's log.** `remove_stream` deletes every segment the listing above finds for
  the stream, every group and any `.rotating` file of one, after the stream is unloaded and its
  record forgotten. A segment that cannot be deleted is logged and leaves the remove done; a
  stream its host process loaded holds no record and none of its segments are deleted.
- **A compile answer for no `@stream`.** With no target, an entry that imports and defines no
  `@stream` writes `{project_directory, no_stream_defined_because}` and exits 0 — the reason the
  refusal gives today, its sample included. A target naming a function stays a
  `not_defined_because`. The runtime answers it as an entry that did not import, the reason the
  missing `@stream`: every record of the project failed, or the project's `stream.py` record.

## MODIFIED

- **`run_stream`'s result.** `RunStreamToolResultStream.stream_instance` is present for a failed
  stream: the failed load's instance. The field is no longer optional.
- **The `logs` tool** reads a failed stream from disk. Its description names a failed stream
  read from its segment until it is loaded again; the server instructions say so.
- **`tatolab logs --stream`** reads a failed stream as a loaded one. `-f` prints its records and
  polls; a page naming another instance follows the new load from its first record, as today.
- **`tatolab dev`** follows a failed stream's records after a run, as it follows a running one,
  so the traceback shows once, from the log. Its note is `<id> failed — fix it and save again`
  in place of the reason, and a save runs the stream again.
- **`tatolab run`** prints `follow it: tatolab logs -f` under a failed stream as under a running
  one; its line stays `<id> failed: <reason>`.
- **`docs/logging-schema.md` §Routing and §Files and rotation:** a failed load writes a segment
  of its own, opened at its failure when it never loaded; `logs` reads a failed stream's newest
  group; `rm` deletes a removed stream's segments.
- **The compile entry** (`_project_stream_compile_entry.py`) documents the fifth answer in its
  module docstring; `_project_stream_entry_execution.py` raises a `StreamEntryDefinesNoStreamError`
  that the entry turns into the document.
- **Plan text** when this ships: §Product's entry is already written; §Control plane's
  observability entry names a failed stream's log served from disk.

## REMOVED

- REMOVED: stream_instance: Option<LoadedStreamInstance>
  `RunStreamToolResultStream`'s optional instance.
- REMOVED: "stream_instance":null
  The wire-order test's failed stream with no instance; its doc line goes with the field.
- REMOVED: Read one loaded stream's log records
  The `logs` tool text that limits it to a loaded stream.
- REMOVED: _no_stream_defined_refusal
  The refusal of an entry that defines no `@stream`.

## Seams with create-project's open tickets

- **#2973 (several streams, `--every-stream`, `dev` following several).** An `--every-stream`
  document with an empty `streams` list is this change's no-`@stream` answer. Its `dev` rework
  follows each stream's records, failed ones through this change's `logs`; whichever lands
  second takes the other's shape.
- **#2972 (the venv).** A failed sync is a failed load: it writes the segment with uv's tail
  through `open_for_a_failed_load`.
- **Rotation of a never-removed stream's old groups** is not decided (ADR §Consequences);
  retention per group applies as today.

## Test plan

- **Engine** (`ProjectWithAStubCompile`):
  - a function that raises writes one segment whose last record is the `ERROR` reason;
    `log_records_of_the_stream_a_call_names` reads it, numbered from 1;
  - a fresh `Runner` (another runtime id) on the same state directory still reads it, under
    the same `stream_instance`; a load after it answers another instance;
  - an entry that does not import writes the project's `stream.py` record's segment, and one
    for each recorded stream;
  - a refused start's segment ends on its reason;
  - `remove_stream` deletes every group of the stream, rotated segments included, and leaves a
    segment of a stream whose id ends the same and whose records name another stream;
  - a stopped stream that is not failed is still refused by `logs`.
- **Engine, `stream_instance`:** two loads of one stream in one runtime, and one in each of two
  runtimes, answer three instances.
- **Contract:** `stream_log_segments_on_disk` groups active and rotated segments, rejects a
  name whose records name another stream, and orders groups by `started_at_millis`.
- **Compile op:** `no_stream_defined_because` parses, and is answered as an entry that did not
  import.
- **Stream suite** (`test_project_stream_compile_entry.py`): an entry with no `@stream` and no
  target writes `no_stream_defined_because` with the sample; with a target it stays
  `not_defined_because`.
- **api-server:** `logs` on a failed stream answers its records; `run_stream`'s failed stream
  carries `stream_instance`; `remove_stream` leaves no segment.
- **CLI:** `logs --stream <failed>` prints the traceback; `dev` prints a failed stream's record
  once and loads again on save; `run` prints the follow line under a failed stream.
- **Integration** (`tests/stream-on-runtime/`, GPU-free): `tatolab run` on a `stream.py` with a
  syntax error, then `tatolab logs --stream <folder>-<hash>` shows the traceback, before and
  after a `tatolabd` restart; `tatolab rm` of it leaves no segment of it under `.streamlib/logs/`.

## Out of scope

- Serving a stopped stream's last log from disk — the decision names a failed one.
- Rotating or capping the old groups of a stream that is never removed.
- Tatolab.app reading `logs` for a failed stream: tatolab-app #100 reads `run_stream`.

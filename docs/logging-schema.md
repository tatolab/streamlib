# StreamLib JSONL logging schema

This is the **durable interface contract** for logs emitted by the
StreamLib runtime. Every line of every segment of a project's log, under the
project's `.streamlib/logs/` (see [Files and rotation](#files-and-rotation)), and
of the runtime's own log is one serialized [`RuntimeLogEvent`][rs]. Downstream
consumers — the local API's `logs` tool, `tatolab logs`, which renders each record
with the runtime's own `format_event_pretty` from the
`streamlib-runtime-client-contract` crate, and any tool that reads a project's
segments — depend on this shape.

**Schema changes are expensive.** Adding a new optional field is fine;
renaming or removing an existing field, or changing its type, requires
bumping `schema_version` and coordinating updates across every
downstream consumer.

Current schema version: **1**.

## One line, one event

Each JSONL line is a UTF-8 JSON object with no trailing comma, followed
by `\n`. Lines are newline-aligned by construction — batched flushes
only write whole records, so a hard crash mid-batch leaves at most the
last in-memory batch missing (see [Durability](#durability) below).

## Routing: one log per project

A process installs one logging pathway, once, and keeps it for its life; every
runtime in the process logs through it. Each record is routed to the project that
emitted it, the project being the only key a log has: a stream is something a
project defines any number of, and nothing maps a log back through a stream's
record once the stream is removed or the runtime restarts.

- Every stream loaded from a project, and every load of it, shares the project's
  one route. A thread carries an entry of that route: the route plus the runtime's
  id, and the emitting stream's id when a stream emits.
- A thread a stream owns — its processor threads, a helper process's stdout /
  stderr readers and bridge threads (which receive the helper's `{op:"log"}`
  records), its describe readers, its shutdown thread, and the threads and
  device callbacks the engine and its built-ins start on the stream's behalf
  (render, capture and playback threads, audio clocks, camera and audio device queues, the runtime
  publisher's reserved-slot forwarder) — carries the stream's entry from the start. A thread a Rust
  processor starts itself carries the entry when its closure is wrapped in
  `carrying_this_threads_project_log_route_entry`; otherwise its records reach the
  pretty mirror only. Engine code acting on a stream from a shared thread (a start
  or stop, a graph change and the compile it triggers) carries the entry for the
  duration.
- A run or a start of a project's streams enters the project's route, with no
  stream, before its venv step and its compile, so the lines `uv sync` and the
  compile write to their standard error (labels `project-venv-uv-sync` and
  `stream-function-compile`) land in the project's log with no `stream`. A
  directory that holds no `pyproject.toml` is no project, and its run's records
  go to the runtime's own log.
- The writer stamps each record from the entry it was emitted in: `runtime_id`
  and `stream`. A project's records land in that project's log and
  nowhere else, so one project's records never land in another's file.
- A record no project emitted — the runtime's own, the runtime process's own
  captured stdout / stderr, iceoryx2's — carries no `stream`. When its runtime
  keeps an own log (`RunnerConstructionOptions::runtime_own_log_directory`), the
  record is written there, to `tatolabd-<started_at_millis>.jsonl`, carrying the
  runtime's id, and rotates as a project's log does; otherwise its `runtime_id` is
  empty and it reaches the pretty mirror only.
- Every record, routed or not, is mirrored to the pretty log stream (stderr under
  `tatolabd`, stdout for an engine a Rust app hosts), unless `STREAMLIB_QUIET` is
  set.

## Files and rotation

A project's log is one or more **segments**, all in
`<project_directory>/.streamlib/logs/`:

| Segment | File name |
| --- | --- |
| Active — the one being written | `project.jsonl` |
| Rotated | `project.<seq>.jsonl` |

- The active segment is appended to across the runtime's runs; one runtime runs
  per machine, so a project's log has one writer. Segments a stream wrote before
  the log was the project's, `<runtime_id>-<stream>-<started_at_millis>.jsonl`,
  are neither read nor removed.
- The route opens the active segment when the first stream or load of the project
  enters it, and closes it when the last one is done with it. As each stream
  unloads, its own records are flushed and `fdatasync`ed once the readers of its
  helpers' stdout / stderr and bridges have reached the end of what the helpers
  wrote (bounded at two seconds) and every record queued for it before then is
  written (bounded at two seconds more); a warning names a bound it outlasted.
- The active segment always keeps the un-numbered name. When it holds
  `STREAMLIB_LOG_ROTATE_BYTES` or more after a flush, the writer creates an empty
  `project.jsonl.rotating`, renames the active segment to the next `<seq>`, then
  renames the `.rotating` file to the active name. The active name is absent
  between the two renames, and a rotation that fails partway is backed out, giving
  the name back to the same file. `seq` starts one past the highest rotated segment
  already on disk (`1` for a new log) and increments per rotation, so a higher
  `seq` holds newer records and the active segment is newer than every rotated
  one. The runtime's own log rotates the same way, as
  `tatolabd-<started_at_millis>.<seq>.jsonl`.
- Rotation happens only between whole batches, so every segment ends on a
  newline, no record is split across two segments, and no record appears in two.
  A segment can therefore pass the threshold by up to one batch.
- Retention keeps at most `STREAMLIB_LOG_RETAIN_SEGMENTS` segments per project's
  log, **the active one included**; opening a writer and every rotation delete
  each rotated segment on disk that falls outside it, so a segment that failed to
  delete is retried at the next rotation. Rotation and retention never `fsync`;
  the durability contract below applies per batch as before, and a clean shutdown
  never rotates.
- A full queue drops the oldest record; each emitter's next record in the
  project's log is preceded by a `dropped=N` synthetic record counting its own
  dropped records. Dropped records no project emitted are counted in a synthetic
  record the pretty mirror shows.

To read a project's log in order: its rotated segments by ascending `seq`, then
the active segment. `project_log_records_after` in the
`streamlib-runtime-client-contract` crate does this, and is what the `logs` tool
answers with: at most `count` records after an opaque cursor, narrowed to one
`stream` when it is given, and the cursor after the last. A cursor survives a
restart; one whose segment retention deleted, or that no longer fits the segment
it names, resumes at the oldest record held and says so (`records_no_longer_held`). A line
still being written, or one that is no JSON record, is not answered.

## Fields

| Field | Type | Nullable | Notes |
| --- | --- | --- | --- |
| `schema_version` | integer | no | Bumped on breaking schema changes. |
| `host_ts` | integer | no | Host wall-clock timestamp (nanoseconds since UNIX epoch). Stamped on the emitting thread for a Rust record, and at receipt for a record a helper process sends. Not monotonic — see [Ordering](#ordering). |
| `runtime_id` | string | no | The id ([`RuntimeUniqueId`][rs_id]) of the runtime whose project emitted the record. Every record in a project's log and in the runtime's own log carries it; it is empty only on a record no project emitted while its runtime keeps no own log. |
| `stream` | string | yes (absent) | The id of the loaded stream that emitted the record. Absent on a record of a project's run or start that no stream emitted — its venv step's and its compile's lines — and on a record no project emitted. |
| `source` | enum | no | `"rust"` \| `"python"`, the [`Source`][rs] enum. Rust events come from the `tracing` pipeline — the runtime process's own, or a helper process's, captured there and relayed over the `{op:"log"}` escalate IPC; python events come from `tatolab.stream.log.*` in a helper process via that same IPC, or from a helper process's captured stdout / stderr. |
| `level` | enum | no | `"trace"` \| `"debug"` \| `"info"` \| `"warn"` \| `"error"`. |
| `message` | string | no | Primary human-readable message. May be empty for events that carry only structured fields. |
| `target` | string | no | Tracing target (module path, typically) for Rust, a record relayed from a helper process included — a call site keeps its own target wherever it ran; for a Python event, the target its helper process declared. |
| `pipeline_id` | string | yes | Pipeline identifier. `null` for runtime-level events. |
| `processor_id` | string | yes | Processor identifier. `null` for events outside a processor. |
| `rhi_op` | string | yes | RHI operation name (`"acquire_texture"`, `"acquire_pixel_buffer"`, `"queue_submit"`, …). Set only inside RHI call sites. |
| `source_ts` | string | yes | Helper-process wall-clock timestamp (ISO8601). Advisory only — never used for ordering. Set only on records a helper process sends via the `{op:"log"}` escalate IPC, whichever source they carry; on a captured engine record it is when the engine made the record, not when the helper got round to sending it. `null` otherwise. |
| `source_seq` | integer | yes | Helper-process sequence number: starts at `1` and increments per record the helper sends via the `{op:"log"}` escalate IPC, its captured engine records included — one sequence per helper, not one per source. One helper hosts one processor, so the sequence is per `(runtime_id, processor_id)`, and a new helper process for that processor starts again at `1`. `null` on every other record. |
| `intercepted` | bool | no (default `false`) | `true` when the record came from fd-level capture of stdout / stderr (a raw fd write, a Python `print()`, a third-party library's output) rather than a direct `tracing` / `tatolab.stream.log.*` call. |
| `channel` | string | yes | `"fd1"` (stdout) or `"fd2"` (stderr) when `intercepted: true`. `null` otherwise. |
| `attrs` | object<string, any> | yes (default `{}`) | User-supplied structured fields captured from the emitting call site. For Rust, anything passed to `tracing::info!(foo = 123, bar = "abc", "msg")` other than the well-known fields above; for a Python event, the `**attrs` / `attrs` object passed to `tatolab.stream.log.*`. |

### iceoryx2's records

A record iceoryx2 emits has target `"iceoryx2"`, its message in `message` and
where iceoryx2 says it came from in `attrs.origin`, a string — for many records
the leading part of an iceoryx2 object's `Debug`. Both are written on one line,
every control character as a space, and bounded: at most 128 bytes of `origin`
and 512 of `message` are kept. A longer field is cut at a character boundary
within its cap and then carries `… [truncated at <cap> bytes]`, so a cut field
is at most its cap plus that marker.

## Ordering

- **Within a project**: file order — rotated segments by ascending `seq`,
  then the active segment — is the order the drain worker wrote the
  records in, and every source of every stream of the project (its own
  Rust, each helper process, the venv step and the compile) shares that
  one log. `host_ts` is not
  monotonic in it: many threads stamp a record and then race into one
  queue, and a wall-clock step moves `host_ts` backwards. Sorting a
  project's records by `host_ts` can reorder records its file holds in
  order.
- **Within a helper process**: `source_seq` recovers the order the
  helper sent its `{op:"log"}` records in, and a jump in it means
  records between the two were lost. A record names its processor but
  not its helper process, so a `source_seq` that falls for one
  `(runtime_id, processor_id)` marks a new helper process, and a loss
  that straddles that boundary cannot be detected from the records
  alone.
- **Across projects and runtimes**: each project writes its own segments
  and nothing in the runtime merges them. `host_ts` is the only field
  comparable across projects, and across runtimes only as far as their
  hosts' clocks agree.

## Interceptors

Records tagged `intercepted: true` come from a capture layer rather than
a direct `tracing` / `tatolab.stream.log.*` call. The three enforcement
layers are:

1. **Compile-time (Rust)**: clippy `disallowed-macros` rejects
   `println!` / `eprintln!` / `print!` / `eprint!` / `dbg!` in library
   code.
2. **CI lint (Rust + Python)**: `cargo xtask lint-logging` walks Rust
   library code for the same macros and rejects `print(`, `sys.stdout`,
   `sys.stderr`, `logging.basicConfig` in the wheel's Python source.
3. **Runtime capture**: on Unix a runtime redirects its own process's
   stdout / stderr through pipes while an engine lives in it, and the
   host pipes each helper process's stdout / stderr. Every captured line
   routes through the unified pathway tagged `intercepted: true` at
   `warn` level, with `channel` `fd1` or `fd2`. A helper's lines land in
   its stream's log; the runtime process's own cannot be told apart by
   stream, so they reach the pretty mirror, and the runtime's own log when
   it keeps one.

All three layers are intentional — clippy and the xtask lint keep
first-party code honest at compile/CI time; the runtime interceptors
catch anything the lint can't see (third-party deps, transitive C
calls, fd writes from native modules).

## Durability

- **Clean shutdown** (a stream's unload — SIGTERM-driven or explicit —
  or a test pathway's `StreamlibLoggingGuard::drop`): **zero loss** within two
  bounds. Every record the stream queued in its project's log before its unload — its helpers' last
  lines included — is written, flushed and `fdatasync`'d before the unload
  returns, provided its helpers' pipe readers reach their end within two seconds
  and the drain worker writes what was queued within two seconds more. A record
  either bound cuts off reaches the pretty mirror only, and a warning says so.
- **Hard crash** (SIGKILL, abort, power loss): up to the last
  `STREAMLIB_LOG_BATCH_MS` milliseconds of in-memory records may be
  lost. **Previously flushed batches are always complete on disk** —
  flushes align to newline boundaries, so a crash mid-batch cannot
  produce a torn JSONL line.
- **Panic**: the panic hook the process's logging pathway installs sends a
  best-effort flush to the drain worker and sleeps briefly before
  passing control to the previous hook, so records emitted up to the
  panic usually land on disk. This is best-effort — do not rely on
  panic-time records for accounting.

No `fsync` runs on every batch by default. That would cost 1–100 ms per
batch on typical storage and destroy throughput. Operators who want
harder durability can set `STREAMLIB_LOG_FSYNC_ON_EVERY_BATCH=1`.

## Tunables

Environment variables override construction-time defaults. Defaults are
listed here; see [`StreamlibLoggingConfig`][rs_config] for the full
type.

| Variable | Default | Effect |
| --- | --- | --- |
| `STREAMLIB_QUIET` | unset (`0`) | When `1`, suppresses the pretty log mirror only — on stderr for `tatolabd`, whose stdout carries nothing, and on stdout for an engine a Rust app hosts. JSONL continues writing. |
| `STREAMLIB_LOG_BATCH_BYTES` | `65536` | Size threshold for JSONL flush. |
| `STREAMLIB_LOG_BATCH_MS` | `100` | Time threshold for JSONL flush. |
| `STREAMLIB_LOG_CHANNEL_CAPACITY` | `65536` | Bounded MPMC channel depth. Drop-oldest when full. |
| `STREAMLIB_LOG_FSYNC_ON_EVERY_BATCH` | `0` | When `1`, `fdatasync` after every size/time-triggered flush. Massive throughput cost; only enable when the operating environment requires per-batch durability. |
| `STREAMLIB_LOG_ROTATE_BYTES` | `104857600` (100 MiB) | Size at which the active segment rotates. `0` never rotates, so one segment grows for the log's life. |
| `STREAMLIB_LOG_RETAIN_SEGMENTS` | `10` | Segments kept per project's log, the active one included. `0` keeps every segment. |

## Example

```json
{"schema_version":1,"host_ts":1700000000000000000,"runtime_id":"Rabc123","stream":"main","source":"rust","level":"info","message":"processor started","target":"streamlib_media_builtins::camera_source","pipeline_id":"pl-42","processor_id":"camera-1","rhi_op":null,"intercepted":false,"attrs":{"device":"/dev/video0"}}
```

Parses cleanly via:

```rust
let line = r#"{"schema_version":1,"host_ts":1700000000000000000,"runtime_id":"Rabc123","source":"rust","level":"info","message":"hi","target":"test","intercepted":false}"#;
let event: streamlib_runtime_client_contract::runtime_log_event::RuntimeLogEvent = serde_json::from_str(line).unwrap();
assert_eq!(event.runtime_id, "Rabc123");
```

## Evolution

1. **Adding a field** (backwards-compatible): add with
   `#[serde(default, skip_serializing_if = "Option::is_none")]` or a
   suitable default; keep `schema_version` unchanged.
2. **Renaming a field**: bump `schema_version` and have every consumer
   update in the same release window. Consumers should treat an unknown
   `schema_version` as a signal to warn and fall back to best-effort
   parsing.
3. **Removing a field**: bump `schema_version`. Downstream readers that
   depended on the removed field need to be updated before the
   removal lands.

See parent issue #430's "AI Agent Notes" for the framing around why
this schema is deliberately minimal (no OTLP spans, no SQLite, no
nested tracing context).

[rs]: ../runtime/streamlib-runtime-client-contract/src/runtime_log_event.rs
[rs_id]: ../runtime/streamlib-engine/src/core/runtime/runtime_unique_id.rs
[rs_config]: ../runtime/streamlib-engine/src/core/logging/config.rs

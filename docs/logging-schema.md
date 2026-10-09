# StreamLib JSONL logging schema

This is the **durable interface contract** for logs emitted by the
StreamLib runtime. Every line of every segment a loaded stream writes under
its project's `.streamlib/logs/` (see [Files and rotation](#files-and-rotation))
is one serialized [`RuntimeLogEvent`][rs]. Downstream consumers — `tatolab logs`,
which renders each record with the runtime's own `format_event_pretty` from the
`streamlib-runtime-client-contract` crate, and any tool that reads a stream's
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

## Routing: one log per loaded stream

A process installs one logging pathway, once, and keeps it for its life; every
runtime in the process logs through it. Each record is routed to the loaded
stream that emitted it:

- A thread the stream owns — its processor threads, a helper process's stdout /
  stderr readers and bridge threads (which receive the helper's `{op:"log"}`
  records), its describe readers, its shutdown thread, and the threads and
  device callbacks the engine and its built-ins start on the stream's behalf
  (render, capture and playback threads, a processor-owned window's present
  loop, audio clocks, camera and audio device queues, a channel tap's
  forwarder) — carries the stream's route from the start. A thread a Rust
  processor starts itself carries the route when its closure is wrapped in
  `streamlib::sdk::logging::carrying_this_threads_loaded_stream_log_route`;
  otherwise its records reach the pretty mirror only.
  Engine code acting on a stream from a shared thread (a load, a start or stop, a
  graph change and the compile it triggers) carries the route for the duration.
- A routed record carries the stream's name in `stream` and its runtime's id in
  `runtime_id`, and is written to that stream's JSONL file and nowhere else, so
  one stream's records never land in another's file.
- A record no stream emitted — the runtime's own, the runtime process's own
  captured stdout / stderr, iceoryx2's — carries no `stream` and an empty
  `runtime_id`, and reaches the pretty mirror only; no runtime-level JSONL file is
  written.
- Every record, routed or not, is mirrored to the pretty log stream (stderr under
  `tatolabd`, stdout for an engine a Rust app hosts), unless `STREAMLIB_QUIET` is
  set.

## Files and rotation

Each loaded stream writes one or more **segments**, all in
`<project_directory>/.streamlib/logs/` — the stream's own project:

| Segment | File name |
| --- | --- |
| Active — the one being written | `<runtime_id>-<stream>-<started_at_millis>.jsonl` |
| Rotated | `<runtime_id>-<stream>-<started_at_millis>.<seq>.jsonl` |

- `<stream>` is the stream's URL-safe cast name with every `.` written as `_`.
  `<runtime_id>-<stream>` together is the `RUNTIME_ID-STREAM` `tatolab logs` reads
  (`tatolab logs --list` lists it; `tatolab logs <runtime_id>-<stream>` reads
  it) — the segment name is parsed from the right, so the dashes the two carry
  never split it. `tatolab logs` reads `<STREAMLIB_HOME>/.streamlib/logs/`,
  `STREAMLIB_HOME` defaulting to its working directory, so run from the project
  directory it reads that project's streams.
- The active segment opens as the stream loads, and is flushed, `fdatasync`ed and
  closed as it unloads, once the readers of its helpers' stdout / stderr and
  bridges have reached the end of what the helpers wrote (bounded at two
  seconds) and every record queued for it before then is written (bounded at two
  seconds more). A record the stream emits after that, or one either bound cut
  off, reaches the pretty mirror only, and a warning names the bound it outlasted.
- `started_at_millis` is the wall-clock moment the stream loaded, so loading the
  same stream again — or a restart under a pinned `STREAMLIB_RUNTIME_ID` — starts
  a new set of segments.
- The active segment always keeps the un-numbered name. When it holds
  `STREAMLIB_LOG_ROTATE_BYTES` or more after a flush, the writer creates an empty
  `<runtime_id>-<stream>-<started_at_millis>.jsonl.rotating`, renames the active segment to
  the next `<seq>`, then renames the `.rotating` file to the active name. The
  active name is absent between the two renames, and a rotation that fails partway
  is backed out, giving the name back to the same file. `seq` starts one past the
  highest rotated segment already on disk (`1` for a new stream log) and increments per
  rotation, so a higher `seq` holds newer records and the active segment is newer
  than every rotated one.
- `seq` is separated by a **dot**, never a dash: `runtime_id` may itself contain
  dashes and dots and `<stream>` dashes, while `started_at_millis` and `seq` are
  always decimal digits.
  An active stem always ends in `-<digits>`, so the text after its last dot is
  never all digits and the two shapes cannot be confused.
- Rotation happens only between whole batches, so every segment ends on a
  newline, no record is split across two segments, and no record appears in two.
  A segment can therefore pass the threshold by up to one batch.
- Retention keeps at most `STREAMLIB_LOG_RETAIN_SEGMENTS` segments per stream
  log instance, **the active one included**; opening a writer and every rotation
  delete each rotated segment on disk that falls outside it, so a segment that
  failed to delete is retried at the next rotation. Rotation and retention never `fsync`; the
  durability contract below applies per batch as before, and a clean shutdown
  never rotates.
- A full queue drops the oldest record; each stream's log gets a `dropped=N`
  synthetic record counting that stream's own dropped records, not per segment.
  Dropped records no stream emitted are counted in a synthetic record the pretty
  mirror shows.

To read a stream's log in order: its rotated segments by ascending `seq`, then the
active segment. A reader following the active segment detects a rotation when
the active name points at a *different* file than the one it holds open — a
missing name is not yet a rotation. It then finishes the held file, finds which
`seq` that file became by matching its inode, reads the segments rotated after
it, and reopens the active name. `tatolab logs --follow` does this.

## Fields

| Field | Type | Nullable | Notes |
| --- | --- | --- | --- |
| `schema_version` | integer | no | Bumped on breaking schema changes. |
| `host_ts` | integer | no | Host wall-clock timestamp (nanoseconds since UNIX epoch). Stamped on the emitting thread for a Rust record, and at receipt for a record a helper process sends. Not monotonic — see [Ordering](#ordering). |
| `runtime_id` | string | no | The id ([`RuntimeUniqueId`][rs_id]) of the runtime whose stream emitted the record. Every record in a stream's file carries it; it is empty only on a record no stream emitted, which reaches the pretty mirror and never a file. |
| `stream` | string | yes (absent) | The URL-safe cast name of the loaded stream that emitted the record, as loaded — dots kept. Every record in a stream's file carries it. Absent on a record no stream emitted, and on records written before the field existed. |
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

## Ordering

- **Within a stream**: file order — rotated segments by ascending `seq`,
  then the active segment — is the order the drain worker wrote the
  records in, and every source of a stream (its own Rust, each helper
  process) shares that one log. `host_ts` is not
  monotonic in it: many threads stamp a record and then race into one
  queue, and a wall-clock step moves `host_ts` backwards. Sorting a
  stream's records by `host_ts` can reorder records its file holds in
  order.
- **Within a helper process**: `source_seq` recovers the order the
  helper sent its `{op:"log"}` records in, and a jump in it means
  records between the two were lost. A record names its processor but
  not its helper process, so a `source_seq` that falls for one
  `(runtime_id, processor_id)` marks a new helper process, and a loss
  that straddles that boundary cannot be detected from the records
  alone.
- **Across streams and runtimes**: each stream writes its own segments and
  nothing in the runtime merges them. `host_ts` is the only field
  comparable across streams, and across runtimes only as far as their
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
   stream, so they reach the pretty mirror only.

All three layers are intentional — clippy and the xtask lint keep
first-party code honest at compile/CI time; the runtime interceptors
catch anything the lint can't see (third-party deps, transitive C
calls, fd writes from native modules).

## Durability

- **Clean shutdown** (a stream's unload — SIGTERM-driven or explicit —
  or a test pathway's `StreamlibLoggingGuard::drop`): **zero loss** within two
  bounds. Every record the stream queued before its unload — its helpers' last
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
| `STREAMLIB_LOG_ROTATE_BYTES` | `104857600` (100 MiB) | Size at which the active segment rotates. `0` never rotates, so one segment grows for the stream's life. |
| `STREAMLIB_LOG_RETAIN_SEGMENTS` | `10` | Segments kept per stream log instance, the active one included. `0` keeps every segment. |

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

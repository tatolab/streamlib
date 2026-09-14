# StreamLib JSONL logging schema

This is the **durable interface contract** for logs emitted by the
StreamLib runtime. Every line of every segment under
`<STREAMLIB_HOME>/.streamlib/logs/` (see [Files and rotation](#files-and-rotation))
is one serialized [`RuntimeLogEvent`][rs]. Downstream consumers — the wheel's
`streamlib logs` (`sdk/streamlib-python-wheel/python/streamlib/_runtime_log_reader.py`)
and any tool that reads a runtime's segments — depend on this shape.

> ~~polyglot SDKs, the future orchestrator~~ — Superseded 2026-09-14: the Python
> wheel is the only polyglot SDK and the orchestrator is retired.

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

## Files and rotation

One runtime instance writes one or more **segments**, all in
`<STREAMLIB_HOME>/.streamlib/logs/`:

| Segment | File name |
| --- | --- |
| Active — the one being written | `<runtime_id>-<started_at_millis>.jsonl` |
| Rotated | `<runtime_id>-<started_at_millis>.<seq>.jsonl` |

- `started_at_millis` is the wall-clock start of the runtime instance, so a
  restart under a pinned `STREAMLIB_RUNTIME_ID` starts a new set of segments.
- The active segment always keeps the un-numbered name. When it holds
  `STREAMLIB_LOG_ROTATE_BYTES` or more after a flush, the writer creates an empty
  `<runtime_id>-<started_at_millis>.jsonl.rotating`, renames the active segment to
  the next `<seq>`, then renames the `.rotating` file to the active name. The
  active name is absent between the two renames, and a rotation that fails partway
  is backed out, giving the name back to the same file. `seq` starts one past the
  highest rotated segment already on disk (`1` for a new runtime) and increments per
  rotation, so a higher `seq` holds newer records and the active segment is newer
  than every rotated one.
- `seq` is separated by a **dot**, never a dash: `runtime_id` may itself contain
  dashes and dots, while `started_at_millis` and `seq` are always decimal digits.
  An active stem always ends in `-<digits>`, so the text after its last dot is
  never all digits and the two shapes cannot be confused.
- Rotation happens only between whole batches, so every segment ends on a
  newline, no record is split across two segments, and no record appears in two.
  A segment can therefore pass the threshold by up to one batch.
- Retention keeps at most `STREAMLIB_LOG_RETAIN_SEGMENTS` segments per runtime
  instance, **the active one included**; opening a writer and every rotation
  delete each rotated segment on disk that falls outside it, so a segment that
  failed to delete is retried at the next rotation. Rotation and retention never `fsync`; the
  durability contract below applies per batch as before, and a clean shutdown
  never rotates.
- The `dropped=N` synthetic record counts per runtime, not per segment.

To read a runtime in order: its rotated segments by ascending `seq`, then the
active segment. A reader following the active segment detects a rotation when
the active name points at a *different* file than the one it holds open — a
missing name is not yet a rotation. It then finishes the held file, finds which
`seq` that file became by matching its inode, reads the segments rotated after
it, and reopens the active name. `streamlib logs --follow` does this.

## Fields

| Field | Type | Nullable | Notes |
| --- | --- | --- | --- |
| `schema_version` | integer | no | Bumped on breaking schema changes. |
| `host_ts` | integer | no | Host wall-clock timestamp (nanoseconds since UNIX epoch). Stamped on the emitting thread for a Rust or app-process Python record, and at receipt for a record a helper process sends. Not monotonic — see [Ordering](#ordering). |
| `runtime_id` | string | no | The owning runtime's id ([`RuntimeUniqueId`][rs_id]). |
| `source` | enum | no | `"rust"` \| `"python"`. Rust events come from the in-process `tracing` pipeline; python events come from `streamlib.log.*` in the app's interpreter, from a helper process via the `{op:"log"}` escalate IPC, or from a helper process's captured stdout / stderr. |
| `level` | enum | no | `"trace"` \| `"debug"` \| `"info"` \| `"warn"` \| `"error"`. |
| `message` | string | no | Primary human-readable message. May be empty for events that carry only structured fields. |
| `target` | string | no | Tracing target (module path, typically) for Rust; subprocess-declared target for polyglot. |
| `pipeline_id` | string | yes | Pipeline identifier. `null` for runtime-level events. |
| `processor_id` | string | yes | Processor identifier. `null` for events outside a processor. |
| `rhi_op` | string | yes | RHI operation name (`"acquire_texture"`, `"acquire_pixel_buffer"`, `"queue_submit"`, …). Set only inside RHI call sites. |
| `source_ts` | string | yes | Helper-process wall-clock timestamp (ISO8601). Advisory only — never used for ordering. Set only on records a helper process sends via the `{op:"log"}` escalate IPC; `null` otherwise. |
| `source_seq` | integer | yes | Helper-process sequence number: starts at `1` and increments per record the helper sends via the `{op:"log"}` escalate IPC. One helper hosts one processor, so the sequence is per `(runtime_id, processor_id)`, and a new helper process for that processor starts again at `1`. `null` on every other record. |
| `intercepted` | bool | no (default `false`) | `true` when the record came from fd-level capture of stdout / stderr (a raw fd write, a Python `print()`, a third-party library's output) rather than a direct `tracing` / `streamlib.log.*` call. |
| `channel` | string | yes | `"fd1"` (stdout) or `"fd2"` (stderr) when `intercepted: true`. `null` otherwise. |
| `attrs` | object<string, any> | yes (default `{}`) | User-supplied structured fields captured from the emitting call site. For Rust, anything passed to `tracing::info!(foo = 123, bar = "abc", "msg")` other than the well-known fields above; for polyglot, the `**attrs` / `attrs` object passed to `streamlib.log.*`. |

> ~~`"deno"` as a `source` value, `host_ts` as a host monotonic timestamp and the
> authoritative sort key across the merged stream, `console.log` / `"logging"` /
> `"stdout"` / `"stderr"` channels~~ — Superseded 2026-09-14: `Source` is `Rust` \|
> `Python` (`runtime/streamlib-engine/src/core/logging/event.rs`), `host_ts` is
> `SystemTime::now()`, and both capture paths tag only `fd1` / `fd2`.

## Ordering

- **Within a runtime**: file order — rotated segments by ascending `seq`,
  then the active segment — is the order the drain worker wrote the
  records in, and every source of a runtime (Rust, app-process Python,
  each helper process) shares that one stream. `host_ts` is not
  monotonic in it: many threads stamp a record and then race into one
  queue, and a wall-clock step moves `host_ts` backwards. Sorting a
  runtime's records by `host_ts` can reorder records its file holds in
  order.
- **Within a helper process**: `source_seq` recovers the order the
  helper sent its `{op:"log"}` records in, and a jump in it means
  records between the two were lost. A record names its processor but
  not its helper process, so a `source_seq` that falls for one
  `(runtime_id, processor_id)` marks a new helper process, and a loss
  that straddles that boundary cannot be detected from the records
  alone.
- **Across runtimes**: each runtime writes its own segments and nothing
  in the runtime merges them. `host_ts` is the only field comparable
  across runtimes, and only as far as their hosts' clocks agree.

> ~~Cross-source: `host_ts` is the authoritative sort key … Within a
> source: FIFO is preserved by the channel — records from the same source
> arrive on the host in the order the source emitted them.~~ — Superseded
> 2026-09-14: `host_ts` is stamped before the record enters the drain
> queue — on the emitting thread for an in-process record, on the receiving
> thread at host receipt for a helper process's record — so neither the file
> nor one source is ordered by it.

## Interceptors

Records tagged `intercepted: true` come from a capture layer rather than
a direct `tracing` / `streamlib.log.*` call. The three enforcement
layers are:

1. **Compile-time (Rust)**: clippy `disallowed-macros` rejects
   `println!` / `eprintln!` / `print!` / `eprint!` / `dbg!` in library
   code.
2. **CI lint (Rust + Python)**: `cargo xtask lint-logging` walks Rust
   library code for the same macros and rejects `print(`, `sys.stdout`,
   `sys.stderr`, `logging.basicConfig` in the wheel's Python source.
3. **Runtime capture**: on Unix a runtime redirects its own process's
   stdout / stderr through pipes, and the host pipes each helper
   process's stdout / stderr. Every captured line routes through the
   unified pathway tagged `intercepted: true` at `warn` level, with
   `channel` `fd1` or `fd2`.

> ~~TypeScript / `console.*` / `Deno.std*` lint patterns; Python `sys.std*` +
> root `logging` interceptors; Deno `globalThis.console` interceptors~~ —
> Superseded 2026-09-14: the Deno SDK is gone, `xtask/src/lint_logging.rs`
> scans Rust and Python only, and Python output is captured at the fd by the
> host, never inside the interpreter.

All three layers are intentional — clippy and the xtask lint keep
first-party code honest at compile/CI time; the runtime interceptors
catch anything the lint can't see (third-party deps, transitive C
calls, fd writes from native modules).

## Durability

- **Clean shutdown** (`StreamlibLoggingGuard::drop`, SIGTERM-driven
  shutdown, or explicit `fdatasync` path): **zero loss**. All buffered
  records are flushed and `fdatasync`'d before the process exits.
- **Hard crash** (SIGKILL, abort, power loss): up to the last
  `STREAMLIB_LOG_BATCH_MS` milliseconds of in-memory records may be
  lost. **Previously flushed batches are always complete on disk** —
  flushes align to newline boundaries, so a crash mid-batch cannot
  produce a torn JSONL line.
- **Panic**: the panic hook installed by `logging::init` sends a
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
| `STREAMLIB_QUIET` | unset (`0`) | When `1`, suppresses the pretty stdout mirror only. JSONL continues writing. |
| `STREAMLIB_LOG_BATCH_BYTES` | `65536` | Size threshold for JSONL flush. |
| `STREAMLIB_LOG_BATCH_MS` | `100` | Time threshold for JSONL flush. |
| `STREAMLIB_LOG_CHANNEL_CAPACITY` | `65536` | Bounded MPMC channel depth. Drop-oldest when full. |
| `STREAMLIB_LOG_FSYNC_ON_EVERY_BATCH` | `0` | When `1`, `fdatasync` after every size/time-triggered flush. Massive throughput cost; only enable when the operating environment requires per-batch durability. |
| `STREAMLIB_LOG_ROTATE_BYTES` | `104857600` (100 MiB) | Size at which the active segment rotates. `0` never rotates, so one segment grows for the runtime's life. |
| `STREAMLIB_LOG_RETAIN_SEGMENTS` | `10` | Segments kept per runtime instance, the active one included. `0` keeps every segment. |

## Example

```json
{"schema_version":1,"host_ts":1700000000000000000,"runtime_id":"Rabc123","source":"rust","level":"info","message":"processor started","target":"streamlib::linux::processors::camera","pipeline_id":"pl-42","processor_id":"camera-1","rhi_op":null,"intercepted":false,"attrs":{"device":"/dev/video0"}}
```

Parses cleanly via:

```rust
let line = r#"{"schema_version":1,"host_ts":1700000000000000000,"runtime_id":"Rabc123","source":"rust","level":"info","message":"hi","target":"test","intercepted":false}"#;
let event: streamlib::logging::RuntimeLogEvent = serde_json::from_str(line).unwrap();
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

[rs]: ../runtime/streamlib-engine/src/core/logging/event.rs
[rs_id]: ../runtime/streamlib-engine/src/core/runtime/runtime_unique_id.rs
[rs_config]: ../runtime/streamlib-engine/src/core/logging/config.rs

# StreamLib JSONL logging schema

This is the **durable interface contract** for logs emitted by the
StreamLib runtime. Every line in
`$XDG_STATE_HOME/streamlib/logs/<runtime_id>-<started_at>.jsonl` is one
serialized [`RuntimeLogEvent`][rs]. Downstream consumers — `streamlib-cli
logs`, polyglot SDKs, the future orchestrator — depend on this shape.

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

## Fields

| Field | Type | Nullable | Notes |
| --- | --- | --- | --- |
| `schema_version` | integer | no | Bumped on breaking schema changes. |
| `host_ts` | integer | no | Host monotonic timestamp (nanoseconds since UNIX epoch). Authoritative sort key across the merged stream. |
| `runtime_id` | string | no | The owning runtime's id ([`RuntimeUniqueId`][rs_id]). |
| `source` | enum | no | `"rust"` \| `"python"` \| `"deno"`. Rust events come from the in-process `tracing` pipeline; python/deno events arrive via the `{op:"log"}` escalate IPC (wired in #442). |
| `level` | enum | no | `"trace"` \| `"debug"` \| `"info"` \| `"warn"` \| `"error"`. |
| `message` | string | no | Primary human-readable message. May be empty for events that carry only structured fields. |
| `target` | string | no | Tracing target (module path, typically) for Rust; subprocess-declared target for polyglot. |
| `pipeline_id` | string | yes | Pipeline identifier. `null` for runtime-level events. |
| `processor_id` | string | yes | Processor identifier. `null` for events outside a processor. |
| `rhi_op` | string | yes | RHI operation name (`"acquire_texture"`, `"acquire_pixel_buffer"`, `"queue_submit"`, …). Set only inside RHI call sites. |
| `source_ts` | string | yes | Subprocess wall-clock timestamp (ISO8601). Advisory only — never used for ordering. `null` when `source="rust"`. |
| `source_seq` | integer | yes | Subprocess-monotonic sequence number. Escape hatch for subprocess-local order. `null` when `source="rust"`. |
| `intercepted` | bool | no (default `false`) | `true` when the record came from an interceptor (captured `print()`, `console.log`, raw fd write, etc.) rather than a direct tracing call. Filled by #438 (Rust fd interceptor), #443 (Python), #444 (Deno). |
| `channel` | string | yes | Interceptor channel identifier when `intercepted: true` (`"stdout"`, `"stderr"`, `"console.log"`, `"logging"`, `"fd1"`, `"fd2"`, …). `null` otherwise. |
| `attrs` | object<string, any> | yes (default `{}`) | User-supplied structured fields captured from the emitting call site. For Rust, anything passed to `tracing::info!(foo = 123, bar = "abc", "msg")` other than the well-known fields above; for polyglot, the `**attrs` / `attrs` object passed to `streamlib.log.*`. |

## Ordering

- **Cross-source**: `host_ts` is the authoritative sort key. Subprocess
  clocks are not synced cheaply with the host, so there is no reliable
  "true origin time" to sort by. Anyone reading the JSONL should sort
  by `host_ts` when merging events from multiple sources.
- **Within a source**: FIFO is preserved by the channel — records from
  the same source arrive on the host in the order the source emitted
  them. `source_seq` (when present) provides a forensic escape hatch
  for recovering that order even after any merge rearranges things.

## Interceptors

Records tagged `intercepted: true` come from a capture layer rather than
a direct `tracing` / `streamlib.log.*` call. The three enforcement
layers specified in #430 are:

1. **Compile-time (Rust)**: clippy `disallowed-macros` rejects
   `println!` / `eprintln!` / `print!` / `eprint!` / `dbg!` in library
   code (#441).
2. **CI lint (Python + TypeScript)**: `cargo xtask lint-logging`
   rejects `print(`, `sys.stdout`, `sys.stderr`, `logging.basicConfig`,
   `console.(log|warn|error|info|debug)`, `Deno.stdout.write`,
   `Deno.stderr.write` in SDK source (#441).
3. **Runtime interceptors**: fd-level redirect for Rust (#438); Python
   `sys.std*` + root `logging` + fd pipes (#443); Deno
   `globalThis.console` + `Deno.stdout` + fd pipes (#444). Every
   intercepted record routes through the unified pathway tagged
   `intercepted: true` at `warn` level.

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

[rs]: ../libs/streamlib/src/core/logging/event.rs
[rs_id]: ../libs/streamlib/src/core/runtime/runtime_unique_id.rs
[rs_config]: ../libs/streamlib/src/core/logging/config.rs

# stream-identity-and-persistence

> **Approved by the owner, 2026-10-28:** the proposal was merged as #2955, and Decided 5
> (a Rust app's stream) was settled as option A ("a is fine").

Built from #2950 and the align merged in #2954 (§Product: "A stream's location, id and display
name" and "Every loaded stream persists"; ADR `docs/decisions/stream-identity-and-persistence.md`).
Today a stream's name is its function's, `main` in every scaffold. It is cast, and it keys
everything: the stream table, the address, the broadcast, the record, the logs and the crash
pins. A stream loaded attached is unloaded and forgotten when its `/mcp/stdio` connection closes.

**After this change:**
- **Identity.** A stream is identified by its **location**: its project directory, resolved,
  plus its stream function as `module:function`. It is keyed and addressed by a **stream id**,
  `<folder>-<function>-<hash>`, which the runtime computes from the location.
- **Display name.** It carries a renamable **display name**.
- **Persistence.** Every load is recorded. `keep`, `run -d` and the attached state go away.
  `run` loads and returns; `logs -f` and `dev` follow.

**Scale gate — this skill. The ADR already exists from the align.**
- **Contracts changed:**
  - the local API's stream tools and their results, plus a new `rename_stream`;
  - the CLI's stream verbs;
  - the stream's address and broadcast;
  - the state directory's stream record;
  - the compile document the runtime reads from the project's interpreter.
- **Not touched:** the RHI, the iceoryx2 wire, the processor model, and `@stream` and the rest
  of the authoring API.

**Precondition.** All four touched sections are DECIDED: §Product, §Processor model's failure
isolation, §Networking's address, and §Control plane's tool and CLI inventories. Each carries
`amended by stream-identity-and-persistence`.

## Current state (recon, 2026-10-28, `main` `f5b1b3728`)

Paths are under `runtime/` unless marked otherwise.

- **The stream table** is a `BTreeMap<String, Arc<LoadedStreamInThisRuntime>>` keyed by cast
  name (`streamlib-engine/src/core/runtime/runtime.rs:98-101`).
  - A loaded name is refused by `refuse_a_stream_name_already_loaded` (`:1312`, `:1395`).
  - A tool's `stream` argument is cast, then looked up by name: `the_stream_a_tool_call_names`
    (`streamlib-api-server/src/mcp.rs:259`) → `Runner::loaded_stream_named` (`runtime.rs:939`).
    Stream actions cast through `the_cast_name_of_a_stream_an_action_names`
    (`stream_actions_of_this_runtime.rs:1148`).
- **Where the name comes from.** `@stream` stamps `module:qualname` and `__name__`
  (`sdk/tatolab-stream/tatolab/stream/_stream_graph_builder.py:53-54,105-106`). Only the name
  reaches the graph (`"stream"`, `:406`).
  - The compile document is `{stream_graph, project_directory}`
    (`compiler_ops/stream_function_compile_in_the_projects_interpreter.rs:67-71`). It carries no
    `module:function`.
  - Entry-file mode runs as `__main__` (`_project_stream_entry_execution.py:58`).
  - The runtime passes `--name` into the compile (`:129-130`). The compile refuses a name that
    casts to nothing (`_project_stream_entry_execution.py:187-193`).
- **Keyed on the name:**
  - the pubsub topic and surface owner key, through `LoadedStreamIdentity{runtime_id,
    stream_name, stream_tag}` (`pubsub/events.rs:23,55`, `loaded_stream.rs:372`);
  - the broadcast `"{root}/{stream_name}.hang"`, keyed `(relay_id, stream_name)`
    (`tatolabd/src/exposed_port_publisher.rs:414-417,723,896`);
  - the log file `<project>/.streamlib/logs/<rid>-<stream>-<millis>.jsonl`
    (`runtime_log_file_paths.rs:25-48`);
  - crash pins, which carry the name on threads and in `runtime-run-in-progress`
    (`runtime_crash_pinned_on_a_stream.rs:28,75,232-236`);
  - `GraphResponse.stream` (`json_schema.rs:35`);
  - the watch page's `GET /` list (`tatolab-watch/main.ts:40-45,91`).
- **The record.** `KeptStreamRecord` lives in the engine (`machine_state_directory.rs:31-56`):
  schema 2, with `stream_name`, `project_directory`, `interpreter`, `stream_function` (an
  `Option<String>` as typed, so `stream.py:main` ≠ `stream:main`, refused at
  `stream_actions_of_this_runtime.rs:1493`), `graph`, `stopped`, `failed_because`, the crash
  count and `exposure_rulings`.
  - Its file is `streams/<cast name>.json`, and the stem must equal the cast (`:369,517-534`).
  - It is re-loaded at start by `reload_each_kept_stream_listed`
    (`stream_actions_of_this_runtime.rs:898-1000`); the crash count is at `:1014-1075`.
- **Attached.**
  - `RunStreamToolArguments.keep` (`mcp.rs:413-431`) maps to `LoadedStreamHolding::Kept` or
    `::Attached` (`:734-738`).
  - `StreamsAttachedToOneMcpStdioConnection` (`:131-207`) unloads on close
    (`mcp_stdio_upgrade.rs:154-155`).
  - An attached replace hangs on caller tags (`stream_actions_of_this_runtime.rs:445-512,
    1243-1322`).
  - Exposure rulings are recorded only for a kept stream (`:822-835`).
  - The library `Runner` loads `Attached` (`runtime.rs:790,809,931`), and so does the Rust `App`
    (`sdk/streamlib-sdk/src/sdk/app.rs:82-88`, one stream named `THE_STREAM_NAME_AN_APP_LOADS`).
- **The wire** (`streamlib-runtime-client-contract/src/local_api_wire_contract.rs`):
  - `RunStreamToolResult{stream, stream_instance, kept, …, replaced_the_kept_record}`
    (`:75-87`);
  - `StopStreamToolResult{stream, kept}` (`:95-101`);
  - `ListStreamsToolResultStream{name, …}` (`:137-148`);
  - `ListedStreamState{Attached, Kept, Stopped, Failed}` (`:153-172`). There is no `running`: a
    loaded kept stream lists `kept`.
- **The CLI.**
  - `run -d` → `run_stream_kept` (`tatolab-cli/src/main.rs:123-125,269-277`). Otherwise `run` and
    `dev` → `attached_stream_on_the_runtime.rs`:
    - it loads with `keep: false` (`:299`);
    - it follows `logs` (`:410-484`);
    - Ctrl-C → `stop_stream` (`:531-558`);
    - a closed connection exits 1 for `run`, while `dev` re-sends `run_stream` itself
      (`:196-205, 585-614`).
  - Stream arguments are sent verbatim (`stream_actions_on_the_runtime.rs:264-268`).
  - `logs` needs `--stream`; otherwise it reads the positional `RUNTIME_ID-STREAM` file
    (`runtime_logs_verb.rs:50-64`).
- **Tatolab.app** lives in the private `tatolab/tatolab-app` (`ARCHITECTURE.md:3429-3433`). It
  sends `run_stream {keep: true}`. `run_stream` denies unknown fields, so the app's call is
  refused once `keep` goes.

## Decided 1 — the location, the stream id and who computes it (owner, 2026-10-27; detail session, 2026-10-28)

- **The compile emits the location.** The compile document gains `stream_function`, the
  function's `module:qualname`. In entry-file mode the module is the entry file's path relative
  to the project directory, `.py` dropped and `/` read as `.`, never `__main__`.
  `project_directory` is already `resolve()`d (`_project_stream_entry_execution.py:205-206`).
- **The runtime computes the id**, in one place, from the compile document:
  `<cast folder>-<cast function>-<hash>`.
  - `<hash>` is the first 6 hex digits of SHA-256 over
    `<project_directory>\n<module:qualname>` in UTF-8.
  - A part that casts to nothing is left out. Folder and function are truncated so the whole
    fits 63 characters, with the hash kept whole.
  - The cast is the existing exposed-name cast
    (`sdk/streamlib-processor-schema/src/exposed_name_cast.rs`).
- **One id per location.** A load whose id is held by a *different* location is refused,
  naming both locations. A load of the *same* location replaces the stream (Decided 3).
- **The graph's `"stream"` stops being an address.** The runtime sets the loaded graph's stream
  to the id. The function's name in the compiled graph becomes the default display name.
  `--name` no longer reaches the compile.

## Decided 2 — the display name (owner, 2026-10-27)

- **Where it lives.** The display name is recorded in the stream record and held in the table
  beside the id. It is free text, never cast, never unique, and never part of an address.
- **How it is set.** A load's `name` (`--name`, `run_stream {name}`) sets it, and a load without
  one keeps the recorded name.
- **`rename_stream {stream, display_name}`** returns `{stream, display_name}` and rewrites the
  record before the table. `tatolab rename [<stream>] <display name>` calls it.

## Decided 3 — every load is recorded (owner, 2026-10-27)

- **Every `run_stream` load is recorded.** The attached machinery goes. Every `run_stream` load
  writes a record before it starts, and the exposure rulings are recorded for every stream.
- **Replace.** A load of a recorded location replaces it under the same id, keeping the display
  name (unless the load gives one) and the rulings. The running stream unloads only once the
  compile has succeeded, and a refused load re-loads it, as the kept replace does today.
- **States.** `ListedStreamState` becomes `Running`, `Stopped` and `Failed`. `StopStreamToolResult`
  loses `kept`; `not_recorded_because` stays. `replaced_the_kept_record` becomes
  `replaced_a_recorded_stream`.
- **Renames.** `KeptStreamRecord` becomes `StreamRecord`, and every `kept_stream*` and
  `*KeptStream*` identifier drops "kept", since every stream is recorded.
- **Old records** (schema 3): a schema-2 record found at start is listed `failed`, with
  `failed_because` saying an earlier runtime recorded it and to run it again. It stays keyed by
  its file stem until `rm`. This is the plan's "cannot re-load → failed, never deleted"; no
  migration is written (pre-1.0).

## Decided 4 — the CLI (owner, 2026-10-27)

- **`run`** loads over a one-shot call and prints:

  ```
  <id> running as "<display name>" (project <dir>, <module:function>)
    follow it: tatolab logs -f
  ```

  Each compile warning prints on stderr. `run` exits 0 once the load is accepted, and 1 naming
  the reason when it is refused. `-d`/`--detach` is removed.
- **`logs -f`** follows until Ctrl-C (exit 0). A runtime that goes away ends it with exit 1,
  naming the crash and the runtime's log folder (`describe_runtime`).
- **`dev`**
  - It loads, then follows, and re-loads on each settled save.
  - When the runtime goes away it says so, keeps running, and waits for the runtime. The runtime
    re-loads the stream from its record, and `dev` follows the same id again. `dev` never
    re-sends `run_stream` for a crash.
  - Ctrl-C ends `dev` and leaves the stream loaded.
- **Stream arguments.** Every verb whose stream argument was required now takes it optionally:
  `stop`, `start`, `rm`, `rename`, `expose`, and `logs` (as `--stream`).
  - The argument can be an id, or a display name exactly one stream holds.
  - When omitted, it is the one stream whose location's project directory equals the resolved
    working directory, stopped and failed included.
  - None, or several, is refused, listing each candidate's id and display name.
  - The CLI resolves the argument through `list_streams`, and the tools only ever see ids.
  - `graph --stream` keeps its omitted-means-every-stream meaning, from its own DECIDED entry.
  - `expose`'s positionals: two are `<node> <port>`, three are `<stream> <node> <port>`.
- **`streams`** prints `ID  NAME  STATE  NODES  PROJECT  FUNCTION`, with state `running`,
  `stopped` or `failed`.

## Decided 5 — a Rust app's stream (owner, 2026-10-28)

The Rust `App` (`sdk/streamlib-sdk/src/sdk/app.rs:82-88`) and any library-hosted `Runner` load
a stream into the app's own engine. That stream has no `@stream` location and no state
directory.
- **Its id.** It keeps the id its host names, cast, as `App` does with
  `THE_STREAM_NAME_AN_APP_LOADS`, and its display name defaults to the same name.
- **Its lifetime.** It is never recorded and lives as long as its host process.
- **Scope of the other rules.** The location, the hash and persistence apply to `run_stream`
  loads in `tatolabd`.
- **The holding.** `LoadedStreamHolding` keeps two variants, `RecordedByTheRuntime` and
  `LivesWithTheHostProcess`. Nothing is "attached".

Rejected: a cargo-project location for a Rust app, hashed the same way — it defines a Rust
location nothing else uses (owner, 2026-10-28).

## MODIFIED: §Product (`ARCHITECTURE.md:109-128, 130-150, 201-251, 296-298`)

When this ships, the five `amended by stream-identity-and-persistence` clauses are folded:
- `tatolabd`: `run_stream` takes no `keep`;
- the state directory: one record per stream, named by its id;
- how a stream is loaded and kept: no attached stream, no `run -d`;
- the stream actions;
- several streams in one project: the display name defaults to the function's.

The two new entries lose nothing.

## MODIFIED: §Processor model — failure isolation (`:1489-1506`)

When this ships, the clause is folded: every stream re-loads and counts crashes, and the crash
pins carry the stream id.

## MODIFIED: §Networking — the address (`:2899-2911, 2955-2980`)

When this ships, the clauses are folded:
- the address and broadcast use `<stream id>`;
- the name-collision rule is replaced by the id's hash.

The `RelayTrackSource.broadcast` doc becomes `<machine>/<stream id>.hang`. Its source is the
Rust doc that `cargo xtask generate-built-in-node-classes` renders into `_built_in_nodes.py`
(`:464-466,499-501`).

## MODIFIED: §Control plane — the tool set and the CLI (`:3474-3480, 3542-3552, 3610-3621`)

- **The tool set** gains `rename_stream`: seven stream actions.
- **The CLI** gains `rename`.
- **Server instructions.** `LOCAL_API_MCP_SERVER_INSTRUCTIONS` (`mcp.rs:93`) and each tool
  description drop `keep` and attached, and say that `stream` is an id.
- **REST.** The `?stream=` query (`handlers.rs:161-294`) takes the id.
- **The watch page** lists id and display name, and opens `/watch/<machine>/<stream id>`.

## In scope beyond `runtime/`

- **Docs and skills** describing attached, `-d` or `keep`, or an address spelled with a name:
  - `README.md` (`87, 114, 152-157, 319, 344-364, 407, 435, 467`);
  - `.claude/skills/` — `drive-running-stream`, `list-running-streams`,
    `stop-running-stream`, `read-live-track`, `inspect-live-graph`,
    `capture-stream-evidence`, `verify-live`, `verify-audio`;
  - the scaffold docstring (`sdk/tatolab-stream/scaffold_template/stream.py:6-9`), which says
    `dev` prints the address instead of naming `/watch/<machine>/main`.
- **Engine-tree users of `run -d`:**
  - `installer/install.sh:231`;
  - `adapters/streamlib-adapter-cuda/tests/opaque_fd_wheel_export_foreign_consumer.rs`
    (`24, 113, 331`).

## Not modified

- **Downstream consumers.** The `examples/` READMEs and docstrings that say
  "`--stream main`", "`expose main`", "`<machine>/main.hang`", or "keep the stream as long as the
  command runs" get backlog at those consumers, per the doctrine.
- **Tatolab.app** gets its own ticket in `tatolab/tatolab-app`: drop `keep`, show display names,
  rename by `rename_stream`, and address by id. It ships in the same release as Decided 3.
- **Records.** `docs/research/` and `docs/plan/changes/archive/` are not changed.

## Tests

**Seams.**
- **Python stream suite:** the compile document's `stream_function` for the `stream.py`,
  `<file>.py:<fn>`, `-f` and `<module>:<fn>` forms. Entry-file mode is never `__main__`.
- **Engine:**
  - the id rule (cast, truncation, a part casting to nothing dropped, the hash over the
    location);
  - two locations with one id refused;
  - a same-location replace keeping id, display name and rulings;
  - `rename_stream` rewriting the record first;
  - a schema-2 record listed `failed`.
- **api-server over a real engine:** a `run_stream` over `/mcp/stdio` survives the connection
  closing and lists `running`. `list_streams` returns id, display name and location. Every tool
  resolves by id.
- **tatolabd:**
  - a stream comes back after a restart under the same id;
  - the record is `streams/<id>.json`;
  - the crash count is pinned by id;
  - the broadcast is `<machine>/<id>.hang`.
- **CLI (stub server):**
  - `run` prints the id and returns 0, or 1 on a refused load;
  - `-d` is gone;
  - stream arguments resolve by id, by unique display name and by working directory, and an
    ambiguous one is refused listing candidates;
  - Ctrl-C on `logs -f` and on `dev` never calls `stop_stream`;
  - `dev` survives a dropped runtime without re-sending `run_stream`.
- **`tests/stream-on-runtime/`:**
  - `test_an_attached_stream_loads_into_the_runtime_and_a_ctrl_c_unloads_it` and
    `test_an_attached_run_unloads_on_ctrl_c_and_on_a_killed_cli_while_a_kept_stream_runs_on`
    are rewritten to assert that the stream persists;
  - `conftest.py` stops parsing "Ctrl-C stops it" and reads the id from `run`'s line;
  - fixtures address streams by the id `run` printed.

## Tickets

Derived by `/derive-tickets`, as two tracer bullets, identity first:
1. **Stream identity**, a new ticket: location, id, display name, `rename`, every key and
   address by id, and CLI resolution.
2. **#2950, persistence**: `keep`, `-d` and attached removed; `run` returns; `logs -f` and `dev`
   follow; the tests rewritten. #2950's body is rewritten then.

## REMOVED

- REMOVED: LoadedStreamHolding::Attached
- REMOVED: LoadedStreamHolding::Kept
- REMOVED: StreamsAttachedToOneMcpStdioConnection
- REMOVED: close_and_unload_every_attached_stream
- REMOVED: attach_or_unload_once_closed
- REMOVED: stream_tags_attached_to_the_caller
- REMOVED: RUN_STREAM_ATTACHED_OVER_A_ONE_SHOT_CALL_REFUSAL
- REMOVED: unload_the_attached_stream_if_still_the_same
- REMOVED: attached_stream_run_source
- REMOVED: replace_an_attached_stream
- REMOVED: replaced_the_kept_record
- REMOVED: KeptStreamRecord
- REMOVED: ListedStreamState::Attached
- REMOVED: ListedStreamState::Kept
- REMOVED: StreamListingState::Attached
- REMOVED: refuse_a_stream_name_already_loaded
- REMOVED: a_stream_name_held_under_another_spelling_refusal
- REMOVED: refuse_a_stream_name_that_casts_to_nothing
- REMOVED: run_stream_kept
- REMOVED: run_stream_attached
- REMOVED: runtime/tatolab-cli/src/attached_stream_on_the_runtime.rs
- REMOVED: runtime/tatolab-cli/tests/tatolab_run_and_dev_attached.rs
- REMOVED: ATTACHED_STREAM_STOP_SIGNALS
- REMOVED: ATTACHED_STREAM_LOADED_NOTE_PATTERN
- REMOVED: Ctrl-C stops it
- REMOVED: keeps no record
- REMOVED: lives as long as this connection
- REMOVED: can only keep

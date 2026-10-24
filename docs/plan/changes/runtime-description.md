# runtime-description

> **Approved by the owner, 2026-10-24**, with its four decisions as recommended ("I agree with
> your decisions") and the contract-fixture question settled the same day.

Built from #2922. A local API client can't tell which runtime it is talking to, how long that
runtime has been up, or where the runtime writes its own log. `GET /health` answers `ok`, and no
tool reports any of the three. Tatolab.app's Settings › Runtime (tatolab/tatolab-app#41) waits
on these values. It doesn't show its bundled version as the runtime's, and it doesn't guess the
state directory's layout.

**After this change:**
- **One served description of the running runtime.** It gives:
  - its version and build id;
  - how long it has been up, as monotonic elapsed time. A restart of `tatolabd` resets it.
  - the absolute path of the folder its own log is written to, or `null` when it keeps none.
- **The CLI shows the same values** through the same call, as a pure client.
- **What a stream author sees:** nothing. No node, port, config key or Python surface changes.

**Scale gate — this skill, no ADR.**
- **Contract changes:** the served MCP tool set, which §Control plane fixes as "exactly" a list,
  and the CLI's surface.
- **Not touched:** the RHI, the IPC wire format, the processor model and the Python API.

**Precondition.** Every entry touched is DECIDED and SHIPPED:
- §Product: `tatolabd` (`ARCHITECTURE.md:109-126`) and the state directory (`:127-141`);
- §Control plane & observability: the one control plane (`:3389-3398`) and the CLI
  (`:3456-3468`);
- §Processor model: the closed set of wall-clock surfaces (`:2081-2095`). This change keeps that
  set and does not touch it.

## Current state (recon, 2026-10-24, `main` `63fffee13`)

**The tool set**
- **How a tool is declared:** each tool is an `#[tool]` method in the `#[tool_router] impl
  LocalApiMcpServerHandler`
  (`runtime/streamlib-api-server/src/mcp.rs:522-954`).
- **Where a tool gets its data:** from two fields, `operations_on_the_loaded_streams: Arc<dyn
  OperationsOnTheStreamsLoadedInThisRuntime>` and `relay_instances` (`mcp.rs:202-209`).
- **Examples:** `list_streams` (`mcp.rs:821-848`) and `list_relays` (`:942-953`). Both answer
  through `serialized_tool_result` (`:1098-1113`).
- **The pinned set:** `CONTROL_TOOL_NAMES` (`mcp.rs:1208-1227`) holds the 16 names. `tools/list`
  must equal it exactly, through `assert_names_exactly_the_control_vocabulary` (`:1246-1256`).
  Two places use it:
  - `tools_list_advertises_exactly_the_control_vocabulary` (`:1585`);
  - the `/mcp/stdio` test (`mcp_stdio_upgrade.rs:372`).
- **A Python copy of the set:** `tests/stream-on-runtime/test_cli_mcp_verb_live.py:40-57`,
  asserted at `:150`.
- **The prose about the tools:** the server instructions describe every tool
  (`LOCAL_API_MCP_SERVER_INSTRUCTIONS`, `mcp.rs:84`).

**Result types and how they are pinned**
- **Where the result types live:** in `runtime/streamlib-runtime-client-contract/src/local_api_wire_contract.rs`,
  for example `ListStreamsToolResult` (`:127-132`) and `ListRelaysToolResult` (`:252-257`).
- **How the wire is pinned:** inline wire-text tests compare the exact JSON and round-trip it
  (`:341-372`, `:375-449`).
- **No recorded fixtures exist:** there are no recorded tool-result fixture files and no xtask
  that records them.
- **The duration convention:** the wire's one duration is spelled `rejoining_after_ms: u64`
  (`:311`).

**`GET /health`**
- **The route:** it answers the text `ok` (`runtime/streamlib-api-server/src/handlers.rs:129-139`).
- **What depends on that body:**
  - the socket tests (`local_api_socket.rs:263-264`, `:322-328`, `:458-462`);
  - the live suite's readiness probe (`tests/stream-on-runtime/conftest.py:456-468`), which
    every live `tatolabd` test waits on.
- **The CLI never calls it.** Its liveness probe is a bare connect
  (`runtime/tatolab-cli/src/machine_runtime_local_api_socket.rs:77-79`).

**The version and the build id**
- **The build id:** `ENGINE_BUILD_ID` is `<crate version>+<git sha>.<per-build nonce>`, with
  the sha `unknown` without a checkout:
  - the constant: `runtime/streamlib-engine/src/core/compiler/compiler_ops/subprocess_bridge.rs:74`;
  - its composition: `engine_build_id_composition.rs:32-40`.
- **Reachable today:** both the api-server and `tatolabd` reach it as
  `streamlib::sdk::helper_process_transport::ENGINE_BUILD_ID`. Nothing outside the helper
  check reads it.
- **The crate version:** the engine, `tatolabd` and `tatolab` all take the workspace version,
  `0.55.1`.
- **The MCP handshake carries a different version.** The MCP `serverInfo.version` is the
  api-server crate's own `1.0.0`. That is `MCP_SERVER_VERSION` (`mcp.rs:81`), set by
  `runtime/streamlib-api-server/Cargo.toml:3`.

**Uptime**
- **No start instant exists:** nothing records when a runtime started.
  - `Runner` keeps no start `Instant`.
  - The own log's file name holds `started_at_millis`, a permitted wall-clock surface, which
    elapsed time must never be derived from.
- **The wall-clock gate won't object:** `Instant` is outside it
  (`xtask/src/check_clock_usage.rs:155-161`, `:685-697`).

**The runtime's own log folder**
- **Where it is set:** `tatolabd` passes `TatolabStateDirectory::runtime_log_directory()`
  (`runtime/streamlib-runtime-client-contract/src/tatolab_state_directory.rs:109-112`) as
  `runtime_own_log_directory` (`runtime/tatolabd/src/machine_stream_hosting.rs:58-61`).
- **Opening it can fail:** the engine opens the log, and a failure leaves it keeping none
  (`runtime.rs:549-554`).
- **What the engine already exposes:** `Runner::runtime_own_log_path()` gives the active
  segment, or `None` (`runtime.rs:697-703`).
- **The path is absolute:** a relative `XDG_STATE_HOME` is skipped
  (`tatolab_state_directory.rs:158-163`).

**How the local API reaches the engine**
- **The entry point:** `serve_the_local_api_for_an_engine(engine: &Arc<Runner>, …)`
  (`runtime/streamlib-api-server/src/local_api_served_for_an_engine.rs:22-45`) casts the engine
  to `Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>`.
- **The trait:** `runtime/streamlib-engine/src/core/runtime/operations_on_the_streams_loaded_in_this_runtime.rs:35`.
  `Runner` and the control plane's test stubs implement it. It carries `this_machines_name`
  and `list_streams`, and nothing about the runtime itself.

**The CLI**
- **`--version`:** it is clap's built-in `version` (`runtime/tatolab-cli/src/main.rs:82-89`). It
  prints `tatolab 0.55.1` and never contacts the runtime.
- **How verbs call tools:** they go through `call_one_local_api_tool`
  (`local_api_mcp_tool_client.rs:361-374`). `streams` is the model:
  `stream_actions_on_the_runtime.rs:352-356` parses `ListStreamsToolResult`.
- **No runtime is a refusal:** it is refused by name and exits 1
  (`machine_runtime_local_api_socket.rs:92-108`, `main.rs:313-320`).

## Decided 1 — a new observation tool serves the description (owner, 2026-10-24)

The plan says `health` "is a REST route, not a tool". It also says the CLI "stays a pure client
of the local API" and is "a pure JSON-RPC client" of the MCP tool set (`:3391-3392`).

- **The tool:** `describe_runtime`, beside `list_streams` and `list_relays`. It takes no
  arguments and no `stream`.
  - The app and the CLI read the one verb.
  - The "exactly" list grows to 17, and `describe_runtime` stays out of
    `TOOLS_THAT_REQUIRE_A_STREAM` (`mcp.rs:1231-1241`).
- **`GET /health` is unchanged.** It stays a liveness probe answering `ok`.
- Rejected: a JSON body on `GET /health` — the CLI, a pure JSON-RPC client, could not read it,
  and every live test's readiness probe reads its `ok` (owner, 2026-10-24).

## Decided 2 — the crate version and the build id, and the handshake's version (owner, 2026-10-24)

- **The version:** the crate version, `0.55.1`.
- **The build id:** `ENGINE_BUILD_ID`.
  - It tells apart two builds of one version: a macOS runtime started before an update still
    runs the old binary.
  - The id is already composed and compiled in, and the helper check already names it in a
    refusal, so serving it reveals nothing new to the owning user.
- **The MCP handshake:** `serverInfo.version` becomes the workspace crate version, replacing the
  api-server crate's `1.0.0`. Any MCP host then shows the version the runtime really is.

## Decided 3 — the result's shape (owner, 2026-10-24)

In `local_api_wire_contract.rs`, beside the other tool results:

```rust
/// The `describe_runtime` tool's result.
pub struct DescribeRuntimeToolResult {
    /// The runtime's crate version, `0.55.1`.
    pub version: String,
    /// `<crate version>+<git sha>.<per-build nonce>`, the build the helper check compares.
    pub build_id: String,
    /// Monotonic milliseconds since this runtime's engine started; a restart resets it.
    pub up_for_ms: u64,
    /// The absolute folder the runtime's own log is written to; `None` when it keeps none.
    pub runtime_log_directory: Option<PathBuf>,
}
```

- **Wire text:**
  `{"version":"0.55.1","build_id":"0.55.1+<sha>.<nonce>","up_for_ms":266400000,"runtime_log_directory":"/home/u/.local/state/tatolab/logs"}`.
- **Why `up_for_ms`:** it matches the wire's one other duration, `rejoining_after_ms`.
- **Why `runtime_log_directory` can be null:** it is the parent of
  `Runner::runtime_own_log_path()`, not the configured directory. A runtime whose log failed to
  open, or a Rust app's `Runner` that keeps none, answers `null` rather than naming a folder
  holding no log. Stream logs stay per project (#2916) and are not this field.
- **Not served:** the machine's name. `graph` already carries it as `machine`.

## Decided 4 — `tatolab --version` shows it (owner, 2026-10-24)

- **The first line:** `tatolab --version` keeps printing its own `tatolab 0.55.1` first.
- **When a runtime answers:** it adds `runtime <version> (<build_id>), up <d>d <h>h <m>m, log
  <runtime_log_directory>`.
- **When none answers:** it adds `runtime: not running` and still exits 0, because a version
  query never fails on a missing runtime.
- **No new verb:** the CLI entry's verb list (`:3456-3468`) is unchanged. `--version` replaces
  clap's built-in with the CLI's own flag.
- **No `--json`:** `tatolab mcp` already gives raw tool results to a script.

## ADDED: the runtime's description, served by the engine

- **On the trait:** `OperationsOnTheStreamsLoadedInThisRuntime` gains `describe_this_runtime()
  -> DescribeRuntimeToolResult`. This extends the one trait the local API reaches the engine
  through. It is not a second handle beside it.
- **On `Runner`:** `Runner` implements it from three sources:
  - a start `Instant` taken when the `Runner` is constructed;
  - `ENGINE_BUILD_ID`, and its `CARGO_PKG_VERSION`;
  - `runtime_own_log_path().parent()`.
- **The test stubs** implement it with fixed values.
- **The tool:** `describe_runtime` is a tool on `LocalApiMcpServerHandler`. It joins
  `CONTROL_TOOL_NAMES`, the Python copy in `test_cli_mcp_verb_live.py`, and the server
  instructions.
- **The handshake:** `MCP_SERVER_VERSION` reads the workspace version. For example, the
  api-server crate takes `version.workspace = true` in place of its own `1.0.0`.
- **The CLI:** `tatolab --version` per Decided 4, parsing `DescribeRuntimeToolResult` as
  `streams` parses `ListStreamsToolResult`.

## MODIFIED: §Control plane & observability — the one control plane (`:3389-3398`)

- **The tool list:** the served MCP tool set names `describe_runtime` among the observation
  verbs.
  - Proposed: "exactly the observation verbs `graph`, `logs` and `describe_runtime` — the
    runtime's version, build id, monotonic uptime and its own log's folder — …".
  - "`health` is a REST route, not a tool" stays, and so does `health`'s `ok`.
- **The handshake:** the entry gains a clause saying the MCP handshake's server version is the
  runtime's crate version.

## MODIFIED: §Control plane & observability — the CLI (`:3456-3468`)

It adds "`--version` prints the CLI's version and, when a runtime answers, the runtime's
description". The verb list is unchanged.

## MODIFIED: §Product — `tatolabd` (`:109-126`)

- "`graph`, `logs`, `mcp`, `relays` and `relay` speak the local API" adds `--version`.
- The state-directory entry (`:127-141`) is unchanged. The served folder is its `logs/`.

## Not modified

- **The wall-clock set (`:2081-2095`):** uptime is an `Instant` elapsed, never a start
  timestamp, and the wall-clock gate is untouched.
- **The Tatolab.app entry (§Distribution):** the app's Settings › Runtime is the app step's GUI,
  which reads this tool.

## Tests

- **Contract crate:** a wire-text test of `DescribeRuntimeToolResult`, both with and without a
  log folder, with a round-trip. This follows `:341-372`.
- **api-server:**
  - `describe_runtime` answers the stub's description;
  - `tools/list` equals the 17 names;
  - the handshake's `serverInfo.version` equals `CARGO_PKG_VERSION`.
- **Engine:**
  - a fresh `Runner`'s `up_for_ms` grows between two reads;
  - `runtime_log_directory` is `None` without an own-log directory, and is the given directory
    with one;
  - `build_id` starts with `version` + `+`.
- **CLI, against the stub runtime:**
  - `--version` prints both lines;
  - with no runtime, it prints `runtime: not running` and exits 0.
- **Live (`tests/stream-on-runtime/`):**
  - `describe_runtime` on a running `tatolabd` answers its version, a `runtime_log_directory`
    under the test state root that holds a `tatolabd-*.jsonl`, and an `up_for_ms` that grows;
  - after a `tatolabd` restart, `up_for_ms` is smaller than before.

**Decided — no recorded fixture; the wire-text test and the live test stand in for it (owner,
2026-10-24).** The ticket asked for "a contract fixture of its result recorded from a running
runtime".
- No recorded-fixture mechanism exists in the tree. The two shipped tool results are pinned this
  way, and adding a recorder for one tool would be a parallel system.
- If tatolab-app needs a JSON file to test its parser against, it copies the wire-text test's
  string.

## Tickets

One tracer bullet: #2922. Needs no rig.

## REMOVED

None. The change only adds: `/health`'s `ok` and every existing tool stay.

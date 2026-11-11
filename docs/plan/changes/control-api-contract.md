# control-api-contract

> **Direction agreed by the owner, 2026-11-10**, in the project thread and in comments on the
> research doc. The owner's words: the MCP tool set stops being the contract, and "we need a way
> of working across different protocols and places", with "a very well defined contract that's
> enforced and versionable and documentable", so SDK docs can be generated for the website. The
> owner agreed to replace §Control plane's "One control plane" entry and to the version policy
> below, asked for an agent surface that is "agent optimized" and still "enables the agent to do
> everything", with progressive disclosure, wants a localhost WebSocket "very quickly after we
> verify it works well for studio", and requires that Studio "always easily get the client
> without relying on a 3rd party service or being online". The owner accepted the author and
> origin record below ("That works").

This change builds on project-action-contract, which made one Rust type per action family the
contract every caller takes. That change settled what an action is. This one settles how the
whole operation set is declared, published, versioned and reached: one registry, one generated
contract file, and every protocol, SDK and doc page a projection of that file.

**Scale gate — this skill plus an ADR.** It changes the local API's public contract, reopens the
local-API decision that no loopback listener carries control, and adds a client package the
runtime unit ships. The ADR is `docs/decisions/control-api-contract.md`, written with the first
ticket.

**Precondition.** §Control plane is IN-FLIGHT under project-action-contract and others; this
change touches the entries listed under MODIFIED and no OPEN entry.

## Current state (recon, `main` `bf01c74`)

- **The MCP tool definitions are the contract.** Each operation is an `#[tool]` method on
  `LocalApiMcpServerHandler`, its argument struct in the same file, across
  `runtime/streamlib-api-server/src/mcp*.rs` (66 tools). The router is the sum of nine
  `#[tool_router]` blocks (`mcp.rs:199`). Adding an operation means writing an MCP tool.
- **The tool list is pinned by hand.** `CONTROL_TOOL_NAMES` (`mcp.rs:1302`) holds every name and
  a test asserts `tools/list` equals it. §Control plane's "One control plane" entry
  (`ARCHITECTURE.md:3976`) lists the tools by name too, and that list has drifted: it names
  `rename_stream`, `list_project_streams`, `expose_port` and the relay verbs, none of which the
  tree serves.
- **The core is mostly protocol-free already.**
  - `StreamGraphAction` (`runtime/streamlib-project-repository/src/stream_graph_action.rs:76`) is
    the graph-edit command shape history records.
  - `OperationsOnTheStreamsLoadedInThisRuntime`
    (`runtime/streamlib-engine/src/core/runtime/operations_on_the_streams_loaded_in_this_runtime.rs:31`)
    is the project and stream service, implemented by `Runner`.
  - `GraphTransactionInFlight` (`runtime/streamlib-api-server/src/mcp_stream_graph_tools.rs:1453`),
    the all-or-nothing executor with history, idempotency and reversal, depends on nothing MCP
    but lives in the MCP tools file, `pub(crate)`.
- **Result types are hand-kept.** `runtime/streamlib-runtime-client-contract/src/local_api_wire_contract.rs`
  holds result structs the CLI parses; nothing generates them for TypeScript or Python.
- **Studio hand-writes its client.** tatolab-app's `apps/tato-app/src/main/local_api_client.ts`
  calls tools by name and parses each result in its own `*_on_the_wire.ts` module. On
  2026-11-10 it still called `run_project` and `rename_stream`, both gone, so Run and stream
  rename are refused by a current runtime. No build step could have caught it.
- **A transaction records only who.** `ProjectTransactionAuthor`
  (`runtime/streamlib-project-repository/src/project_transaction.rs:32`) is a kind and a name. A
  call with no author is recorded as an agent named after its `clientInfo`
  (`runtime/streamlib-api-server/src/mcp_project_asset_tools.rs:727`). Nothing records the
  adapter, client build or contract version a change arrived through.
- **The generated-and-checked pattern exists.** `cargo xtask generate-built-in-node-classes --check`
  (`xtask/src/generate_built_in_node_classes.rs`) fails CI when `_built_in_nodes.py` differs from
  the runtime. The contract file follows the same pattern.

## ADDED

- **The operation registry.** Every control operation is declared once in a new crate,
  `streamlib-control-contract`: its method name, request type, result type, refusal type, a
  one-line doc, an agent usage note, `since` (the contract version it arrived in), stability
  (`stable` or `experimental`), its group (`project`, `stream`, `graph`, `asset`, `file`,
  `dependency`, `node_type`, `view`, `observe`) and its exposure (`studio`, `agent`, `public`).
  Request and result types are the action contract's Rust types, with serde and schemars. The
  project-action-contract types move into this crate rather than being copied.
  [NEEDS DECISION: how an operation is declared]
  - (a) An `impl ControlOperation for AddNode` per operation, with associated `Request`,
    `Result` and constants, collected into one `const` table the core and adapters iterate.
  - (b) A derive macro on the request type that emits the same.
  - (c) One `control_operations!` table macro listing every operation.
  - Recommendation: (a). It is plain Rust, compiles to one table and needs no new proc-macro;
    a missing operation is a compile error in the dispatcher's exhaustive match.
- **The contract file.** `cargo xtask generate-control-contract` writes
  `runtime/streamlib-control-contract/contract.json` from the registry: every request with its
  params and result schemas (JSON Schema 2020-12, which MCP 2026-07-28 accepts), every
  notification with its direction, every refusal, and each entry's `since`, stability and
  docs. `--check` fails CI on any difference, as `generate-built-in-node-classes --check` does.
  [NEEDS DECISION: the file's shape]
  - (a) OpenRPC-shaped methods plus an explicit `notifications` array.
  - (b) LSP-metaModel-shaped: `requests`, `notifications`, `structures`.
  - Recommendation: (a), so existing OpenRPC doc tooling can render most of it.
- **The contract version.** One integer, `contract_version`, bumped only on a breaking change
  (a removed or renamed operation, a removed field, a narrowed type). An addition is marked by
  its `since` and needs no bump. An `experimental` operation may change without a bump before it
  turns `stable` (owner, 2026-11-10). CI compares the committed contract file with `main`'s and
  fails a pull request whose breaking difference does not bump the integer.
- **The discover call.** `discover` answers `contract_version`, the runtime's version and
  build id, and the operations it serves. Every adapter serves it. A client built against
  another `contract_version` is refused at connect with a message naming both numbers.
- **One dispatcher.** The core runs every operation through
  `ControlOperationDispatcher::dispatch(request, request_origin)`. `GraphTransactionInFlight`
  moves out of `mcp_stream_graph_tools.rs` into the dispatcher's crate and is renamed for its
  role. Adapters parse, call `dispatch`, and render the result; none holds operation logic.
- **The JSON-RPC adapter.** Every operation is a JSON-RPC 2.0 method of its registry name on the
  owner-only socket, at `/control` (plain HTTP request/response) and `/control/stream`
  (an upgraded, newline-delimited connection for subscriptions). Notifications carry the
  change feed, stream state and graph changes to subscribers. This is Studio's, the CLI's and
  every SDK's door.
- **Generated clients, shipped with the runtime.** The build generates a TypeScript package
  (`@tatolab/runtime-client`: `.d.ts` types with each doc line as TSDoc, a client, optional
  response validators, `contract.json` and a generated `REFERENCE.md`) and a Python module in
  the `tatolab-stream` wheel. The runtime unit carries the TypeScript package, so Studio builds
  against the copy in the unit it bundles, with no registry and no network (owner,
  2026-11-10). The Rust client re-exports the registry's own types.
- **Generated reference docs.** The same generator writes the website's SDK reference from
  `contract.json`: one page per group, one entry per operation with its doc, inputs, result,
  refusals, `since` and stability.
- **The request origin.** Every recorded transaction gains `origin`, which the runtime fills
  and a client cannot set: `adapter` (`jsonrpc`, `mcp`, `websocket`, `cli`), `client` (name and
  version), `contract_version` and `connection`. `ProjectTransactionAuthor` gains an optional
  `account`, set only from a verified signed-in session, for multiplayer; a signed-out local
  user is told apart by a per-install id. Who made a change and how it arrived stay separate
  (owner, 2026-11-10).
- **The agent surface.** The MCP adapter is generated from the registry's `agent` exposure. It
  lists a small core set (`describe_runtime`, `list_streams`, `graph`, `logs`, `transact`,
  `start_project`, `stop_project`, `undo`) and three discovery tools that reach every other
  operation: `list_operation_groups`, `describe_operations` and `call_operation`. Each tool's
  description is the registry's agent usage note, with an `outputSchema` and read-only or
  destructive annotations. Resources and prompts stay, hand-written prompts beside generated
  tools. Every operation stays reachable; nothing loads 66 schemas up front (owner, 2026-11-10,
  "agent optimized" yet able "to do everything"; the two-tier shape is session, 2026-11-10).
- **Agent evals.** A suite of scripted agent tasks runs against a test runtime over the MCP
  adapter and grades the final graph, recording success rate, tool calls and tokens per run.
- **A localhost WebSocket adapter, after Studio.** The JSON-RPC adapter served over a WebSocket
  on loopback for tools that cannot open a Unix socket, such as OBS or a browser-based tool
  (owner, 2026-11-10). It lands only after Studio runs on the JSON-RPC adapter.
  [NEEDS DECISION: how the WebSocket is gated]
  - (a) A random token the runtime writes to an owner-only file in the runtime directory, sent
    on connect, plus an `Origin` allow-list against DNS rebinding.
  - (b) Off by default; the user turns it on in Studio, which shows the token to paste.
  - Recommendation: (a) and (b) together: off by default, token-gated when on.

## MODIFIED

- §Control plane, "One control plane" (`ARCHITECTURE.md:3976`): the operation registry is the
  canonical control vocabulary, and the contract file generated from it is the published
  contract. JSON-RPC, MCP and, later, the WebSocket are adapters over one dispatcher. The entry
  names no tools; the tool set is what `contract.json` lists (owner, 2026-11-10).
- §Control plane, the local API is reachable only on its own machine (`ARCHITECTURE.md:4095`) and
  the auth posture (`:4090`): control stays on the owner-only socket by default; the loopback
  WebSocket is the one listener that may also carry it, gated as decided above.
- §Control plane, the MCP host entry (`ARCHITECTURE.md:4108`): `tatolab mcp` and `/mcp/stdio`
  stay; the tools behind them are the generated agent surface.
- §Control plane, the CLI (`ARCHITECTURE.md:4052`): every verb is a call through the Rust client
  on the JSON-RPC adapter.
- The transaction record: `origin` and `author.account` join the envelope project-action-contract
  defined, under its `format_version`.

## REMOVED

- REMOVED: CONTROL_TOOL_NAMES

  The hand-written argument structs in `runtime/streamlib-api-server/src/mcp*.rs` also go, as
  each operation moves into the registry. They have no one spelling to grep; the dispatcher's
  exhaustive match and the contract file's `--check` prove the move.

## Order

1. Registry and dispatcher for a handful of operations (`describe_runtime`, `list_streams`,
   `graph`, `start_project`, `add_node`, `connect`, `transact`), the contract file and its
   `--check`, the JSON-RPC adapter, and the generated TypeScript client in the runtime unit.
2. Every remaining operation moves into the registry; the MCP adapter is generated from it;
   `discover` and the contract-version gate.
3. `origin` and `author.account` on the transaction record.
4. Generated reference docs and the Python client.
5. Agent evals.
6. The loopback WebSocket adapter.

[NEEDS DECISION: where this sits in `ROADMAP.md`. Step 1 unblocks Studio's breakages; the
owner sets the order.]

## Consumers

Tato Studio (tatolab-app) moves from its hand-written client to the generated one in the runtime
unit, which also fixes its `run_project` and `rename_stream` calls. That is backlog at
tatolab-app and never blocks this change.

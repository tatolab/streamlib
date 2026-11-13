# control-client-sdk-and-push

> **Direction from the owner, 2026-11-12**, in the project thread, after Studio moved onto the
> generated client (tatolab-app#334). The owner's words: the client should be "an actual
> client", not "just types" — one that, "even if the adapter is external (e.g. like someone
> connects to mcp or websocket or whatever on their own)", still handles "the actual message
> creation and validation so I can do things like `client.open_project()`". The owner wants
> Studio off the HTTP and MCP adapters, on a WebSocket: "for studio over socket and for other
> tools over the loop back". Studio's graph view polls every 2.5 s, "very very slow updating
> the ui… massive lag"; the owner asked that this upgrade remove the poll in favour of push.
> The owner asked to see this design before any code is written.

This change builds on control-api-contract, which made one registry, one dispatcher and one
generated contract file. That change shipped its first two Order steps. This one makes the
generated TypeScript package a real client, pulls control-api-contract's loopback WebSocket
forward and puts it on the owner-only socket too, and adds the push Studio needs to stop polling
the graph.

**Scale gate — this skill plus an ADR amendment.** It changes the client package's public API,
the framing of the stream connection, and the local API's listener posture. The ADR
`docs/decisions/control-api-contract.md` gains a section with the first ticket.

**Precondition.** §Control plane & observability is IN-FLIGHT under control-api-contract and
playback-controls. This change touches the entries listed under MODIFIED and no OPEN entry.

## Current state (recon, `main` `4f65318`)

- **The client is a typed transport.** `sdk/tatolab-runtime-client/` holds a generated
  `control_contract.d.ts` (every params and result type, and `ControlOperations`, a map from
  method to both) and a hand-written `runtime_client.js`: one generic
  `call(method, params, { signal, resultReviver })`, `callOneControlOperation`,
  `onNotification` and `ControlOperationError`. `xtask/src/generate_runtime_client.rs`
  writes the types. Nothing validates a request or an answer at run time, so a caller that
  reads a result must trust the types or re-check every field. Studio re-checks: about 2,500
  lines of readers under `apps/tato-app/src/main/*_on_the_wire.ts` take each result as
  `unknown`.
- **OpenRPC's own generator does not fit.** `@open-rpc/generator` 2.1.1 (Oct 2025) runs on our
  `contract.json` (which passes the OpenRPC meta-schema). It writes one function per method, but
  names 885 types by hash, validates params only (ajv 6), never validates answers, reaches only
  http/websocket/postMessage by host and port, and ignores `x-notifications`.
  `@open-rpc/client-js` 2.1.0 (Aug 2026) has a pluggable `Transport` class and a
  newline-delimited `StreamTransport`, but no validation and no reviver, so a `u64` past 2^53
  loses digits. zod 4's `z.fromJSONSchema` converts all 161 component schemas, but only at run
  time, with no static types, and zod calls it semi-experimental.
- **The stream connection is not a WebSocket.** `/control/stream`
  (`runtime/streamlib-api-server/src/control_json_rpc_adapter.rs:67`) upgrades to the
  `control-jsonrpc` token with a raw `hyper::upgrade::on` (`:126-180`) and frames
  newline-delimited JSON both ways (`:225-333`). axum's `ws` feature is already on
  (`runtime/streamlib-api-server/Cargo.toml:59`), and a WebSocket upgrades over the socket today
  (`local_api_socket.rs:271-286`, the test `the_router_upgrades_a_websocket_over_the_socket`).
- **One unscoped WebSocket exists.** `GET /ws/events` (`handlers.rs:121`, `:368-439`) sends every
  `PUBSUB` event on `topics::ALL` as JSON text, with no contract, no version and no scoping.
  Nothing in the tree or in Studio calls it.
- **The loopback listener carries no control.** `loopback_http_listener.rs:24-58` serves only
  the lock holder's pid, with a Host check; every other route 404s there (`:236-250`).
- **Three change sources, one notification.**
  - The project feed is a `tokio::broadcast`
    (`runtime/streamlib-project-repository/src/project_repositories_open_in_this_runtime.rs:45`).
    It feeds both `project_changed` (`control_json_rpc_adapter.rs:338-409`) and the MCP
    transactions resource (`mcp_project_change_feed.rs:238-279`).
  - Stream actions are a second `tokio::broadcast` of action ids
    (`project_stream_action_queue.rs:232-271`). Only the MCP resource
    `streamlib://stream-actions` (`mcp_stream_action_resources.rs:82-130`) forwards it.
  - The engine's `PUBSUB` (`runtime/streamlib-engine/src/core/pubsub/bus.rs:13`) carries a
    loaded stream's lifecycle (`RuntimeStarted`, `RuntimeStopped`, `RuntimePaused`…,
    `loaded_stream.rs:577-1747`) and its graph edits (`GraphDidChange` after each processor or
    link change, `operations_runtime.rs:117-506`). No adapter forwards it but `/ws/events`.
  - **A node's state change is never published.** `RuntimeEvent::ProcessorStateDidChange`
    (`events.rs:290`) is declared and has no publish site;
    `StateComponent::transition_to_only_from` (`graph/components/state_component.rs:237`)
    publishes nothing.
- **Studio polls.** In tatolab-app: the graph every 2.5 s
  (`STREAM_GRAPH_POLL_MILLISECONDS`, `src/renderer/view_host/stream_graph_and_catalog_readings.ts:9`),
  the log every 1.5 s, the project listing every 5 s, the status line (`list_streams`) every
  2 s, a deletion every 250 ms, and the runtime's description every 15 s. Studio still opens
  `/mcp/stdio` for the two feeds above.

## ADDED

- **A generated client, one method per operation.** `cargo xtask generate-runtime-client`
  also writes `control_schemas.js` and `control_client.js` with their `.d.ts`. The client
  looks like this:

      const client = await TatolabRuntimeClient.connect({ socketPath, clientName });
      const queued = await client.open_project({ project_directory });
      const stop = client.on_project_changed((change) => …);

  - For every method, the generator writes a method that checks its params against the
    method's schema before anything is sent. A request that does not match rejects locally
    with `ControlRequestInvalid`, naming the path and the field, and is never sent.
  - Each method builds the JSON-RPC request itself.
  - Each method parses the answer against the result schema and resolves the typed result.
    Otherwise it rejects with one of three errors:
    - `ControlOperationError`: the runtime refused or failed, in its own words, with the
      structured `refusal` when one is given;
    - `ControlAnswerInvalid`: the answer does not match the contract, naming what does not;
    - a transport error.
  - Every notification in `x-notifications` gets an `on_<name>(listener)` that validates
    each notification before the listener sees it.
- **The schemas are zod, generated as source.** The generator writes one named zod schema per
  component schema (`OpenProjectParams`, `QueuedStreamActionToolResult`…), plus one params
  and one result schema per method. Every exported TypeScript type is `z.infer` of its
  schema, so a type and its validator cannot drift. The generator reads the same JSON Schemas
  `control_contract.d.ts` is written from. `--check` holds the generated files to the
  contract, as it does today. The package depends on `zod` 4, pinned exactly; Studio already
  ships zod 4.6.7.
- **Message building and validation without I/O.** For a caller that brings its own
  connection, such as an MCP host, a browser or a test, the package exports:
  - `controlRequestMessage(method, params, id)`, a validated request;
  - `controlAnswerResult(method, answerMessage)`, the validated result or the typed error;
  - `controlNotificationParams(message)`, a validated notification;
  - every schema, under `ControlSchemas`.
- **A transport is one small interface.** `ControlTransport` sends one JSON-RPC message and
  resolves its answer. Where the transport can, it also hands each notification it receives
  to a listener and reports its close. `new TatolabRuntimeClient(transport)` takes any
  transport. The package ships two: `ControlWebSocketTransport`, its main one, and
  `ControlPostTransport`, one `POST /control` per call, with no notifications. A third party
  writes its own, and an `@open-rpc/client-js` `Transport` fits behind it in a few lines.
- **Integers keep their digits.** Every transport parses answers with a reviver that reads an
  integer past `Number.MAX_SAFE_INTEGER` as a `bigint`. A field whose schema is
  `format: uint64` or `int64` validates as `number | bigint`. A free-form field (a log
  record's fields, a node's config) keeps a large integer as a `bigint`. This replaces the
  `resultReviver` call option.
- **One WebSocket control adapter, on two listeners.** The JSON-RPC adapter is served as a
  WebSocket at `/control/ws`: one JSON-RPC message per text frame, both ways, carrying calls,
  answers and notifications. It is the same dispatcher and the same contract-version gate as
  `/control`.
  - **On the owner-only socket**, for Studio and the CLI. It needs no token: the socket's file
    permission is the gate, as for every route there.
  - **On a loopback TCP port**, for tools that cannot open a Unix socket, such as OBS or a
    browser page. This is control-api-contract's loopback WebSocket, pulled forward. It is
    always on and fully open: no token, and no check of the `Host` or `Origin` a request gives,
    so a headless runtime needs no setup (owner, 2026-11-12). A gate is a dedicated design of
    its own, layered on later.
    - The loopback listener otherwise serves only what `loopback_http_listener.rs` serves today.
    - Rejected: off by default behind a token Studio shows and an `Origin` allow-list — a
      runtime run headless has no Studio to turn it on, and the owner will design the gate
      whole rather than half now (owner, 2026-11-12).
- **Push for a loaded stream's live graph.** The contract gains
  `subscribe_stream_graph_changes(streams)` and the notification `stream_graph_changed`
  (`{ stream, change }`, where `change` is `edited`, `node_state_changed`,
  `playback_state_changed`, `drops_raises_or_link_states_changed` or `missed`, with the node and
  its new state where one applies).
  - The adapter reads it from `PUBSUB`, the engine's one event system, on each subscribed
    stream's topic: `GraphDidChange`, the playback events, and `ProcessorStateDidChange`.
  - The engine starts publishing `ProcessorStateDidChange` from `ObservableProcessorState`
    (`state_component.rs`), which holds every write of a processor's state: `transition_to`,
    `transition_to_only_from`, `transition_to_unless_already_failed` and
    `fail_with_reason_and_capture_device`, the last covering every failure. `StateComponent`
    only forwards to it. No second channel.
  - A full queue drops an event in `PUBSUB` (`bus.rs:116`). The adapter then sends
    `stream_graph_changed` with `change: "missed"`, and the client reads the graph again, as
    `project_changed` does for a lagged feed. `PUBSUB` hands a listener the count of events it
    lost at its next delivery, an extension of the one event system (it counts each drop per
    subscription today and tells the listener nothing).
  - A subscription names stream ids. A stream loaded again gets a new stream tag and so a new
    topic (`runtime.rs:3830`); the adapter moves the subscription to the new topic and sends
    `change: "missed"`, so the client reads the graph again.
  - A node's dropped and refused bag counts, the raise its `process()` keeps, and a link's
    state change without a `PUBSUB` event: they are counted live, some in a helper process's
    board, on paths that run every frame. They reach a subscriber all the same, so Studio drops
    its slow `graph` read (owner, 2026-11-12). Each connection samples them from each followed
    stream's graph every 500 ms and sends `drops_raises_or_link_states_changed` for a stream
    whose sample differs from the one before, and on its first sample, so a change made between
    the client's graph read and that sample still reaches it. A written count, which grows with
    every bag and which Studio does not show, is not sampled. The kind's name and the 500 ms are the implementer's (session, 2026-11-12).
- **Push for stream actions.** The contract gains `subscribe_stream_action_changes` and
  `stream_action_changed` (`{ action }`), read from the stream-action queue's broadcast
  (`project_stream_action_queue.rs:232`), so a client follows an action without the MCP
  resource. That broadcast reports a lag but not which ids it skipped, so on a lag
  `stream_action_changed` carries `missed` and the client calls `describe_stream_action` for
  each action it follows.
- **Push for a project's new log records.** The contract gains
  `subscribe_project_log_records(project_directories)` and the notification
  `project_log_records_written`, so Studio's log view follows a project's log without its 1.5 s
  `logs` read (owner, 2026-11-12). The notification carries the records themselves, as the
  engines that run a server and a client apart do for an append-only log — Godot's remote
  debugger, Unity's PlayerConnection, Chrome DevTools' `Log.entryAdded`, `docker logs -f`
  (owner, 2026-11-13). The engine's log drain worker announces each project whose log it handed
  new records to the OS, once per flush, at most every 100 ms, on one process-wide broadcast.
  Each connection holds a cursor for each project it subscribed to, starting at the log's end,
  reads on from it on each announcement, and sends
  `{ project_directory, records, after, next_after, records_no_longer_held }`, at most 500
  records to a notification, a longer run as several; a connection that falls behind the
  broadcast reads every subscribed project on from its cursor, so nothing is missed. A client
  whose own cursor is not a notification's `after` reads `logs` from its cursor `until` that
  notification's `next_after`, and `logs` gains `until` for it. Projects are matched with
  symlinks resolved and named by the directory the connection spelled. The names, the 500 and
  the cursor-per-connection shape are the implementer's (session, 2026-11-13).
- **Push for files changed outside the runtime.** `project_changed` gains the change
  `files_changed`, with `files` naming each changed file relative to the project, so Studio
  learns of an editor's save without its 5 s project listing read. The runtime watches a
  project's folder with the OS's file watch (the `notify` crate) only while some connection
  subscribes to that project, skips what its history skips, and gathers a burst over 100 ms
  (owner, 2026-11-12). A missing project folder subscribed to by `subscribe_project_log_records`
  is refused. The field names and the 100 ms are the implementer's (session, 2026-11-12).
- **The change feed as a control operation.** `read_project_changes { project_directory }`
  answers a project's change feed as its MCP resource renders it, opening the project as
  `list_assets` does. A client reads it once, and again after `missed`, and `project_changed`
  carries each change after that. Studio reads its undo history this way, so no first-party
  client opens `/mcp/stdio`, which serves agents (owner, 2026-11-12).

## MODIFIED

- **The JSON-RPC adapter** (control-api-contract, ADDED): `/control/stream`'s
  newline-delimited framing is replaced by `/control/ws`. `POST /control` stays, for one-shot
  callers and scripts. The adapter entry's promise of "stream state and graph changes" is met
  by `stream_graph_changed`.
- **Generated clients, shipped with the runtime** (control-api-contract, ADDED): "a client,
  optional response validators" becomes a generated client that validates every request and
  every answer by default. The Python client of control-api-contract's Order step 4 follows the
  same shape later, with its own schema library. That is out of scope here.
- **A localhost WebSocket adapter, after Studio** (control-api-contract, ADDED, Order step 6):
  it is the loopback listener of the WebSocket control adapter above, built in this change,
  with no gate (owner, 2026-11-12).
- **§Control plane, the local API is reachable only on its own machine**
  (`ARCHITECTURE.md:4546`): the loopback listener also serves `/control/ws`, always and with no
  gate. Control otherwise stays on the socket.
- **§Control plane, the auth posture** (`ARCHITECTURE.md:4541`): the socket stays tokenless,
  and so does the loopback WebSocket, until the owner's dedicated design of its gate.
- **§Control plane, the MCP host entry** (`ARCHITECTURE.md:4559`): `/mcp/stdio` stays for
  agents. Its resources stay; no first-party client depends on them.

## REMOVED

- REMOVED: control-jsonrpc
- REMOVED: CONTROL_JSON_RPC_STREAM_UPGRADE_PROTOCOL_TOKEN
- REMOVED: serve_control_json_rpc_over_the_upgraded_stream
- REMOVED: resultReviver
- REMOVED: handle_websocket
- REMOVED: /ws/events

The last two remove the unscoped `PUBSUB` firehose. Every push goes through the contract, so it
is versioned and validated.

## Consumers

Tato Studio (tatolab-app) moves onto the generated client in one app change. That work is
tracked at tatolab-app and never blocks this change. The app change:

- opens one `ControlWebSocketTransport` on the socket for every call, viewer and subscription;
- deletes its readers (`*_on_the_wire.ts`, `tatolab_runtime_control_operation_call.ts`) and its
  `/mcp/stdio` connection;
- subscribes to `project_changed` for edits and the transactions feed, to
  `stream_graph_changed` for live state, and to `stream_action_changed` for actions;
- deletes `STREAM_GRAPH_POLL_MILLISECONDS` and re-reads the graph only on a notification.

The other polls (log, listing, status line, runtime description) are named under Out of scope.

## Order

1. The generated client: schemas, methods, `on_<notification>`, the message builders, and
   both transports over the existing adapter, with `ControlWebSocketTransport` arriving with
   step 2. Studio can adopt it over `POST /control` at once.
2. `/control/ws` on the socket. `/control/stream` is removed in the same PR, with no
   compatibility route (pre-1.0, no shims). Its callers are the generated client, which moves
   to `/control/ws` in that PR, and Studio's `RuntimeViewerControlConnection`, which stays on its
   pinned streamlib commit until the app change moves it.
3. `stream_graph_changed` with `ProcessorStateDidChange` published, and `stream_action_changed`.
   Studio drops the graph poll and `/mcp/stdio` here.
4. The loopback listener for `/control/ws`, always on and with no gate.

Steps 1 to 3 are what removes Studio's lag. Step 4 is for outside tools. In `ROADMAP.md` this
sits under step 6, "One control API contract", as its next step.

## Out of scope

- Push for the log tail, the project listing and the status line. Each is one more
  notification in the same shape, added when Studio's next poll is the one that hurts.
- The Python client.
- Server-to-client requests. JSON-RPC over the WebSocket allows the runtime to call the client,
  and nothing here needs it.

## Decided (owner, 2026-11-12)

The owner chose every recommended option, in the project thread.

1. `/ws/events` is removed; the contract's notifications replace it.
2. The package validates with zod 4.
3. Studio uses the WebSocket on the socket for every call, viewer and subscription.
4. `PUBSUB` tells a listener how many events it lost.
5. A graph subscription follows its stream across a reload.
6. A missed stream action is recovered by `describe_stream_action`.

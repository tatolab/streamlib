# local-api

> **Approved by the owner, 2026-10-01**, as written, with its stated assumptions. Tickets
> derived 2026-10-01 (below).
>
> **Amended 2026-10-04** by the moq-on-the-tailnet pivot (`docs/decisions/moq-on-the-tailnet.md`):
> Zenoh is removed before the rest of this change is built. S2, the announcement (#2578), has
> nothing left to edit — no description, peer table or mesh-peers listing exists once the mesh is
> gone — and `nodes` lists the local registry alone. `tap` and `exchange` are carried as written;
> the sharing step deletes them.

Step 2 of the one-runtime-per-machine pivot: control leaves the network, and MCP hosts reach it by
launching a command. After this change:
- a runtime that hosts its local API serves today's router — REST, both WebSockets, the OpenAPI
  document, `POST /mcp` — on one Unix socket in its runtime directory, and on nothing else;
- the TCP listener, its all-interfaces default, the port walk from 9000, `--host`, `--port`,
  `--url`, the registry's `control_url` and the bearer gate are gone, with no TCP path kept beside
  the socket;
- a runtime stops announcing control URLs on the mesh, and `nodes` stops printing them;
- the runtime speaks MCP 2026-07-28 and nothing earlier;
- an MCP host is configured as `claude mcp add streamlib -- streamlib mcp`, and `ssh <machine>
  streamlib mcp` reaches another machine's runtime.

This file replaces the on-hold `one-runtime-per-machine-ripout.md`; its two `[NEEDS DECISION]`
blocks were settled by `/align` on 2026-10-01 (PR #2570): the router over the socket, and the
`mcp` verb.

**Scale gate — this skill, plus an ADR (`docs/decisions/local-api.md`, already written).** The
Python API's public contract moves (`Runtime.host_control_plane` loses `bind_host` and
`bind_port`; the CLI loses `--host`, `--port`, `--url` and gains `mcp`); the wire moves (the mesh
description loses a field; the MCP revision changes); the control transport moves.

**Precondition.** Every entry this delta builds is DECIDED: §Control plane & observability
`ARCHITECTURE.md:4438-4442` (auth: whoever can open the socket), `:4443-4451` (reachable only on
its machine), `:4452-4463` (the `mcp` verb; 2026-07-28 only), `:4247-4296` (one vocabulary, its
`/mcp` clause superseded), `:4310-4326` (the CLI, `mcp` joining); §Networking `:3528-3545` and
`:3889-3910` (the announcement and `graph`'s peers, whose `control_plane_urls` clauses this
removes). Not built against: the rest of the local API `:4464-4468` (stream verbs, machine or
user — §Product `:114-125`), and the URL forms' listener, whose grammar and forms are §Networking's
OPEN `:4023-4034` and step 8's. This change serves today's runtimes, one per `run`, each with its
own socket; the one-per-machine runtime inherits the mechanism. Since approval, the stream verbs
and one runtime per machine owned by one user were DECIDED (#2580); runtime hosting builds them.

**Verified against the tree 2026-10-01 (HEAD 21d3cc51f).** Two read-only sweeps: the control
plane (no control-plane code changed since the on-hold recon at 30fbef3; anchors corrected below)
and the MCP 2026-07-28 specification, fetched live.

**The listener and its config**
- `runtime/streamlib-api-server/processors/api_server.rs:127-153` walks ten ports from the
  requested one, binding `tokio::net::TcpListener` on `format!("{}:{}", host, port)` (`:129-130`),
  failing with "Could not find available port in range" (`:150`); it records the requested port,
  never `local_addr()`. `control_url` is `http://127.0.0.1:{port}` (`:165`); `axum::serve` at
  `:195`; `hosted_control_plane` is recorded at `:175-176` and cleared at `:209-210`.
- `ApiServerConfig { host, port, log_path, require_auth }` (`src/api_server_config.rs:13-30`);
  `ApiServerControlPlaneHostConfig { bind_host, bind_port }` (`src/control_plane_host.rs:13-18`).
- Defaults `0.0.0.0` / `9000`: `cli.py:71-72` (used `:914`, `:923`),
  `src/python_runtime_lifecycle.rs:475`, `_engine.pyi:650-658`; `--control-plane-port 9000` in
  the rigs (`codec_roundtrip_rig.rs:836`, `cross_runtime_link_rig.rs:79`), six engine fixtures and
  the two packages' live nodes; `CONTROL_PORT` falls back to 9000 in `audio_capture_node.py:40`,
  `audio_loopback_node.py:107`.

**The router, the client, the registry**
- `build_router` (`src/handlers.rs:80`): `/health`, `/api/graph`, `/api/registry`,
  `/api/runtime/shutdown`, `/api/surfaces/{surface_id}/image`, `/ws/tap/{channel}`, `/ws/events`,
  `/api/openapi.json`, `/mcp`. Nothing reads `ConnectInfo`, the peer address or `Host`. axum 0.8.9
  implements `Listener` for `tokio::net::UnixListener` and serves upgrades on any listener; no
  first-party code uses tokio's `UnixListener` yet.
- The CLI's `graph`, `tap` and `logs` are MCP `tools/call` POSTs (`cli.py:573-582` →
  `_control_plane_client.py:94-120`, stdlib `urllib`, which has no `AF_UNIX` and refuses non-http
  URLs at `:72-80`); `exchange` is the REST PNG GET (`:366-403`); the channel form composes both.
  Nothing calls either WebSocket.
- `NodeRegistryEntry { schema_version, runtime_id, runtime_name, control_url, pid, hint }` at
  schema 2 (`src/node_registry.rs:28-46`); Python reads it strictly (`_node_registry.py:35-158`);
  liveness is a `graph` call within 1.5 s, pruned only when the pid is gone too (`:205-206`).
- The precedent socket: `<runtime dir>/surface-share-{runtime_id}.sock`
  (`streamlib_runtime_directory.rs:63-64`), in the 0700 directory; a live duplicate is found by a
  connect probe and refused, a stale file removed (`runtime.rs:1583-1619`). The file gets no mode
  of its own.

**MCP today** (`src/mcp.rs`)
- `POST /mcp` only, one JSON body back, `202` for an id-less message (`:191-205`); every error
  answers HTTP 200. `MCP_PROTOCOL_VERSION = "2025-06-18"` (`:69`). Dispatch (`:241-252`):
  `initialize` (`:242`, `initialize_result` `:255-266`), `ping`, `tools/list`, `tools/call`,
  `resources/list`, `resources/templates/list`, `resources/read`, `prompts/list`, `prompts/get`.
  No `server/discover`, no `subscriptions/listen`, no header checks, no `_meta` read; `RpcError`
  has no `data` (`:153-156`); a missing resource is `-32002` (`:180-188`), which 2026-07-28 forbids.
- 2026-07-28, as fetched: no `initialize`/`ping`; every request carries
  `_meta["io.modelcontextprotocol/protocolVersion"]` and `…/clientCapabilities` (missing →
  `-32602`); `server/discover` is mandatory; every result carries `resultType: "complete"`;
  discover and the list/read results carry required `ttlMs` and `cacheScope`; an unsupported
  version is `-32022` with `data {supported, requested}`. Over HTTP, `MCP-Protocol-Version`,
  `Mcp-Method` and (for `tools/call`, `resources/read`, `prompts/get`) `Mcp-Name` are required and
  must match the body (`-32020`, 400); unknown method is 404. stdio is newline-delimited JSON,
  requests may interleave, `notifications/cancelled` cancels, stdin closing is the shutdown
  signal, and "custom transports that run over a reliable bidirectional byte stream (e.g. Unix
  domain sockets) SHOULD reuse the stdio framing".
- Bearer gating (`src/auth.rs`, `handlers.rs:94-132`) is never switched on by a shipped path;
  `README.md:206` tells users `claude mcp add --transport http streamlib http://127.0.0.1:9000/mcp`.

**The mesh**
- `HostedControlPlaneEndpointRegistry` (`mesh/hosted_control_plane_endpoint.rs:21`) derives
  `control_plane_urls` from `getifaddrs` (`:51-162`); the description's `control_plane_urls`
  (`runtime_mesh_description.rs:42`, no serde default) is wire; it is rendered by
  `json_schema.rs:184`, the peer table (`runtime_mesh_peer_table.rs:117`, `:148`), the observe-only
  session (`python_runtime_mesh_observation.rs:27-74`) and `nodes` (`cli.py:530-540`), and
  threaded through `runtime.rs` (`:37`, `:151`, `:336-342`, `:419`, `:700`), `runtime_context.rs`
  (`:13`, `:59`, `:86`, `:105`, `:165-166`, `:257`, `:283`, `:872-873`), `mesh/mod.rs:22`, `:55`
  and `runtime_mesh_membership.rs` (`:33`, `:138`, `:152`, `:561`, `:589`, `:629-643`).

---

## ADDED: §Control plane & observability — the socket

- **The listener.** A runtime hosting its local API serves `build_router` on a
  `tokio::net::UnixListener` at `<runtime dir>/local-api-<runtime_id>.sock`, beside the surface
  socket, chmod 0600 after bind inside the 0700 directory. A live duplicate is refused by the
  surface socket's connect probe, naming the path; a stale file is removed. The listener is the
  only one: `ApiServerConfig` loses `host`, `port` and `require_auth`;
  `ApiServerControlPlaneHostConfig` loses `bind_host` and `bind_port`;
  `Runtime.host_control_plane()` takes no arguments; the rigs and fixtures follow.
- **The registry.** The entry's `control_url` becomes `local_api_socket_path`, schema 3; a
  reader refuses 2 by name (entries are per run; nothing to migrate). Liveness is the same `graph`
  round trip over the socket; pruning still needs the pid gone.
- **The client.** `_control_plane_client.py` makes every call over the socket with
  `http.client.HTTPConnection` whose connect dials `AF_UNIX` — stdlib, no new dependency: tool
  calls, the PNG GET, the probe.
- **The selector.** `--node <runtime name or id>` is the only way to pick a runtime for `graph`,
  `tap`, `exchange`, `logs` and `mcp`; with none, the sole live runtime, as today. `nodes` prints
  `LOCAL_API_SOCKET` where it printed `CONTROL_URL`.
- **The gate.** File permission is the whole gate (auth entry `:4438`): `auth.rs`, the token
  file and `STREAMLIB_MCP_TOKEN` are deleted. A socket no browser can dial needs no `Origin`
  check, so none is added.

## ADDED: §Control plane & observability — MCP 2026-07-28, and the `mcp` verb

- **The revision.** `mcp.rs` serves 2026-07-28 only: `initialize` and `ping` leave the dispatch;
  `server/discover` joins, answering `supportedVersions: ["2026-07-28"]`, today's capabilities and
  instructions, `serverInfo` in `_meta`; every request's `_meta` is checked — a missing version or
  capabilities is `-32602`, any other version `-32022` with `data {supported, requested}` (so a
  legacy `initialize` is refused naming 2026-07-28); every result carries `resultType:
  "complete"`; the list and read results carry `ttlMs` and `cacheScope: "public"`; a missing
  resource is `-32602`. `subscriptions/listen` answers its acknowledgement with an empty set — no
  list ever changes under a host, since `listChanged` stays `false` — and holds until cancelled.
  `RpcError` gains `data`.
- **Two framings, one dispatch.** `POST /mcp` stays Streamable HTTP for the CLI and tests, now
  with the header checks (`MCP-Protocol-Version`, `Mcp-Method`, `Mcp-Name` against the body,
  `-32020`) and the spec's statuses (400, 404 for an unknown method, 405 for GET). Beside it the
  router serves `/mcp/stdio`: an HTTP/1.1 `Upgrade: mcp-stdio` after which the connection carries
  MCP's stdio framing both ways — the spec's framing for a byte-stream socket. Requests on it run
  concurrently; responses are written as they complete; `notifications/cancelled` drops the
  request's answer; the stream closing ends every request it carried.
- **The verb.** `streamlib mcp [--node …]` resolves the runtime, opens the socket, sends the one
  upgrade request, and then copies bytes: stdin → socket, socket → stdout. It parses no message.
  Stdin closing half-closes the socket and the verb exits when the runtime closes its side; the
  runtime going away exits the verb non-zero with one stderr line naming the runtime. No runtime
  live at launch is a stderr refusal naming `streamlib nodes`, exit 1. The verb joins the CLI's
  subcommands; `test_the_wheel_serves_no_mcp_verb` is replaced by a test that a scripted
  stdio exchange through the verb reaches a running node's tools.

## MODIFIED: §Networking `:3528-3545`, `:3889-3910` — the announcement

- The description drops `control_plane_urls`; so do `graph.mesh.peers[]`, the observe-only
  session, `_engine.pyi:2270` and the `nodes` peers table (`RUNTIME_NAME HOST ENGINE_VERSION`).
- `hosted_control_plane_endpoint.rs`, `HostedControlPlaneEndpointRegistry` and their threading
  through `Runner`, `RuntimeContext` and the membership are deleted.
- `graph` still lists peers. No runtime is drivable from another machine through its local API.
  The description is wire with no serde default; pre-1.0 there is no cross-version wire, so no shim.

## MODIFIED: §Control plane & observability `:4310-4326` — the CLI, the rigs, the docs

- **The CLI.** `run` and `dev` lose `--host` and `-p/--port` (`cli.py:911-926`, `:285`);
  `graph`, `tap`, `exchange` and `logs` lose `--url` (`:977-996`); `mcp` joins.
- **Rigs and fixtures.** They find a runtime through the registry, or take
  `--local-api-socket <path>` where they take `--control-plane-port`, `CONTROL_PORT`,
  `CONTROL_PLANE_URL` or `http://127.0.0.1:$PORT` today; the e2e scripts curl with
  `--unix-socket`. Engine: the two rigs, `runtime/streamlib-engine/tests/fixtures/` (six Python
  nodes, nine scripts). Packages: `packages/streamlib-moq/tests/live/`,
  `packages/streamlib-webrtc/tests/live/` — the gate does not search them.
- **Tests pinned to TCP** move to the socket: `free_port` / `launch_node`
  (`test_cli_launch.py:110`, `:238-283`), `test_the_control_plane_binds_every_interface_by_default`
  (`test_cli.py:413`), `StubControlPlane` (`test_cli_observation_verbs.py:90-180`, an `AF_UNIX`
  server), the four test apps, `test_runtime_name.py:97`, `test_cli.py:813-843`,
  `test_cross_floor_check.py:485-486`, `test_processor_identity.py:94`, `:158-159`, and the
  2025-06-18 bodies in `mcp.rs`'s tests and `test_mcp_resources_and_prompts.py:158-236`.
- **The README.** The nodes table; MCP setup becomes `claude mcp add streamlib -- streamlib mcp`
  plus the ssh form; `:217-219` ("unauthenticated port … narrow it with `--host`") becomes
  "control is reachable only on its machine".

## Companion operating-model PR (dedicated, per the flow rule)

Seven skills name port 9000, `--url`, `POST /mcp`, `control_url` or `STREAMLIB_MCP_TOKEN`:
`discover-running-nodes`, `drive-running-node`, `inspect-live-graph`, `tap-live-channel`,
`capture-node-evidence`, `verify-live`, `verify-audio` (`teardown-running-node` names only "a
control port"). They move to `--node` and the socket in their own PR once S1 merges and before the
ship gate runs, since the gate searches `.claude/`.

## Left to later changes, so nothing is lost

| Legacy | Change | Waits on |
|---|---|---|
| One runtime and one socket per `run`; `nodes` as a registry of runtimes | runtime hosting | §Product's loading OPEN, machine or user — decided 2026-10-01 (#2580); built in step 4 |
| `load`, `unload`, `streams`, `expose` as verbs | runtime hosting | the rest of the local API `:4464` — decided 2026-10-01 (#2580); built in step 4 |
| The URL forms' listener | streams as URLs | §Networking's URL-grammar OPEN |
| `host_control_plane`, `_control_plane_client`, "control plane" in names | the namespace rename — since the reorder, the package split deletes the first two (#2592, #2593); the Rust names come with the app | its OPEN — the names decided 2026-10-01 (#2580) |

## Assumptions stated, not asked

- The socket is named `local-api-<runtime_id>.sock`, after the glossary's term, not today's names.
- `mcp` reaches the runtime through an upgrade on the same socket rather than a second socket, so
  the local API stays one socket; the verb is a byte pipe because the plan's "interprets
  nothing" rules out a forwarder that derives HTTP headers from each message and maps cancels.
- `POST /mcp` stays beside `/mcp/stdio` because the CLI's one-shot calls are simplest as HTTP and
  "today's router unchanged" is decided; both framings call one `dispatch_jsonrpc`.
- `ttlMs` is 0 for `resources/read` of the live graph and one hour for the static lists.
- Registry v3 ships with no migration; `nodes` keeps listing peers read from the mesh.

## Expected slices

- **S1 — the socket.** Listener, config, registry v3, the client over `AF_UNIX`, the CLI flags,
  the bearer gate deleted, rigs and fixtures, tests, README's nodes and auth text. Touches the
  fixtures #2568 migrates — sequenced after it.
- **S2 — the announcement.** `control_plane_urls` leaves the description, `graph`, `nodes` and
  the schemas. Independent of S1.
- **S3 — MCP 2026-07-28.** The revision on `POST /mcp`, the CLI client's headers and `_meta`,
  the tests. Independent of S1 (it runs over TCP until S1 lands).
- **S4 — the `mcp` verb.** `/mcp/stdio`, the verb, its live test, README's MCP setup. Blocked by
  S1's socket (the expand ticket) and S3; it needs the socket to exist, not the port to be gone.

## Tickets

Derived 2026-10-01; milestone #60, *Local API*. S1 split expand–migrate–contract so CI stays green
while the fixtures move.

1. #2578 — a runtime stops announcing control-plane URLs on the mesh (S2) — independent; carries
   the six announcement bullets.
2. #2572 — the runtime speaks MCP 2026-07-28 and nothing earlier (S3) — independent; carries
   `2025-06-18`, `initialize_result`, `notifications/initialized`.
3. #2573 — the local API is served on a user-only socket, and the CLI talks through it (S1
   expand) — independent.
4. #2574 — rigs, fixtures and the wheels' live tests reach a node through its socket (S1 migrate)
   — blocked by 3 and #2568; needs the rig.
5. #2575 — the TCP port, `--host`/`--port`/`--url` and the bearer gate are gone (S1 contract) —
   blocked by 4; carries the listener, flag, registry and auth bullets.
6. #2576 — MCP hosts connect by launching `streamlib mcp` (S4) — blocked by 2 and 3; carries
   `claude mcp add --transport http`, `test_the_wheel_serves_no_mcp_verb`.
7. #2577 — the live-ops skills follow (the companion operating-model PR) — blocked by 5 and 6.

## REMOVED

- REMOVED: hosted_control_plane_endpoint
- REMOVED: runtime/streamlib-engine/src/core/runtime/mesh/hosted_control_plane_endpoint.rs
- REMOVED: HostedControlPlaneEndpointRegistry
- REMOVED: urls_another_machine_could_reach_it_at
- REMOVED: control_plane_urls
- REMOVED: CONTROL_PLANE_URLS
- REMOVED: DEFAULT_CONTROL_PLANE_BIND_HOST
- REMOVED: DEFAULT_CONTROL_PLANE_BIND_PORT
- REMOVED: DEFAULT_CONTROL_PLANE_PORT
- REMOVED: Could not find available port in range
- REMOVED: bind_host
- REMOVED: bind_port
- REMOVED: control_url
- REMOVED: requested_url
- REMOVED: _refuse_a_non_http_url
- REMOVED: test_the_control_plane_binds_every_interface_by_default
- REMOVED: --control-plane-port
- REMOVED: runtime/streamlib-api-server/src/auth.rs
- REMOVED: require_bearer_token
- REMOVED: require_auth
- REMOVED: AUTH_TOKEN_FILE
- REMOVED: STREAMLIB_MCP_TOKEN
- REMOVED: claude mcp add --transport http
- REMOVED: test_the_wheel_serves_no_mcp_verb
- REMOVED: 2025-06-18
- REMOVED: initialize_result
- REMOVED: notifications/initialized

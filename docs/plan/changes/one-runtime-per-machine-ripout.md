# one-runtime-per-machine-ripout

> **On hold (2026-09-30).** The plan PR was reworked so that only the owner's confirmed sentences
> are DECIDED. The entries this change implements (the local API, and the control plane reachable
> only on its own machine) are now OPEN. So are both of its `[NEEDS DECISION]` blocks: a later
> owner requirement, a free user seeing and using streams locally, may need a loopback listener. No
> plan section is flipped to IN-FLIGHT while this waits. Decision 2 below carries the plan's current
> direction as option (c). The recon below stays valid as a record of the tree at 30fbef3.

The rip-out step of the one-runtime-per-machine pivot: the control plane leaves the network. After this
change:
- every engine process that hosts its control plane serves it — the same router, vocabulary and MCP
  tools — on one Unix domain socket in its runtime directory, and on nothing else for control (the
  URL forms' loopback listener is the local-API OPEN's, decision 2 below);
- the TCP listener, its all-interfaces default, the port walk from 9000, `--host`, `--port`, `--url`
  and the registry's `control_url` are gone, with no TCP path for control kept running beside the
  socket;
- a runtime stops announcing control-plane URLs on the mesh, and `nodes` stops printing them;
- an MCP host reaches the tools the way the owner decides below.

The change implements §Control plane & observability's `[one-runtime-per-machine]` local-API OPEN
(one local API per machine, the socket the authority for control, so control is reachable only on its
own machine), and the `control_plane_urls` clauses of §Networking's announcement entry and of its
`graph` mesh-peers entry (`CONTROL_PLANE_URLS`). Rationale:
`docs/decisions/one-runtime-per-machine.md`. Of the pivot's legacy inventory, this is the part that
retires outright. Everything else in it is reshaped by a build change, and each item is mapped to its
change under "The rest of the inventory" below, so nothing is lost.

**Scale gate: this skill, plus the existing ADR.**
- The Python API's public contract: `Runtime.host_control_plane` loses `bind_host` and `bind_port`,
  and the CLI loses `--host`, `--port` and `--url`.
- The wire: the mesh description loses a field.
- The control plane's transport.

**Precondition.** The entries above are OPEN, and this change waits for them to be decided; nothing
flips. §Product's `[one-runtime-per-machine]` OPEN on how the runtime is started (whether it serves a
machine or a user) does not block, because this change serves today's per-app runtimes, whose runtime
directory is per-user today (§Control plane & observability, the runtime-directory entry). The runtime
inherits the mechanism when it exists. The local-API OPEN's undecided "how MCP hosts reach it" is
decision 2 below. No plan section flips to IN-FLIGHT while this change is on hold.

**Verified against the tree 2026-09-30 (HEAD 30fbef3; its code is identical to main 9c0356b).** One
read-only recon sweep, and the pivot's inventory sweep.

**The listener**
- `runtime/streamlib-api-server/processors/api_server.rs:130` binds a `tokio::net::TcpListener` on
  `format!("{}:{}", host, port)` (`:129`). It walks ten ports from the requested one (`:127-153`),
  and fails with "Could not find available port in range". It reports the requested port, never
  `local_addr()`, so port 0 records 0.
- `control_url` is always `http://127.0.0.1:{port}` (`:165`), and `axum::serve(listener, app)` runs
  at `:194`.
- `ApiServerConfig { host, port, log_path, require_auth }` (`src/api_server_config.rs:13-29`).
  `ApiServerControlPlaneHostConfig { bind_host, bind_port }` (`src/control_plane_host.rs:13-17`).
- The defaults `0.0.0.0` and `9000` live in three places: `sdk/streamlib-python-wheel/python/streamlib/cli.py:71-72`,
  `src/python_runtime_lifecycle.rs:475` and `_engine.pyi:654-658`.
- Two engine rigs pin `const DEFAULT_CONTROL_PLANE_PORT: u16 = 9000` behind `--control-plane-port`
  (`runtime/streamlib-engine/examples/codec_roundtrip_rig.rs:836`, `cross_runtime_link_rig.rs:79`).

**The router serves unchanged over a Unix socket**
- `build_router` (`src/handlers.rs:80`) serves these routes: `/health`, `/api/graph`, `/api/registry`,
  `/api/runtime/shutdown`, `/api/surfaces/{surface_id}/image`, `/ws/tap/{channel}`, `/ws/events`,
  `/api/openapi.json`, and `/mcp`.
- `/mcp` speaks streamable HTTP: POST only, one JSON body, `MCP_PROTOCOL_VERSION = "2025-06-18"`
  (`src/mcp.rs:69`, `:191-205`).
- Nothing reads `ConnectInfo`, the peer address or `Host`.
- The router and MCP tests drive it with `oneshot` (`handlers.rs:697-714`, `mcp.rs:1297-1302`).
- axum is 0.8.9 (`Cargo.lock:419-420`). It implements `Listener` for `tokio::net::UnixListener`
  (`serve/listener.rs:45-46`) and serves upgrades on any listener (`serve/mod.rs:396`). This was read
  from source and not yet compiled.

**The registry and the client**
- `NodeRegistryEntry { schema_version, runtime_id, runtime_name, control_url, pid, hint }` is at
  schema 2 (`src/node_registry.rs:28-46`). It is written to `<runtime dir>/nodes/<runtime_id>.json`.
- The runtime directory already holds one Unix socket, `surface-share-{runtime_id}.sock`
  (`runtime/streamlib-engine/src/core/runtime/streamlib_runtime_directory.rs:63-64`).
- Python reads the entry strictly (`_node_registry.py:35-158`).
- Liveness is a `tools/call graph` POST to `{url}/mcp` within 1.5 s, and any HTTP status counts as
  alive. An entry is pruned only when its pid is gone too (`:205-206`).
- `_control_plane_client.py` is stdlib `urllib`, which has no `AF_UNIX`. It refuses non-http URLs
  (`:72-80`).
- A target resolves by `--url` first, then `--node`, then the sole live node (`:145-206`).
- `tap` on the CLI is an MCP tool call like `graph` (`cli.py:1400-1411`); the WebSocket tap serves
  programmatic clients.

**The CLI**
- `run` and `dev` take `--host` (dest `bind_host`) and `-p/--port` (dest `bind_port`)
  (`cli.py:911-926`), and pass both to `host_control_plane` (`:285`).
- `graph`, `tap`, `exchange` and `logs` take an exclusive `--url`/`--node` group (dest
  `requested_url` and `requested_node`, `:977-996`).
- `nodes` prints `CONTROL_URL` (`:461-465`) and a mesh-peers table with a `CONTROL_PLANE_URLS`
  column (`:530-540`).

**The mesh**
- `HostedControlPlaneEndpointRegistry`
  (`runtime/streamlib-engine/src/core/runtime/mesh/hosted_control_plane_endpoint.rs:21`) records the
  bound endpoint. It derives `control_plane_urls` from `getifaddrs` (`:51-162`).
- The description carries `pub control_plane_urls: Vec<String>` with no serde default
  (`runtime_mesh_description.rs:42`). Its msgpack field names are the wire contract (`:7-8`).
- `graph.mesh.peers[]` renders the field (`core/json_schema.rs:184`), and so does the wheel's
  observe-only session (`src/python_runtime_mesh_observation.rs:27-74`).

**Auth and MCP setup today**
- Bearer gating exists (`src/auth.rs`, `handlers.rs:49-53`, `:94-132`). The token lives at
  `.streamlib/api-server/auth-token`, and the client reads `STREAMLIB_MCP_TOKEN`
  (`_control_plane_client.py:45`).
- No production path sets `require_auth`; `ApiServerControlPlaneHostConfig` has no such field
  (`control_plane_host.rs:32-40`). Every shipped node is unauthenticated.
- `README.md:206` tells users `claude mcp add --transport http streamlib http://127.0.0.1:9000/mcp`.
- `test_the_wheel_serves_no_mcp_verb` (`tests/test_cli.py:402`,
  `tests/test_cli_observation_verbs.py:1233`) pins mcp-served-with-the-node's "no CLI verb" clause.

---

## [NEEDS DECISION] 1 — the local API's protocol

The plan says one socket and the vocabulary unchanged. It does not say what runs on the socket.

- **(a) The existing router over the socket.** This is HTTP/1.1 on `AF_UNIX`, the shape of Docker's
  API and of Tailscale's LocalAPI. REST, both WebSockets, the OpenAPI document and MCP's streamable
  HTTP carry over byte for byte. The change is the listener and the client's connect, and every
  router and MCP test stands as written.
- **(b) A new framing,** such as length-prefixed JSON-RPC. It is a second wire for one vocabulary:
  the router, its tests and `exchange`'s binary PNG are all re-expressed, for no capability the
  socket lacks.

**Recommendation: (a).**

## [NEEDS DECISION] 2 — how an MCP host reaches the tools (the local-API OPEN's undecided "how MCP hosts reach it", §Control plane & observability)

An MCP host is configured with a command (stdio) or a URL (HTTP); none dials a Unix socket.

- **(a) A stdio bridge.** A CLI verb, `streamlib mcp [--node …]`, relays each JSON-RPC message to
  `POST /mcp` over the socket. It is configured as
  `claude mcp add streamlib -- streamlib mcp --node <runtime name>`.
  - Nothing listens on TCP, and the socket's permissions are the only gate.
  - It reverses mcp-served-with-the-node's "no CLI verb, stdio server, or bridge process" clause
    (§Control plane & observability, the mcp-served-with-the-node entry), and
    `test_the_wheel_serves_no_mcp_verb` goes with it.
  - The bearer gate has nothing left to protect, so it goes too.
- **(b) A loopback HTTP listener beside the socket.** It listens on 127.0.0.1 at an OS-chosen port
  recorded in the registry, and is bearer-gated.
  - URL-configured hosts keep working.
  - It brings back a TCP listener, a port to discover, and a token to hand out — which nothing does
    today (`api_server.rs:70-73` only logs the token's path).
- **(c) The plan's current direction: the socket for control, a loopback HTTP listener for the URL
  forms.** The local-API OPEN's direction (review, not decided) keeps the socket as the authority for
  control and serves the URL forms — `ndjson`, `png`, `ts`, `hls`, `whep`, `moq`, `page` — on a
  separate HTTP listener, loopback by default and a LAN or tailnet address on request, because
  browsers and ffmpeg cannot dial a socket. An MCP host then uses either (a)'s stdio bridge over the
  socket or that listener's URL. "Nothing listens on TCP" is therefore no longer the direction: a TCP
  listener stays for the forms, and whether it also serves `/mcp`, and how it is gated, follows the
  local-API OPEN.

**Recommendation: (c).** It is the plan's direction; (a) stands as its stdio half.

On 2(c), `tokio::net::TcpListener` stays for the forms listener, and none of 2(a)'s candidates below
join the REMOVED list until the local-API OPEN decides that listener's gate.

On 2(a), these join the REMOVED list below, written here without the bullet prefix so the gate does
not read them before the decision:
`tokio::net::TcpListener`, `STREAMLIB_MCP_TOKEN`, `require_bearer_token`, `require_auth`,
`AUTH_TOKEN_FILE`, `runtime/streamlib-api-server/src/auth.rs`, `claude mcp add --transport http`,
`test_the_wheel_serves_no_mcp_verb`.

On 2(b), the bearer gate is switched on for the loopback listener, and `tokio::net::TcpListener`
stays for it alone.

---

## ADDED: §Control plane & observability — the local API, as built

- **The listener.** An engine process that hosts its control plane serves `build_router` on a
  `tokio::net::UnixListener` bound at `<runtime dir>/control-plane-<runtime_id>.sock`.
  - The socket is created mode 0600 in the directory the runtime already checks as owner-only.
  - A live duplicate is refused the way the surface socket refuses one. A file left by a dead pid is
    replaced.
  - `record_what_the_control_plane_bound` records the socket's path.
- **Config.** `Runtime.host_control_plane()` takes no arguments. `ApiServerConfig` loses `host` and
  `port`, and `ApiServerControlPlaneHostConfig` loses `bind_host` and `bind_port`. The Rust rigs
  follow.
- **The registry.** The entry's `control_url` becomes `control_socket_path`, at schema version 3.
  - A reader refuses version 2 by name. The entry is per-run and rewritten at the next start, so
    there is nothing to migrate.
  - Liveness is the same `graph` round trip, now over the socket, and pruning still needs the pid
    gone too.
- **The client.** The Python client makes every call it makes today over the socket: tools,
  `exchange`'s PNG, and the probe. It uses `http.client.HTTPConnection` with an `AF_UNIX` connect,
  which is stdlib and adds no dependency.
- **The selector.** `--node <runtime name or id>` is the only way to pick a node for `graph`, `tap`,
  `exchange` and `logs`. With no `--node`, the sole live node on this machine is used, as today.

## MODIFIED: §Networking, the announcement entry (`control_plane_urls`) and the `graph` mesh-peers entry — the announcement

- The mesh description drops `control_plane_urls`. So do `graph.mesh.peers[]`, the observe-only
  session, and the `nodes` mesh-peers table.
- `hosted_control_plane_endpoint.rs` and `HostedControlPlaneEndpointRegistry` are deleted, with their
  plumbing through `Runner` and `RuntimeContext::hosted_control_plane`
  (`runtime.rs:37`, `:151`, `:336-342`, `:419`, `:700`; `runtime_context.rs:13`, `:59`, `:86`, `:105`,
  `:165`, `:257`, `:283`, `:872`).
- `graph` still lists peers. No runtime is drivable from another machine through its control plane.
- The description is a wire contract with no serde default, so an older engine reads a new one as
  unanswered. Pre-1.0 there is no cross-version wire (§Networking, the cross-runtime-links entry's
  "no cross-version wire" clause), so there is no shim.

## MODIFIED: §Control plane & observability, the bind-posture, CLI and runtime-directory entries — the CLI, the rigs, the docs

- **The CLI.** `run` and `dev` lose `--host` and `--port`. `graph`, `tap`, `exchange` and `logs` lose
  `--url`. `nodes` prints `CONTROL_SOCKET` in place of `CONTROL_URL`.
- **Rigs and fixtures.** They find a node through its registry entry, or take
  `--control-plane-socket <path>`, where they take `--control-plane-port`, `CONTROL_PORT` or
  `http://127.0.0.1:$PORT` today.
  - The e2e scripts curl with `--unix-socket`.
  - The fixtures are in `runtime/streamlib-engine/tests/fixtures/`, and the rigs are the two above.
- **The packages' live tests.** The MoQ and WebRTC wheels' live tests follow the same pattern in
  their own directories (`packages/streamlib-moq/tests/live/`, `packages/streamlib-webrtc/tests/live/`).
  The gate does not search those.
- **The README.** Its control-plane section changes:
  - the nodes table;
  - the MCP setup, per decision 2;
  - `:217-219` ("unauthenticated port … narrow it with `--host`") becomes "control is reachable
    only on its machine".
- **Tests pinned to TCP.** Each moves to the socket:
  - `launch_node` and `free_port` (`tests/test_cli_launch.py:110-261`);
  - `test_the_control_plane_binds_every_interface_by_default` (`tests/test_cli.py:413`);
  - `StubControlPlane` (`tests/test_cli_observation_verbs.py:90-173`), which becomes an `AF_UNIX`
    `HTTPServer`;
  - the four test apps that call `host_control_plane()` with the defaults.

---

## The rest of the inventory — reshaped by build changes, never kept beside them

Each build change is proposed on its own once its OPEN is decided, and carries its own REMOVED
bullets.

| Legacy (inventory C) | Build change | Waits on |
|---|---|---|
| The native `register_declared_processor_class` at decoration; `GraphSnapshot` with no Python caller; three-part `MeshPortAddress` | graph-description | its resource field: OPEN "how a resource request is spelled" |
| Per-process `Runner` construction; `rt.run()` owning signals and the watchdog; the `app` and `helper` hook roles | runtime-startup | OPENs: `run` with no runtime; machine or user; cross-stream reach; Apple permission |
| The engine-default Zenoh session; "any runtime may wire" | runtime-session-and-stream-map | OPEN: the stream map's details, including how a peer authenticates |
| The `sys.executable` helper spawn; the exact build-id handshake | per-graph-environments | OPENs: how a graph arrives with its environment; build agreement |
| `apply_thread_priority` with no arbiter | runtime-resources | OPEN: resource request spelling |
| `GpuContext::init_for_platform_sync()?` mandatory in `Runner::start` | accelerators-optional | OPEN: the accelerator line |
| `streamlib` distribution, import and crate names | tatolab-namespace-rename, last | OPEN: the rename's remaining names |

Sharing a surface beyond the runtime's own processor interpreters has no change until its OPEN is decided.

## Companion operating-model PR (dedicated, per the flow rule)

Eight live-ops skills name port 9000, `--url`, `POST /mcp`, `control_url` or `STREAMLIB_MCP_TOKEN`:
- `discover-running-nodes`;
- `drive-running-node`;
- `inspect-live-graph`;
- `tap-live-channel`;
- `capture-node-evidence`;
- `verify-live`;
- `verify-audio`;
- `teardown-running-node`.

They move to `--node` and the socket in their own PR once S1 merges, and before this change's gate
runs, since the gate searches `.claude/`.

## Expected slices

- **S1 — the socket.** The listener, the config, registry v3, the client over `AF_UNIX`, the CLI
  flags, the rigs and fixtures, and the tests.
- **S2 — the announcement.** `control_plane_urls` leaves the description, `graph` and `nodes`, and
  the schemas are regenerated. S2 is independent of S1.
- **S3 — MCP per decision 2,** and the README. It follows S1.

## Assumptions stated, not asked

- The socket's name and mode follow the surface socket's precedent in the same directory.
- Registry v3 ships with no migration, since entries are per-run.
- `nodes` keeps listing peers read from the mesh. Only the URLs leave.

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

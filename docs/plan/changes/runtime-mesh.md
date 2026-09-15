# runtime-mesh

Every runtime is on the mesh from the moment it is constructed. After this change:
- a runtime has a stable **runtime name** that belongs to it, not to its control plane;
- it opens one Zenoh session in the app process, announces itself under its mesh name and
  discovers other runtimes with nothing configured;
- a second live runtime of the same name is refused by name;
- `graph` carries the runtime's name, its session state and its mesh peers, and
  `streamlib nodes` lists mesh peers beside the local registry's nodes.

No link crosses the mesh yet: remote links, surfaces, stamps and hop loss are `cross-runtime-links`.

The change implements these `[runtime-mesh]` entries in `docs/plan/ARCHITECTURE.md`: §Networking
`:2406-2415` (session), `:2416-2423` (discovery and mesh name), `:2424-2431` (runtime name and
address grammar), `:2438-2440` (no auth) and `:2463-2465` (`graph` and `nodes`); and the
display-name clause of §Processor model `:787-794`.

Owner align 2026-09-14 (PR #2258). Rationale lives in `docs/decisions/runtime-mesh.md`, which
this proposal amends. The audit behind it is `~/Documents/streamlib-iceoryx2-deep-dive/`, notes 22
and 23.

**Scale gate: this skill, plus an ADR.**
- **Python API public contract:** `Runtime()` gains keyword arguments, `host_control_plane` loses
  `node_name`, `rt.add` refuses a display name that cannot be one address chunk, `graph` gains a
  top-level key, and `streamlib nodes` changes its output.
- **A new wire:** what one runtime announces to another.
- **A new dependency:** `zenoh`, statically linked into the wheel.

**Precondition.** Every entry above is DECIDED. Untouched: the common-clock OPEN at `:2466`, the
auth OPEN at `:2708`, and the remote-link, surface, stamp and loss entries `:2432-2437` and
`:2441-2462`, which belong to `cross-runtime-links`.

**Verified against the tree 2026-09-14 (HEAD 23967517d).** Three read-only recon sweeps checked
the naming hooks, the control-plane and lifecycle seams, and the Zenoh crate. The Zenoh sweep
read `zenoh` 1.10.1's source and probed it on the rig.

**Naming today**
- `host_control_plane(node_name=)` (`python_runtime_lifecycle.rs:349-361`) carries the name through
  `ApiServerControlPlaneHostConfig.node_name` (`control_plane_host.rs:19-21`, `:38-40`) into
  `ApiServerConfig.name` (`api_server_config.rs:24-26`). Unnamed, `generate_runtime_name`
  (`processors/api_server.rs:89-112`) mints an adjective-noun into `resolved_name` (`:203-208`).
  **Nothing reads it** — not the registry, `graph`, `/health`, logs or MCP — and nothing tests it.
- `streamlib run`/`dev --name` sets `dest="node_name"` (`cli.py:840-845`), passed after `setup` at
  `:254-256`. No test covers it.
- `runtime_id` comes from `STREAMLIB_RUNTIME_ID` verbatim, or else `R` plus a cuid2
  (`runtime_unique_id.rs:24-35`), and is minted once in `Runner::new` (`runtime.rs:163`).
- The registry entry is `{schema_version, runtime_id, control_url, pid, hint}`
  (`node_registry.rs:27-38`, mirrored at `_node_registry.py:35-42`).
  - `control_url` is hardcoded to `http://127.0.0.1:{port}` (`api_server.rs:260-270`).
  - `nodes` prints `RUNTIME_ID CONTROL_URL PID ALIVE? HINT` (`cli.py:445-483`), tested at
    `test_cli_observation_verbs.py:1183-1200`.
- The app directory reaches the engine as `STREAMLIB_APP_DIRECTORY`, a full path (`cli.py:90-98`,
  `:228`). The wheel also captures `sys.path[0]` as the entry directory
  (`python_helper_process_spawn_host.rs:84-101`).
- `Runtime.__init__(self)` takes nothing (`_engine.pyi:462`). `#[new]` runs `Runner::new` with the
  GIL detached (`python_runtime_lifecycle.rs:252-268`). `Runner::new()` has no options
  (`runtime.rs:127-249`).
- Display names are disambiguated with ` 2`, ` 3` (`add_v_op.rs:96-118`) and otherwise
  unvalidated. A unicode name round-trips (`processor_spec.rs:114`).
- The engine reads no hostname anywhere. `channel_name.rs:13-21` still says a channel name becomes
  "a Zenoh key-expression cross-node", which `:2430-2431` now rules out.

**Lifecycle and control plane**
- `Runner::new()` needs no GPU: tokio `:146-157`, runtime id `:163`, logging `:173`, init hooks
  `:184`, iceoryx2 node `:199`, event bus `:204`, surface socket `:212`, whose
  `bring_up_surface_service` already refuses a live duplicate (`:1290-1318`). `start()` fails
  without Vulkan at `:353`. `stop()` is `:519-599`; `Runner` has no `Drop` impl.
- `GraphResponse` is `{nodes, links, extensions}` (`core/json_schema.rs:30-38`), its key list pinned
  by `a_graph_with_no_extensions_still_carries_the_key_as_an_empty_list` (`:804-808`) and
  deserialized strictly at `mcp_prompts.rs:172`. No OpenAPI body schema describes it
  (`handlers.rs:159-167`).
- The engine has current-thread tokio runtimes at `tap.rs:413,473,526`,
  `operations_runtime.rs:802,825` and `surface_image_exchange.rs:217`.
- The engine crate has no self-re-exec test. The nearest pattern is a `[[bin]]` launched through
  `CARGO_BIN_EXE_*` (`tests/surface_share_subprocess_crash.rs:32`), and it is not in CI.

**Zenoh 1.10.1** (newest, published 2026-09-07)
- **Licence:** `EPL-2.0 OR Apache-2.0`, MSRV 1.75. The workspace is 1.88.
- **Default features** include QUIC, TLS, WebSocket, compression and auth.
  - `transport_udp` hard-enables the QUIC datagram link, which pulls in quinn, rustls, ring and
    CDLA-Permissive-2.0 `webpki-roots`. `deny.toml:42-62` refuses that licence.
  - `default-features = false, features = ["transport_tcp"]` builds cleanly. Multicast scouting,
    liveliness and queryables work on it with no `unstable`.
  - Under the repo's `deny.toml` it passes on the Apache arm. It adds no `DT_NEEDED`, and costs
    +3.6 MB gzip / +10 MB stripped.
- **Session open** in peer mode waits `scouting/delay` (500 ms) with scouting on and takes 0.5 ms
  with multicast off and no peers. It succeeds with no network (`docker --network none`) and fails
  only on a listener or multicast bind. Close takes 0.1–0.2 ms.
- **Threads:** Zenoh runs its own pool of five tokio runtimes. `.wait()` works from a plain thread
  and panics on a current-thread runtime.
- **Liveliness tokens carry no payload** (`builders/liveliness.rs:38-41`; replies at
  `session.rs:3224-3240`), and a sample carries no zid.
  - After a SIGKILL a peer sees the token's delete within milliseconds, because the kernel closes
    TCP. The 10 s lease matters only for a partition or power loss.
- **Key chunks** may not be empty or contain `/`, `*`, `$`, `#` or `?`; one starting with `@` is
  verbatim, so `**` never matches it; spaces and unicode are legal. The `namespace` config prefixes
  keys but does not stop sessions of different meshes connecting.
- **Multicast on the rig:** `auto` picks Wi-Fi (`lo` has no MULTICAST flag); pinned to `127.0.0.1`,
  two processes still found each other.

---

## ADDED: §Networking — how a runtime is told its name and mesh

A runtime's mesh configuration has five values, all optional.

**Python.** `Runtime(*, runtime_name=None, mesh_name=None, mesh_peer_endpoints=None,
mesh_listen_endpoints=None, mesh_multicast_discovery=None)`, keyword-only and stub-gated.

**Rust.** `Runner::new_with_runtime_mesh_configuration(RuntimeMeshConfiguration)`, with
`Runner::new()` taking the defaults.

**Environment.** A value the constructor leaves unset is read from `STREAMLIB_RUNTIME_NAME`,
`STREAMLIB_MESH_NAME`, `STREAMLIB_MESH_PEER_ENDPOINTS`, `STREAMLIB_MESH_LISTEN_ENDPOINTS` or
`STREAMLIB_MESH_MULTICAST_DISCOVERY`. The two endpoint variables take comma-separated lists, and
the discovery variable takes `0` or `1`. This happens in the engine, so a Rust app and a container
get it too.

**CLI.** `streamlib run` and `dev` take `--runtime-name`, `--mesh-name`, `--mesh-peer` and
`--mesh-listen` (both repeatable) and `--no-mesh-multicast-discovery`, and pass them to `Runtime()`.
`--name` goes.

**Values.** An endpoint is a Zenoh locator, `tcp/<host>:<port>`, and a router is named the way a
peer is. A malformed value — an endpoint on a transport the build lacks, such as `udp/`, included —
is refused at construction by name, the caller's wiring error as a wrong `device_id` is. An
unreachable endpoint never fails the runtime.

**DECIDED (owner, 2026-09-14) — a runtime's name and mesh come from its constructor, its
environment or the CLI, and nowhere else.** `streamlib run` constructs `Runtime()` before
`setup(rt)` (`cli.py:243-256`), so a CLI-launched app is named on its command line or in its
environment, never in `app.py`; most apps need neither, the default being stable. The session
therefore opens in `Runner::new()`, which keeps the duplicate refusal and discovery CI-provable
with no GPU and lets a container configure itself with no code. Rejected: `setup(rt)` naming the
runtime, which makes mesh configuration mutable state on a constructed runtime and moves both
proofs behind the GPU in `start()`; and a `[tool.streamlib]` table in `pyproject.toml`, the first
streamlib-specific file an app would author, which the zero-ceremony bar (`:33-40`) rules out.

## MODIFIED: §Networking `:2406-2415` — how the session is read

1. **The session opens in `Runner::new()`.** It opens after logging and before the iceoryx2 node,
   beside the runtime-id socket refusal, which is where a runtime's identity already comes up. A
   refused runtime therefore builds no iceoryx2 node and no surface socket.
   - `Runtime()` runs this with the GIL detached, as it does today.
   - **Cost:** `Runtime()` takes about 500 ms longer while multicast discovery is on. This is
     Zenoh's own scouting delay, which the engine chooses and no author can set; a ticket may
     measure and lower it. `dev`'s warm restart pays it.
2. **The session is built from defaults, never from a Zenoh config file or `ZENOH_*` variable.**
   - Peer mode.
   - TCP listener on `tcp/[::]:0`, unless `mesh_listen_endpoints` names one.
   - Multicast scouting on its default group.
   - No `namespace`: the mesh name is a key prefix the engine writes itself (`:2419`).
   This is the M1 domain precedent applied to a second transport.
3. **Local-only means the open failed**, which in peer mode is a bind failure. The runtime warns
   once, naming the reason, runs on, and never retries for its life.
   - A runtime with discovery off and no peers has a session that reaches nobody. That is isolation,
     not local-only, and `graph` says which one it is.
4. **The session closes at the end of `stop()`.** The token is undeclared first, so peers see the
   runtime leave at once.
   - An engine the ladder leaks (`local-transport-hardening` S2) is closed by process exit. The
     kernel closes its TCP connections, and peers see the leave within milliseconds.
   - No Zenoh call ever runs on one of the engine's current-thread tokio runtimes.
5. **A helper opens no session because it never constructs a `Runner`.** Zenoh's thread pool starts
   on first use, so a helper spawns none of its threads.

## MODIFIED: §Networking `:2416-2423` — how announcement and discovery are read

1. **A runtime announces itself with a liveliness token plus a description queryable, both under
   `streamlib/<mesh name>/@runtime/<runtime name>`.**
   - A token cannot carry a payload, so the token key carries only what a dead runtime must still
     answer: its host identity and its pid.
   - The queryable answers a msgpack document: `runtime_id`, `host_name`, `pid`,
     `engine_version` (the crate version) and `control_plane_urls`.
     - `control_plane_urls` is one `http://<address>:<port>` per non-loopback, non-link-local
       interface address the bind covers — an IPv6 literal bracketed (RFC 3986), so
       `http://[2001:db8::1]:9000` — and is empty with no control plane.
     - It answers from the runtime's state at query time, so a control plane hosted after
       construction shows up.
   - The `@runtime` chunk is verbatim, so no `**` subscription over a mesh's port addresses ever
     matches it. Since a display name may not begin with `@` (below), no address collides with it.
   - The rest of the key layout belongs to the tickets.
2. **Discovery is a liveliness subscriber with history**, plus one description query per runtime
   that appears; one that leaves is removed. `graph` reads the peer table lock-free, never waiting.
3. **The mesh name is one chunk of the channel-name grammar**, `[a-z][a-z0-9_-]*`
   (`channel_name.rs`).
   - Runtimes in different meshes on one network may still connect at the transport and exchange
     nothing, and unrelated Zenoh traffic (ROS 2's `rmw_zenoh`) may connect the same way. This is
     what `:2421-2422`'s "separate by naming different meshes" means in the tree.

## MODIFIED: §Networking `:2424-2431` — how the runtime name is read

1. **Grammar.** A runtime name is one legal key chunk.
   - It is non-empty, contains no `/`, `*`, `$`, `#` or `?`, and does not begin with `@`.
   - It is otherwise free text like a display name, validated against the real `zenoh-keyexpr`
     rules.
   - An explicit name that breaks the rule is refused at construction, naming the character.
2. **Default.** `<hostname>-<app directory name>-<id>`, with every forbidden character replaced
   by `-`. The id is four base-36 characters of an FNV-1a hash over the directory's full path,
   the virtual camera's own recipe (`virtual_camera_sink.rs:509-520`), so two checkouts of one
   app on one host get different names and every run of one checkout gets the same one. The app
   directory is resolved in this order:
   - `STREAMLIB_APP_DIRECTORY`, which the CLI sets;
   - the wheel's captured entry directory for a hand-run `python app.py`;
   - otherwise the working directory, which is what a Rust app gets.
   - **Never auto-suffixed** (owner, 2026-09-14). A name is the address other runtimes and agents
     wire against, so it may not depend on start order the way the control-plane port does
     (`api_server.rs:220-249` increments from 9000; the registry records the real URL).
   - **Where it bites:** a second run from the same directory is refused until one is given
     `--runtime-name`; moving the directory renames the runtime, as it already relabels its
     unnamed virtual cameras.
   - **The refusal** names the runtime name and the holder's host and pid — both on the token key
     — and offers both fixes: stop it (`streamlib nodes` shows it) or start under another name.
3. **The duplicate check.** Before declaring its token, a runtime queries the mesh for tokens under
   its own name. Discovery is on, so `open` has already waited out the scouting delay. The query then
   waits at most an engine-chosen bound for connected peers to answer.
   - Any token refuses `Runtime()` by name, naming the host that holds it, unless its host identity
     is this host's and its pid is gone.
   - Host identity on Linux is the kernel boot id plus the pid-namespace inode, so a container on
     the same kernel is never mistaken for this host. The probe showed a killed runtime's token
     gone within milliseconds, so the exception covers a restart racing its predecessor's exit.
   - macOS has no host identity and no exception, so a duplicate there is refused until the old
     token leaves. The check compiles on both platforms (`core/` plus a `linux/` and an `apple/`
     half) and is cross-compile verified.
4. **Stated residual.** Two runtimes that start inside one discovery window, or meet when a
   partition heals, are not refused. Both keep running, each says so once naming the other's host,
   and `graph` lists both. Which one a remote link reaches is `cross-runtime-links`'s to settle.
5. **`runtime_id` stays** per-run — logs, registry file, iceoryx2 names, the description — never an address.

## MODIFIED: §Processor model `:787-794` — a display name is one address chunk

The display name is part of a port's mesh address. `add` therefore refuses a requested display name
that is empty, contains `/`, `*`, `$`, `#` or `?`, or begins with `@`, naming the character and
the fix. The refusal applies in Rust, in `rt.add`, and in MCP `add_processor`.
- A class short name and the engine's ` 2` suffix always pass, so no default display name is
  refused.
- Spaces and unicode stay legal, and `processor_spec.rs:114`'s round trip stands.

## MODIFIED: §Networking `:2463-2465` and §Control plane `:2625-2628` — `graph`, the registry and `nodes`

1. **`graph` gains a fourth top-level key, `mesh`, always present:**
   `{"mesh_name", "runtime_name", "session": "open" | "local_only", "local_only_reason"?, "peers":
   [...]}`.
   - Each peer is `{"runtime_name", "runtime_id", "host_name", "engine_version",
     "control_plane_urls"}`, sorted by name. `runtime_name` is always present; the other four are
     absent until the description answers, so each is optional on the peer type, and the key-list
     test, the strict fixture and the schema cover both shapes.
   - `local_only_reason` is present only when the session is local-only.
   - No peer carries a last-seen time: a wall-clock one would be a fifth surface (`:1171-1181`), and
     a monotonic one means nothing to another machine.
   - The key list test, `mcp_prompts.rs:172`'s strict fixture and `generate_schemas.rs` follow.
2. **The registry entry gains `runtime_name`** and bumps `schema_version`.
   - `host_control_plane` takes the name from its runtime.
   - `node_name`, `ApiServerConfig.name`, `resolved_name` and the generator go.
   - This composes with `local-transport-hardening`'s MODIFIED entry on the same lines, which moves
     only the registry's directory.
3. **`streamlib nodes` puts `RUNTIME_NAME` first**, and `--node` accepts a runtime name or a runtime
   id.
4. **`nodes` adds a mesh-peers table below the registry table:**
   `RUNTIME_NAME HOST CONTROL_PLANE_URLS ENGINE_VERSION`.
   - The rows come from a short-lived session that declares nothing. It is reached through a
     stub-gated `_engine` function and takes `--mesh-name`, `--mesh-peer` and
     `--no-mesh-multicast-discovery`.
   - A peer that is already a registry row is not repeated.
   - **Cost:** about one second more per `nodes` call: the scouting delay plus the query bound.
   - `nodes` is a registry surface rather than a tool (`:2563-2564`), so the CLI stays a JSON-RPC
     client for every tool.

## Carried without plan text: the dependency

- **Which crates.** `zenoh = { version = "1.10.1", default-features = false, features =
  ["transport_tcp"] }` on `streamlib-engine`, the only crate that names it.
  - No `zenoh-ext`: its defaults turn every transport back on.
  - No `unstable` and no `shared-memory`.
  - UDP waits for `cross-runtime-links`, if a link needs it. That would add CDLA-Permissive-2.0 to
    `deny.toml`, which that change must bring.
- **Licence.**
  - Zenoh is elected under Apache-2.0; `EPL-2.0` never joins `deny.toml`.
  - Zenoh's crates ship no `LICENSE` or `NOTICE`, and `cargo about` collects no NOTICE files
    (`about.toml:26-32`). The notices generator therefore carries Zenoh's upstream `NOTICE.md` for
    Apache-2.0 §4(d), through the roster mechanism it already uses for vendored C++ projects.
  - `test_third_party_notices.py` samples `zenoh`.
  - `LICENSE`, `LICENSES/` and `docs/license/` are untouched.
- **Portability gate.** `test_wheel_portability.py` stays the pass/fail. The probe linked no new
  host library.
- **Tests never join the default mesh by accident.**
  - Every test that constructs a runtime runs with multicast discovery off, unless it is a mesh
    test. The engine's lib tests get this through a test-only default. The wheel's pytest, the
    engine's integration tests and the CI workflows get it through `STREAMLIB_MESH_MULTICAST_DISCOVERY=0`.
  - A mesh test pins multicast to `127.0.0.1` through a test-only override and takes its own mesh
    name.
  - Rig fixtures that passed `node_name` pass `runtime_name`.

## Left to `cross-runtime-links`, so this change is not read as settling them

- **What a runtime offers:** learned by query when a link names a port, never announced.
- **Peers of another engine version:** `engine_version` is rendered here; whether a link between
  two versions is refused, and whether M4's build id joins the announcement, is that change's.
- **Duplicate names met after start** (residual 4 above): that change defines what a link naming
  a name two live runtimes hold does before any link resolves a name.

## Assumptions stated, not asked

- **No `ctx.runtime_name` for processors.** The MoQ wheel's `streamlib/<runtime_id>` broadcast
  default is a consumer's and is left alone.
- **The two `packages/` live fixtures passing `node_name=`** (`moq_broadcast_roundtrip_node.py:265`,
  `whip_whep_roundtrip_node.py:155`) migrate in the same PR, per the 2026-09-11 canary ruling.
- **`/verify-live`** gains no mesh bullet here; the two-runtime rig proof is `cross-runtime-links`'s.

## Expected slices

`/derive-tickets` decides the breakdown. The shape the recon supports:

| # | Slice | Blocked by | Proof |
|---|---|---|---|
| N1 | The five configuration values across constructor, environment and CLI; runtime-name and mesh-name grammar and default; display-name refusal in all three doors; `runtime_name` on the registry entry, the `nodes` column and `--node`; `node_name`, `--name`, `ApiServerConfig.name` and the generator deleted | — | CI: the default comes from hostname, app directory name and the path hash in all three resolution arms — two directories sharing a final component get different defaults, one directory gets the same default twice; a keyword beats the environment, which beats the default; each forbidden character is refused by name in a runtime name, a mesh name and a display name through `rt.add` and MCP; a `udp/` endpoint is refused by name; the `nodes` table test shows the name, and `--node <runtime name>` resolves |
| N2 | `zenoh` (TCP only); session in `Runner::new()`; token and description queryable; peer table; duplicate refusal with same-host takeover; local-only; close at `stop()`; `graph` `mesh` key; test defaults; notices and portability | N1 | CI, GPU-free, two OS processes, multicast pinned to `127.0.0.1` with a per-test mesh name: each lists the other in `graph.mesh.peers` within a bound, and an arm with explicit `tcp/127.0.0.1` peers and discovery off does the same. A second process of the same name is refused naming the host; after a SIGKILL of the first the name is free at once. A peer that closes leaves `graph`; one whose description has not answered renders its name alone and still deserializes. An IPv6 control-plane address renders bracketed. Two mesh names see nothing of each other. A taken listen endpoint gives `local_only` with its reason and a constructed runtime. Portability and notices gates are green. Rig: two `streamlib run` apps list each other, and SIGTERM exits both cleanly |
| N3 | `streamlib nodes` mesh-peers table through the observe-only session | N2 | CI (GPU-free pytest): a `Runtime()` in a subprocess under a test mesh appears in `nodes --mesh-name <test mesh>` and is not repeated when it is also a registry row; with no peers the table says so |

Every new engine test is named in `.github/workflows/test.yml`'s slice and the
`run_local_ci_gates` mirror (`xtask/src/main.rs:204`, `:317`), or it runs nowhere — so the
two-process fixture must be reachable from that slice, which a `CARGO_BIN_EXE_*` integration test
is not today. Multicast on GitHub's runners is unverified: the explicit-peer arm does not depend on
it, and the first CI run proves the multicast arm or retires it to the rig.

Nothing in `local-transport-hardening` or `loss-visibility` blocks N1–N3. N2, #2261 (M1) and
#2266 (S2) all edit `Runner::new` and `stop()`; whichever lands second rebases.

## Records the implementation owes, in the same PRs

- The "Zenoh key-expression cross-node" sentences at `channel_name.rs:13-21`, rechecking the
  related wording at `:42`, `:136-137` and `:320-345`.
- `control_plane_host.rs:5`'s nonexistent `streamlib-runtime` binary; `:19-21`'s registry claim.
- `_engine.pyi`'s `Runtime.__init__` (`:462`) and `host_control_plane` (`:488-500`) docstrings;
  `cli.py:840-845`'s help text; `THIRD-PARTY-NOTICES.md`, regenerated.
- At `/ship-change`, the glossary gains **Runtime name** and **Mesh name**.

## REMOVED

Naming (N1):
- REMOVED: generate_runtime_name
- REMOVED: resolved_name
- REMOVED: ADJECTIVES
- REMOVED: pub node_name: Option<String>
- REMOVED: node_name: Optional[str]
- REMOVED: node_name=
- REMOVED: dest="node_name"
- REMOVED: Node name published to the registry

Records (N2):
- REMOVED: a Zenoh key-expression cross-node
  The channel name never reaches the mesh (`:2430-2431`).

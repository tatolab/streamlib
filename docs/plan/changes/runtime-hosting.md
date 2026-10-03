# runtime-hosting

Step 4 of the one-runtime-per-machine pivot: one runtime per machine runs many streams, keeps the
ones it was told to keep, is addressed by its machine, and arrives from an installer. After it:
- `tatolabd` takes no stream. It is the machine's one runtime process — one engine, one GPU
  context, one Zenoh session, one local API on one socket — and streams load into it and leave it
  without touching each other; each has its own graph, events, registry of node types,
  interpreter, logs, shutdown, watchdog and exposures;
- every stream action the CLI has — `run`, `run -d`, `stop`, `start`, `rm`, `streams`, `expose` —
  is a local-API tool (`dev` is `run` plus a watch); with no runtime running each fails at the
  socket; `graph` returns every stream;
- a kept stream is recorded in the state directory and comes back on every start, a crash's
  restart included, unless stopped or failed; an attached one lives as long as its connection;
- every exposed name is cast to lowercase URL-safe; nothing is added to a stream's environment;
- an address is `<machine>/<stream>/<node>/<port>`, right-anchored; the runtime name is gone;
- Linux installs by `curl | sh`, which registers a systemd user service; macOS installs a signed,
  notarised `Tatolab.app` that starts the runtime if it is not running, as Docker Desktop does.

**Scale gate — this skill, plus the existing ADR.** The processor model, the Python API's public
contract (the builder's remote references), the wire (mesh keys, the announcement) and the local
API's tools move. The rationale is `docs/decisions/runtime-hosting.md` (#2580, #2600), extended
here with decisions 1 to 5, all resolved by the owner.

**Precondition.** Every entry built is DECIDED: §Product `ARCHITECTURE.md:107-113`, `:114-121`
(who starts it), `:122-149` (loaded and kept; one user), `:158-163`; §Processor model
`:1395-1416` (graph is data, emitted; environment beside it), `:1424-1436` (the split),
`:1443-1455` (failure isolation); §Media I/O `:3211-3222` (built here, decision 2); §Networking
`:4027-4031` (own configuration — router mode is step 8's), `:4061-4087` (address, collisions),
`:4088-4095` (exposure); §Distribution `:4225-4235`; §Control plane `:4415-4431`, `:4575-4583`,
`:4596-4601`. Not built against: needs `:1417`, resources `:1437`, accelerators optional `:1891`
(the GPU still initialises at start), packs `:584`, discovery `:4096`, the stream map `:4113`,
URLs `:4125`, the relay role, the control client. **Sequencing:** after #2592, #2593 and #2566.

**Verified against the tree 2026-10-02 (HEAD 65e6c53a7)**, three sweeps; `E` =
`runtime/streamlib-engine/src`, `A` = `runtime/streamlib-api-server`.
- **Collides between two streams in one process:** `PUBSUB` (`E/core/pubsub/bus.rs:13`) with
  id-less runtime events (`events.rs:15`, `:55-69`, `:170`, `:317`) — every listener commits on
  any change (`graph_change_listener.rs:38-63`) and one shutdown stops every loop
  (`runtime.rs:1209-1226`, `:1558-1561`, `:1304`); `PROCESSOR_REGISTRY` and its resolver
  (`processor_instance_factory.rs:232-247`, `:461`); the shutdown escalation
  (`runtime_shutdown_request.rs:62`, `:76-85`); the watchdog's exit 124
  (`engine_teardown_watchdog.rs:22-94`, `end_the_process_at_once.rs:40-51`);
  `APP_ENTRY_DIRECTORY_CAPTURED_BY_THE_LANGUAGE_HOST` (`core/app_directory.rs:19`), which also
  names virtual cameras (`runtime/streamlib-media-builtins/src/virtual_camera_sink.rs:34`, `:501`);
  `get_streamlib_home()`, the process's cwd (`E/core/streamlib_home.rs:18`), holding logs and the
  pipeline cache; first-wins logging (`logging/init.rs:48-56`, `:102`). A `GpuContext` per
  `start()` (`runtime.rs:565`) with a first-wins `VULKAN_DEVICE_FOR_IMPORT`
  (`vulkan/rhi/vulkan_buffer.rs:27`) is the NVIDIA dual-device crash. The helper-group table is
  lock-free by design, read from the signal thread (`helper_process_group_registry.rs:4-9`, `:37`).
- **The seam:** `Runner` (`runtime.rs:140-230`) owns one `Compiler`, which owns the graph,
  transaction and commit lock (`compiler.rs:29-40`); `commit` takes its context (`:93`). Wired to
  one graph: the offer and link-request answers (`runtime.rs:391-393`, `:452-456`) and the local
  API, an `ApiServer` processor holding one `RuntimeOperations` (`A/src/control_plane_host.rs:24-47`,
  `state.rs:14`). Test binaries build many `Runner`s each (86 calls across 24 files).
- **Channels:** cuid2-named locally (`E/iceoryx2/channel_name.rs:198-215`); a mesh ingress
  (`:242-248`) allows one publisher (`iceoryx2/node.rs:321`). **Names:** `runtime_name.rs:25-180`,
  ~800 lines; every Zenoh key in `runtime_mesh_key.rs:28-326`,
  parsed by position (`:164-184`, `:304-319`); host identity is boot-scoped
  (`host_identity.rs:20-107`); the duplicate check reads a failed query as free
  (`duplicate_runtime_name_on_the_mesh.rs:34-161`). Nothing persistent per user exists.
- **Tools:** `shutdown` reaches the global funnel (`A/src/mcp.rs:590-604`, `handlers.rs:179-215`);
  Quit in Apple's menu does too (`E/apple/application_menu.rs:42`). **Release:** wheels only, ad
  hoc signed (`release-wheel.yml:61-269`, `macos-wheel.yml:37-190`); no installer anywhere.

---

## Decision 1 — RESOLVED: the runtime has no shutdown verb

Owner, 2026-10-02: "shutdown doesn't feel like it makes sense in this context" — the runtime stays
on, as `tailscaled` does. The `shutdown` tool, `POST /api/runtime/shutdown` and the runtime's Quit
menu item go; `stop <stream>` ends a client's work; the runtime stops as a service does
(`systemctl --user stop`, the app's switch) or by a signal in the terminal running it.

## Decision 2 — RESOLVED (a): a minimal `Tatolab.app` ships in this change

Owner, 2026-10-02: on a Mac "most people would open the app and it starts the service if not
started just like docker"; the owner holds the Developer ID. Chosen over a terminal until step 10;
§Product's who-starts entry holds as written, the terminal a developer's path; step 10 grows it.

## Decision 3 — RESOLVED: every exposed name is cast to lowercase URL-safe

Owner, 2026-10-02: only URL-safe names, "always cast to lowercase … don't force people to write it
like that or change imports". Every exposed name is cast — lowercased, accents dropped, anything
outside RFC 3986's unreserved set turned into `-`, ≤63 characters; empty, `.` or `..` refused — and
a defaulted node duplicate takes `-2` (`camerasource-2`). Written into §Networking's address entry and
amending the display-name entry. Node and port names are cast in step 1 — #2565's live add and
#2567's builder build the cast and `-2`, not ` 2`, and `@node` refuses ports casting alike
(stream-graph re-spelled here); machine and stream names and remote addresses in S3. The engine
and the pure-Python builder, which cannot call it, each cast from one fixture of cases.

## Decision 4 — RESOLVED: no environment variables are built in

Owner, 2026-10-02: a stream that wants a `.env` "could just include a python library that reads
.env files on their own … why do we need to do that?" The runtime adds nothing per stream and
nothing from the caller — no `.env`, no `-e`, no shell — to a stream's compile and processor
interpreters, which start in the project directory; examples' `os.environ` reads go with their
conversion backlog (the extensions never read any).

## Decision 5 — RESOLVED: a stream that keeps crashing the runtime is `failed`

Owner, 2026-10-02: "restarting is not the same as failing … I'd want it in a failed state … and
basically just make the user aware". A kept stream always restarts; one implicated in the
runtime's last two crashes in a row — no time window; a clean stop of the runtime or a manual
restart is no crash and resets the count — is recorded `failed` with its reason, shown by
`streams`, skipped at start; `start` retries it, `rm` forgets it. A kept stream that cannot re-load
at start (a missing venv, a type that will not describe) is `failed` too; a first load that fails
is refused, as before. A crash no stream owns restarts and is a bug to file. Chosen over parking
it `stopped` and over offline `stop` and `rm`; how a crash is pinned is an assumption below.

## Target layout

```
runtime/tatolabd/, runtime/tatolab-cli/       the runtime process and the CLI
runtime/streamlib-engine/src/core/runtime/    runtime.rs (Runner, the one engine), loaded_stream.rs
                                              (LoadedStreamInThisRuntime), machine_state_directory.rs,
                                              machine_name.rs (replaces runtime_name.rs)
installer/                                    install.sh, tatolabd.service, homebrew/tatolab.rb (the cask)
apps/tatolab-macos/                           Tatolab.app, a menu-bar app carrying the runtime
```

## ADDED: §Processor model — one engine, many streams

- **`Runner` is the engine.** `tatolabd` builds one; the library type stays constructible so the
  runtime suite runs many per test binary. It makes the `GpuContext` once (so
  `VULKAN_DEVICE_FOR_IMPORT` names the only device), the iceoryx2 node, one tokio runtime, the
  mesh membership, one surface service keyed by stream, signal ownership, and hosts the local API
  itself — the `ApiServer` processor and `control_plane_host.rs` go.
- **`LoadedStreamInThisRuntime`**, keyed by stream name: its `Compiler`, `RuntimeStatus`,
  listener, and a `RuntimeContext` sharing the engine's GPU, node and tokio whose `runtime_ops` is
  the stream. Events carry their stream, on a per-stream topic. Node types: built-ins once for the
  machine; Python descriptors per stream, described in its interpreter; the node catalog lists
  both. Logs: one subscriber, records routed per stream. Shutdown: per-stream funnel and
  escalation; the machine's ladder walks every stream at once. The helper-group table stays one
  lock-free machine table, each slot tagged with its stream. Exposures, the offer and link-request
  answers (`*ThisRuntimesGraph` → `*ThisStreamsGraph`) and the virtual camera's stable id (the
  stream's project directory, its name and the node's) are per stream; a pipeline cache per stream.
- **The watchdog is per stream:** on expiry it kills that stream's groups, abandons its threads and
  unloads the stream, never recording it `failed`. Past an engine-chosen bound of abandoned
  threads, the runtime exits 124, counted (engineering) as a crash implicating those streams.
- **Links between streams on one machine** ride iceoryx2 with no exposure; the input's stream owns
  the link; a source stream not loaded leaves it `awaiting_remote`, reason naming it.
- **A remote port read by several streams** has one ingress per machine that each subscribes to.

## ADDED: §Product — `tatolabd`, the state directory, the verbs

- **`tatolabd`** takes the machine lock, opens the state directory, resolves the machine name, joins
  the mesh, serves `<runtime dir>/local-api.sock`, and re-loads every kept stream neither stopped
  nor failed (decision 5); one that cannot re-load is `failed`, never deleted. It never detaches.
  Signed on macOS, it loads only its bundled Vulkan loader and MoltenVK, named in
  `VK_ADD_DRIVER_FILES` before the first instance.
- **The machine lock**: on Linux the abstract socket `@tatolab-runtime` (a container sharing the
  host's network is this machine to it); on macOS an `fcntl` lock on a root-owned 0666 regular
  file in root-owned `/Library/Application Support/Tatolab/`, made at the app's first-launch
  administrator prompt — any other type, owner or mode refused by name. A second `tatolabd`, or
  any squatter, is refused naming its user, pid and executable (peer credentials, `F_GETLK`); any
  user may take the one runtime first, the decided "whoever started it"; the CLI names it too.
- **The state directory**: `$XDG_STATE_HOME/tatolab/` (else `~/.local/state/tatolab/`), or
  `~/Library/Application Support/Tatolab/`: `machine.json` (machine id, name, mesh settings),
  `streams/<stream>.json` per kept stream (the graph compiled at load, the environment, `stopped` or
  `failed` and why, its crash count, exposure rulings; mode 0600) and the runtime's own log, which
  belongs to no project. A stream's logs and pipeline cache stay under its project's `.streamlib/`,
  as `:4462-4481` decides.
- **Loading.** `run_stream {project_directory, stream_function, name, keep}` — `stream_function`
  as `run` takes it (`stream.py:main`, none for the sole one): the runtime finds
  `<project>/.venv/bin/python` (absent → refused, pointing at `uv sync`), runs `tatolab.stream`'s
  compile entry in it, describes the Python types, and loads. A name loaded or kept, stopped or
  failed included, is refused naming its project, `--name` the way out; a `keep` load of the same project
  and function replaces the record — how a changed source is picked up.
- **Attached is the connection's lifetime.** `keep: false` binds the stream to the connection
  carrying the call — `/mcp/stdio`, which `tatolab run` and `tatolab mcp` both hold; it unloads
  when that connection closes (Ctrl-C, a closed terminal, a killed CLI, an MCP host exiting). `run`
  follows the stream's records with `logs {stream, after}`, `after` a record sequence number; a
  runtime crash closes the connection and `run` exits 1 naming it. A one-shot `POST /mcp` can only keep.
- **The tools**: `stop_stream` (unload; record `stopped` for a kept stream, end an attached one's
  `run`), `start_stream` (a stopped or failed one), `remove_stream` (unload, forget),
  `list_streams` (name, attached / kept / stopped / failed and why, project, node count),
  `expose_port {stream, node, port, exposed}` (the owner's ruling,
  recorded for a kept stream; the function's `exposed` is the default for a port without one).
  `dev` is `run` plus a watch re-loading on save and, after a crash, waiting and loading again.
  `set --machine-name | --mesh-name | --mesh-peer | --mesh-listen | --no-mesh-multicast-discovery`
  writes `machine.json`, applied at the next start, said so; `tatolabd` flags, then `STREAMLIB_MESH_*`,
  override it — the flags `run` and `dev` carried today leave them.
- **Restart.** The service restarts `tatolabd` on failure and kept streams return; criterion
  (#2559's record): `kill -9` on the rig, the kept scaffold stream shows frames again within 10 s.

## ADDED: §Control plane — every tool names its stream

- `graph {stream?}`: one stream's one-shape graph, loadable, or `{machine, streams: […], mesh}`.
  `add_node`, `remove_node`, `connect`, `disconnect`, `tap` and `logs` take `stream`; `exchange`
  takes a surface id, unique on the machine. A link end renders right-anchored — `{node, port}`,
  `{stream, node, port}`, `{machine, stream, node, port}` — and `connect` takes `<end>_machine`,
  `<end>_stream`, `<end>_node`, `<end>_port`, leading parts omitted meaning here; `tap`'s channel
  is an address. Renamed on the wire: `created_by_machine`, `mesh.machine`, `peers[].machine`,
  `egress_ports[].readers: [{machine, stream}]`, `link_requests_awaiting_machine` and
  `awaiting_machine`, `disconnect`'s `input_machine` and `input_stream`, `connect`'s answer
  `input_machine`, a link request's `requester_machine` and `requester_stream`. The `graph`
  resource, the catalog, instructions and prompts follow. `nodes`, `--node` and the registry go:
  one socket at a fixed path; `graph.mesh` lists peers.

## MODIFIED: §Networking — the machine segment

- **Addresses and keys.** `MeshPortAddress` gains the stream; the grammar and
  `runtime_mesh_key.rs` change in one place each; the token is
  `streamlib/<mesh>/@machine/<machine>/<machine id>/<host identity>/<pid>`; offered ports, readers,
  link requests, egress and data keys gain the stream; `InboundLinkName` and the ingress hash follow.
- **The machine id** — 128 random bits minted once into `machine.json` — rides the token. **The
  name**: the recorded one, else the hostname, cast (decision 3). The claim reads every `@machine`
  token: a live holder with another id moves this machine to the next unused `<name>-2`, `-3`…,
  recorded and said once with `tatolab set --machine-name`; a token with this id from another boot,
  or a gone pid, is taken over. A failed query is never read as free: the runtime stays off the
  mesh, `graph.mesh` renders it `claiming`, and it retries on an engine-chosen backoff. Two claims
  inside one discovery window keep today's residual: both say so, links error.
- **The builder.** `stream.remote_output(address)` and `remote_input(address)` take the address
  string, right-anchored — `"main/camera/video"` another stream here, `"rig/main/camera/video"`
  another machine — each chunk cast; one casting to empty, `.` or `..` refused where written.

## ADDED: §Distribution — the Linux installer and the Mac app

- **Linux.** Each release attaches `tatolab-runtime-<version>-x86_64-linux.tar.gz` (built in
  `manylinux_2_28`; the portability gate runs over it). `curl -fsSL
  https://tatolab.github.io/streamlib/install.sh | sh` unpacks it to
  `~/.local/share/tatolab/<version>/`, points `current` at it, links `tatolab` and `tatolabd` into
  `~/.local/bin`, and writes `~/.config/systemd/user/tatolabd.service` (`Restart=on-failure`,
  `WantedBy=default.target`, `After=graphical-session.target`) — always on, across logouts with
  `loginctl enable-linger`, which it offers. A display node opened with no display in the
  runtime's environment takes `DISPLAY` and `WAYLAND_DISPLAY` from the user manager's, which
  desktop sessions import at login, and is refused by name when there is none. Re-running upgrades in place
  and restarts the service; `--uninstall` removes the service and files, never the state directory.
  On macOS the script refuses, pointing at the app.
- **macOS.** `Tatolab.app`, from a notarised `.dmg` on the release and `brew install --cask
  tatolab/tap/tatolab`: `Contents/MacOS/tatolabd`, an `SMAppService.agent` (plist in
  `Contents/Library/LaunchAgents/`, `KeepAlive` on a failed exit), and the lend laid out by Apple's
  code rules — native code under `Contents/Frameworks/`, the rest under `Contents/Resources/`,
  linked into one `tatolab/runtime/` tree, PyInstaller's layout, `dladdr` proved to find
  `_vulkan_driver/` through the link. Registration is the login item, so one switch, "Run
  Tatolab", covers both; opening the app registers the agent unless the user turned the switch
  off, which it remembers, and on `.requiresApproval` opens Login Items for the user. The menu shows whether
  the runtime is up and its stream count (`list_streams`); Quit quits the app. First launch links
  `tatolab` into `/usr/local/bin` and creates the lock's directory behind one administrator
  prompt. `Info.plist`
  carries the camera, microphone, local-network and Documents, Desktop and Downloads usage
  strings; the app and `tatolabd` carry the hardened-runtime device entitlements; every Mach-O is
  signed with the Developer ID and the `.dmg` notarised and stapled, with control-tower's desktop
  workflow steps and six `APPLE_*` secrets copied here. Discovery retries after a local-network
  denial. The prompt's wording under `SMAppService` is §Media I/O's acceptance check.

## MODIFIED: in-flight change files

- package-split-and-lend: `tatolabd` finds the lend at `../lib/tatolab/lend` or, inside the
  bundle, through the layout above. stream-graph: the builder's remote references take an address
  string; its names take decision 3's cast and `-2` (re-spelled in that file).

## MODIFIED: records re-spelled at the fold

§Product `:122-149` (the tools); §Processor model `:1373-1388` (decision 3), `:1424-1436`,
`:1443-1455`; §Media I/O's header, `:1914-1929` (the camera id); §Networking `:3565-3592`
(claiming), `:3597-3616`, `:3620-3638`, `:3653-3695`, `:3774-3899`, `:3982-4014` (`nodes` gone),
`:4061-4087`; §Control plane `:4361-4408` (no
`shutdown`), `:4442-4458` (the verbs), `:4462-4481` (registry gone; the state directory holds the
runtime's own log); the pivot ADR's steps 4 and 10.
`docs/architecture/` and the README in the shipping tickets.

## Inventory — what the old shape leaves, and the slice that ends it

| Old shape | Ends | Slice |
|---|---|---|
| `PUBSUB`'s id-less events, `RUNTIME_GLOBAL`; `PROCESSOR_REGISTRY` per process; the global shutdown funnel; the watchdog's process end; untagged group slots; first-wins logging | per stream | S1 |
| `APP_ENTRY_DIRECTORY_CAPTURED_BY_THE_LANGUAGE_HOST`, `STREAMLIB_APP_DIRECTORY`, the virtual camera's app-directory id; `get_streamlib_home()`'s cwd | the stream's project directory | S1 |
| `GpuContext` per `start()`; a surface service per `Runner`; the `ApiServer` processor, `control_plane_host.rs`, one `RuntimeOperations` | once per engine | S1 |
| `*ThisRuntimesGraph` | `*ThisStreamsGraph` | S1 |
| `tatolabd --stream-graph`; the CLI's compile and spawn; #2592's harness, fixtures and macOS done-proof starting `tatolabd` through `run` | `run_stream`; the harness starts `tatolabd` | S2 |
| `node_registry.rs`, `nodes`, `--node`, `local-api-<runtime_id>.sock`; `shutdown`; the Quit item; `run`/`dev` mesh flags | one socket; `set` | S2 |
| `runtime_name.rs`, `STREAMLIB_RUNTIME_NAME`, `--runtime-name`, the duplicate check | `machine_name.rs`, `machine.json` | S3 |
| Three-part `MeshPortAddress`, `@runtime` keys, every `*runtime_name*` key in `graph`, MCP, requests, ingress, egress; `remote_output(runtime_name, node, port)` | four parts | S3 |
| A `meshlink-` ingress per reader; the mesh fixtures and rigs naming runtimes | one per machine; two machines or two streams | S3 |
| `.claude/` skills naming `nodes`, `--node`, runtime names | one operating-model PR | after S3 |

## Left to later changes

| Not here | Because | Lands with |
|---|---|---|
| The app's stream views, in-place updates, the remaining Rust names | the app | step 10 |
| Starting with no GPU | accelerators OPEN | step 5 |
| `run <url-or-zip>`, registries | packs OPEN | step 7 |
| `machines`, router mode, dialing relays, the relay role, the URL forms | their OPENs | steps 8, 9 |

## Assumptions stated, not asked

- **One engine, a table of streams**, not a stream id on every graph node.
- **Pinning a crash**: a node's threads, those it spawns and its escalate worker carry its stream;
  alternate-stack signal handlers and a hook on an escaping panic write the crashing thread's
  stream through a file opened at start. OS-owned threads, SIGKILL and an OOM kill implicate none.
- **A stream's processes inherit the runtime's environment minus every `PYTHON*` variable**; `PYTHONPATH` is the lend then the project.
- **The runtime compiles**, in the project's interpreter, so an agent loads exactly as the CLI does.
- **Attached is a connection's lifetime**: an agent's attached stream ends when its host restarts `mcp`.
- **The owner's `expose` wins** over the function's `exposed` (the glossary's suggestion/decision).
- **The integration suite's `tatolabd` is built with a test-only feature** moving lock, state and
  runtime directories under a short `/tmp` root (socket paths stay under 104 bytes); the release
  build has none — no second mode.
- **A machine id per state directory**, not `/etc/machine-id`, so a container is its own machine.
- **Machine settings apply at the next start**: a rename re-addresses everything, so never live.
- **The app is Tauri**, as control-tower's desktop app is, registering its agent through
  `objc2-service-management`; a native AppKit menu would be smaller, and switching rewrites the menu.
  The cask's tap is `tatolab/homebrew-tap`, created by the owner, bumped per release.

## Tickets, each deleting what it replaces, tests included

Derived 2026-10-02, milestone #58; "(ultracode)": `/implement` builds it only with ultracode on.
- **S1 — one engine, many streams:** #2604 (ultracode). Blocked by #2592.
- **S2, split in two — the stream actions:** #2605 (ultracode; blocked by #2604, #2593); then the
  failed state and the crash recorder: #2606 (blocked by #2605).
- **S3 — the machine segment:** #2607 (ultracode). Blocked by #2605, #2566, #2567 (the shared cast
  fixture); it takes `APP_ENTRY_DIRECTORY_CAPTURED_BY_THE_LANGUAGE_HOST` and `STREAMLIB_APP_DIRECTORY`
  from S1, since the runtime-name default reading them dies here.
- **S4 — Linux:** #2608. **S5 — `Tatolab.app`:** #2609 (ultracode; the owner copies the secrets and
  creates the tap first). Both blocked by #2605.
- Operating-model PR: #2610, the live-ops skills (after #2607, #2594).

## REMOVED

- REMOVED: runtime/streamlib-engine/src/core/runtime/runtime_name.rs
- REMOVED: STREAMLIB_RUNTIME_NAME
- REMOVED: --runtime-name
- REMOVED: duplicate_runtime_name_on_the_mesh
- REMOVED: this_runtimes_name_on_the_mesh
- REMOVED: RUNTIME_GLOBAL
- REMOVED: APP_ENTRY_DIRECTORY_CAPTURED_BY_THE_LANGUAGE_HOST
- REMOVED: STREAMLIB_APP_DIRECTORY
- REMOVED: runtime/streamlib-api-server/src/node_registry.rs
- REMOVED: runtime/streamlib-api-server/src/control_plane_host.rs
- REMOVED: NODE_REGISTRY_SCHEMA_VERSION
- REMOVED: AnnouncedRuntimeIdentity
- REMOVED: OutputPortsInThisRuntimesGraph
- REMOVED: LinkRequestsAppliedIntoThisRuntimesGraph
- REMOVED: runtime/streamlib-engine/src/core/runtime/output_ports_in_this_runtimes_graph.rs
- REMOVED: runtime/streamlib-engine/src/core/runtime/link_requests_applied_into_this_runtimes_graph.rs
- REMOVED: input_runtime_name
- REMOVED: created_by_runtime_name
- REMOVED: requester_runtime_name
- REMOVED: reader_runtime_names
- REMOVED: link_requests_awaiting_runtime
- REMOVED: from_runtime_name
- REMOVED: to_runtime_name
- REMOVED: --stream-graph
- REMOVED: tatolab nodes
- REMOVED: RUNTIME_SHUTDOWN_REQUESTED_STATUS
- REMOVED: api/runtime/shutdown
- REMOVED: call_shutdown
- REMOVED: QuitMenuItemRequestsRuntimeShutdown

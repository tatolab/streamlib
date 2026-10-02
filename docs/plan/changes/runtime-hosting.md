# runtime-hosting

Step 4 of the one-runtime-per-machine pivot: one runtime per machine runs many streams, keeps the
ones it was told to keep, is addressed by its machine, and arrives from an installer. After it:
- `tatolabd` takes no stream on its command line. It is the machine's one runtime process — one
  engine, one GPU context, one Zenoh session, one local API on one socket — and any number of
  streams load into it and leave it without touching each other;
- a stream has its own graph and bookkeeping inside that engine: events, node registry,
  interpreter, log file, shutdown, teardown watchdog, process-group table, exposures;
- `run`, `run -d`, `stop`, `start`, `rm`, `streams`, `expose` and `dev` drive the running runtime,
  and each is a local-API tool an agent can call; with no runtime running every one fails at the
  socket; `graph` returns every stream;
- a kept stream is recorded in the state directory and comes back on every start, a crash's
  restart included; an attached one lives as long as its terminal command;
- an address is `<machine>/<stream>/<node>/<port>`, right-anchored; the runtime name is gone; a
  machine name clash on the mesh takes the next unused `-2`, `-3`… once and keeps it;
- the release attaches a runtime unit per platform; `curl | sh` and a brew tap install it, and on
  Linux the installer registers `tatolabd` as a systemd user service.

**Scale gate — this skill, plus the existing ADR.** The processor model (registries, shutdown and
spawn become per stream), the Python API's public contract (the builder's remote references), the
wire (the mesh keys gain a chunk, the announcement a machine id) and the local API's tool set all
move. The rationale is `docs/decisions/runtime-hosting.md` (#2580, #2600), which this PR extends
with decision 1 and the assumptions' rejected alternatives once decided.

**Precondition.** Every entry built is DECIDED: §Product `ARCHITECTURE.md:107-113` (required,
installer-shipped), `:114-121` (who starts it), `:122-143` (loaded and kept; stop, start, rm; one
user), `:152-157` (composition, flat graph); §Processor model `:1389-1402` (graph is data, emitted),
`:1403-1407` (environment beside the graph), `:1415-1425` (several streams, the split),
`:1434-1446` (failure isolation, the state directory); §Media I/O `:3202-3213` (Apple: terminal
until the app); §Networking `:4018-4022` (the runtime decides what leaves), `:4052-4071` (the
address and its collisions), `:4072-4079` (exposure); §Distribution `:4209-4219` (one version);
§Control plane `:4399-4415` (the graph's words), `:4559-4567` (one socket), `:4580-4585` (`graph`
and the stream tools). Not built against: needs `:1408-1414`, resources `:1428-1433`, accelerators
optional `:1882-1892` (the GPU still initialises at start), packs `:578-597`, discovery `:4080-4096`,
the stream map `:4097-4104`, the URL grammar and forms `:4109-4120`, the relay role, the control
client. **Sequencing:** after #2592 and #2593 (native `tatolabd` and `tatolab`) and #2566
(exposure). Built on step 3's tree, cited below from today's.

**Verified against the tree 2026-10-02 (HEAD 65e6c53a7).** Three read-only sweeps. `E` =
`runtime/streamlib-engine/src`, `A` = `runtime/streamlib-api-server`.
- **What collides between two streams in one process.** `PUBSUB` is one static
  (`E/core/pubsub/bus.rs:13`); runtime events carry no id (`events.rs:15`, `:55-69`, `:170`,
  `:317`), so every graph-change listener commits on any change (`graph_change_listener.rs:38-63`,
  no-op cross-talk) and one shutdown stops every run loop (`runtime.rs:1209-1226`, `:1558-1561`;
  `Runner::request_runtime_shutdown` ignores `self`, `:1304`). Also process-global:
  `PROCESSOR_REGISTRY` and its resolver (`processor_instance_factory.rs:232-247`, `:461`); the
  shutdown escalation (`runtime_shutdown_request.rs:62`, `:76-85`); the teardown watchdog, which
  ends the process with 124 (`engine_teardown_watchdog.rs:22-94`, `end_the_process_at_once.rs:40-51`);
  `REGISTERED_HELPER_PROCESS_GROUP_IDS` (`helper_process_group_registry.rs:37`);
  `APP_ENTRY_DIRECTORY_CAPTURED_BY_THE_LANGUAGE_HOST` (`core/app_directory.rs:19`); the logging
  install, first caller wins, a second getting no file (`logging/init.rs:48-56`, `:102`) and
  records stamped with the first id (`worker.rs:77-98`). A `GpuContext` is made per `start()`
  (`runtime.rs:565`) and `VULKAN_DEVICE_FOR_IMPORT` is first-wins (`vulkan/rhi/vulkan_buffer.rs:27`)
  — the NVIDIA dual-device crash (`docs/learnings/nvidia-dual-vulkan-device-crash.md`). Signals,
  init hooks, the window pump, device probes and the clock identity are correctly machine-wide.
- **The seam.** `Runner` (`runtime.rs:140-230`) owns one `Compiler`, which owns the one graph,
  its transaction and commit lock (`compiler.rs:29-40`); thread runners are graph components, so
  the executor is the graph. `Compiler::commit(&Arc<RuntimeContext>)` (`compiler.rs:93`) already
  takes its context. Wired to one graph: the mesh's offer and link-request answers
  (`runtime.rs:391-393`, `:452-456`), and the local API, which is an `ApiServer` processor inside
  the graph holding one `RuntimeOperations` (`A/src/control_plane_host.rs:24-47`, `state.rs:14`),
  implemented by `Runner` itself.
- **Channels.** Local channels are `{processor_id}/{port}` on cuid2 ids, unique across graphs
  (`E/iceoryx2/channel_name.rs:198-215`). A mesh ingress is `meshlink-<fnv64 of the address>/bags`
  (`:242-248`) with one publisher per channel (`iceoryx2/node.rs:321`): two streams reading one
  remote port would collide. The surface service already tags surfaces by owner and releases by
  owner (`E/linux/surface_share/state.rs:26`, `:450-493`).
- **Names.** The runtime name is defined, defaulted from `<host>-<app dir>-<id>` and validated in
  `E/core/runtime/runtime_name.rs:25-180`; ~800 lines across engine, api-server, wheel, tests and
  docs name it. Every Zenoh key is built in `runtime_mesh_key.rs:28-326`, parsed by fixed chunk
  position (`:164-184`, `:304-319`). Host identity is boot-scoped (`host_identity.rs:20-107`); no
  persistent machine id exists. The duplicate check queries before declaring, 2 s ceiling, a
  failed query read as no holder, takeover on same host and gone pid
  (`duplicate_runtime_name_on_the_mesh.rs:34-161`).
- **Disk.** Nothing persistent per user exists. Logs go to `<home>/.streamlib/logs/<runtime_id>-<ms>.jsonl`
  (`E/core/logging/paths.rs:16-23`; the comments at `init.rs:120` and `event.rs:5` naming
  `XDG_STATE_HOME` are wrong). The registry is one JSON per live runtime under `<runtime dir>/nodes/`
  (`A/src/node_registry.rs:28-64`), written by the `ApiServer` processor (`A/processors/api_server.rs:164-216`).
- **Tools.** `graph`, `tap`, `logs`, `exchange`, `shutdown`, `add_processor`, `remove_processor`,
  `connect`, `disconnect` (`A/src/mcp.rs:275-427`); `shutdown` reaches the global funnel
  (`:590-604`, `A/src/handlers.rs:179-215`). stream-graph and local-api re-spell them (#2565,
  `ARCHITECTURE.md:4399-4415`).
- **Release.** Wheels only, built in `manylinux_2_28` and on `macos-15`, ad hoc signed, uploaded to
  the GitHub release (`release-wheel.yml:61-269`, `macos-wheel.yml:37-198`); the Pages index
  reads `.whl` only (`scripts/build_simple_index.py:31-86`). No install script, formula, unit or
  plist exists anywhere.

---

## [NEEDS DECISION] 1 — what happens to the `shutdown` tool

Today any caller of the local API can end the runtime. With one runtime holding many people's
streams, that ends all of them; under the Linux service a clean exit stays down until the next
login, and on a Mac until someone restarts the terminal. Neither Docker nor Tailscale has a client
command that stops its daemon (`tailscale down` disconnects; `tailscaled` keeps running).

- **(a) Retire it.** The `shutdown` tool and `POST /api/runtime/shutdown` go; `stop <stream>` is
  how a client ends work. The runtime stops by its service manager (`systemctl --user stop`), the
  app, or a signal in its terminal.
- **(b) Keep it, machine-wide.** Any caller ends every stream; the service does not restart it.
- **(c) Keep it as a restart.** The runtime exits and the service manager restarts it, re-loading
  kept streams; attached ones end. On a Mac with no app, nothing restarts it.

**Recommendation: (a).** It leaves no client able to end other people's streams, and the
vocabulary still covers every stream action.

---

## Target layout

```
runtime/tatolabd/                      the machine's runtime process; no stream arguments
runtime/tatolab-cli/                   tatolab: run, run -d, stop, start, rm, streams, expose, dev, set …
runtime/streamlib-engine/src/core/runtime/
  runtime.rs                           Runner: the one engine — GPU context, iceoryx2 node, tokio,
                                         mesh, surface service, signals; loaded streams by name
  loaded_stream.rs                     LoadedStreamInThisRuntime: compiler + graph, status, events,
                                         registry, environment, logs, shutdown, watchdog, groups, exposures
  machine_state_directory.rs           the state directory: machine record, kept streams, logs
  machine_name.rs                      replaces runtime_name.rs
runtime/streamlib-api-server/          hosted by the Runner beside the streams, never a node in one
installer/install.sh                   curl | sh, served from the Pages site
installer/tatolabd.service             the systemd user unit the script installs
installer/homebrew/tatolab.rb          the formula, published to tatolab/homebrew-tap per release
```

## ADDED: §Processor model — one engine, many streams

- **`Runner` is the engine, one per process**: a second `Runner` in one process is refused by name.
  It makes the `GpuContext` once at start (so `VULKAN_DEVICE_FOR_IMPORT` names the only device),
  the iceoryx2 node, one tokio runtime, the mesh membership, one surface service whose owner key
  is the stream, signal ownership, and hosts the local API itself — the `ApiServer` processor and
  its graph node are deleted.
- **`LoadedStreamInThisRuntime`** per stream, keyed by stream name: its `Compiler` (graph,
  transaction, commit lock, abandoned threads), `RuntimeStatus`, graph-change listener, and a
  `RuntimeContext` sharing the engine's GPU, node and tokio handle whose `runtime_ops` is the
  stream. Events carry their stream: the topic is per stream, so a listener hears only its own
  stream's graph change and shutdown. The registry: built-ins once for the machine; Python
  descriptors per stream, described in that stream's interpreter (step 3's describe). The
  interpreter, project and working directory are the stream's environment. Logs: one subscriber
  for the process, records routed by stream to that stream's file. Shutdown: a per-stream funnel
  and escalation; the machine's ladder walks every stream at once. The teardown watchdog is per
  stream: on expiry it kills that stream's process groups, abandons its threads, marks it failed
  and reports it; only a machine shutdown ends the process. The process-group table and the
  exposure set are per stream.
- **Links between streams on one machine** ride iceoryx2 as a link inside one stream does, with no
  exposure. The input's stream owns the link, resolving the source in the engine's table of
  loaded streams; a source stream not loaded leaves it `awaiting_remote`, reason naming the
  stream, and it wires when that stream loads — the mesh's states, reused.
- **A remote port read by several streams** has one ingress for the machine — one network copy,
  one publisher — and every reading stream subscribes to it.

## ADDED: §Product — `tatolabd`, the state directory, the verbs

- **`tatolabd`** takes no stream. At start: take the machine lock; open the state directory;
  resolve the machine name (§Networking below); join the mesh; serve the local API at
  `<runtime dir>/local-api.sock`; re-load every kept, not-stopped stream. A kept stream whose
  project or interpreter is gone is reported by name and kept, never deleted. It never detaches.
  On macOS it finds its Vulkan loader and MoltenVK manifest relative to its executable, in the
  lend, and names the manifest in `VK_ADD_DRIVER_FILES` before the first instance, unless the user
  chose drivers.
- **The machine lock** is `/tmp/tatolab-runtime.lock`, `flock`ed for life, holding the owner's uid
  and pid. A second `tatolabd` — another user's or the same user's — is refused naming the holder.
  A CLI that finds no socket but a held lock says whose runtime this machine runs.
- **The state directory** is `$XDG_STATE_HOME/tatolab/` (else `~/.local/state/tatolab/`) on
  Linux, `~/Library/Application Support/Tatolab/` on macOS. It holds `machine.json` (the machine
  id, the recorded machine name, the mesh settings), `streams/<stream>.json` per kept stream (the
  graph compiled at its load, the environment, `stopped`, the owner's exposure rulings) and
  `logs/` (one JSONL per stream per load, and the runtime's own). Logs leave the project's
  `.streamlib/`; its shader cache stays.
- **Loading.** `run_stream {project_directory, stream, name, keep}`: the runtime finds
  `<project>/.venv/bin/python` (absent → refused, pointing at `uv sync`), runs `tatolab.stream`'s
  compile entry in it — compiling stays in the project's interpreter, never the runtime process —
  describes the Python types, and loads. A name already loaded is refused naming the project it
  came from, `--name` the way out; a `keep` load of the same project and function replaces the
  recorded stream, which is how a changed source is picked up.
- **Attached is a held connection.** `tatolab run` makes the call on an upgraded connection
  (`/streams/attach`, the `/mcp/stdio` pattern) that carries the stream's log records back; the
  stream unloads when that connection closes — Ctrl-C, a closed terminal, a killed CLI. A runtime
  crash closes it, and `run` exits 1 naming the crash and the runtime's log. A one-shot call
  (`POST /mcp`) can only keep.
- **The verbs**, each one tool: `run` / `run -d` → `run_stream`; `stop` → `stop_stream` (unload,
  record `stopped`); `start` → `start_stream` (re-load the record); `rm` → `remove_stream`
  (unload, forget); `streams` → `list_streams` (name, attached / kept / stopped, project, node
  count); `expose` / `expose --remove` → `expose_port` (the owner's ruling on one port, recorded
  for a kept stream; the function's `exposed` is the default for any port without one); `dev` is
  `run` plus a watch that re-loads on save and, after a crash, waits for the runtime and loads
  again; `set --machine-name`, `--mesh-name`, `--mesh-peer`, `--mesh-listen`,
  `--no-mesh-multicast-discovery` → `machine.json`, applied at the next start, said so.
- **Restart.** Linux's unit restarts `tatolabd` on failure; every kept stream comes back. The
  acceptance criterion (#2559's record): on the rig, `kill -9` under the unit, and the kept
  scaffold stream shows frames again within 10 s, measured and recorded in the PR.

## ADDED: §Control plane — every tool names its stream

- `graph {stream?}`: with a stream, that stream's one-shape graph, loadable; with none,
  `{machine, streams: [<one-shape graph>…], mesh}`. `add_node`, `remove_node`, `connect`,
  `disconnect`, `tap` and `logs` take `stream`. A link end is right-anchored: `{node, port}` in the
  stream, `{stream, node, port}` on this machine, `{machine, stream, node, port}` on another;
  `connect` takes `<end>_machine`, `<end>_stream`, `<end>_node`, `<end>_port`, leading parts
  omitted meaning here. `tap`'s channel is an address. Instructions and prompts follow.
- `nodes` and `--node` are deleted with the registry: one socket at a fixed path. `graph.mesh`
  still lists peers; the discovery verbs are the discovery OPEN's.

## MODIFIED: §Networking `:3588-3610`, `:3644-3691`, `:3765-3790`, `:4052-4071` — the machine segment

- **`MeshPortAddress`** gains the stream: `<machine>/<stream>/<node>/<port>`, the chunk grammar and
  `runtime_mesh_key.rs` each changed in one place; the token key `streamlib/<mesh>/@machine/<machine>/<machine id>/<pid>`;
  offered ports, readers, link requests, egress and data keys gain the stream under the machine.
  `InboundLinkName` and the ingress hash follow.
- **The machine id** is minted once into `machine.json` (128 random bits) and carried on the token
  beside the host identity, so a restart — a reboot included — reclaims its own name through a
  router. **The name**: the recorded one, else the hostname, chunk-checked. A live holder with
  another machine id moves to the next unused `<name>-2`, `-3`…, queried in turn, recorded, and
  said once with `tatolab set --machine-name`; a holder with this machine id and a gone pid is
  taken over. A failed query is never read as free: the runtime runs local-only and retries the
  claim at its next start. Two machines claiming one name inside a discovery window both keep it,
  each says so naming the other, and a link to that name is `error` naming both — today's
  residual, unchanged.
- **The builder.** `stream.remote_output(address)` and `remote_input(address)` take the address
  string, right-anchored — `"main/camera/video"` is another stream here, `"rig/main/camera/video"`
  another machine — and refuse a chunk the grammar refuses where the author wrote it.

## ADDED: §Distribution — the runtime unit and the minimal installer

- **The unit.** Each release attaches `tatolab-runtime-<version>-x86_64-linux.tar.gz` (built in
  `manylinux_2_28`, the wheel's glibc floor) and `…-aarch64-darwin.tar.gz` (`macos-15`, 15.0,
  ad hoc signed, signatures checked in the tarball) — `cargo xtask build-runtime`'s prefix, at the
  repository's one version. The portability gate runs over the unit.
- **`curl -fsSL https://tatolab.github.io/streamlib/install.sh | sh`** installs the newest
  release's unit into `~/.local/share/tatolab/<version>/` and links `tatolab`, `tatolabd` into
  `~/.local/bin`, keeping `bin/` and `lib/` siblings. On Linux it writes
  `~/.config/systemd/user/tatolabd.service` (`Restart=on-failure`, `WantedBy=default.target`,
  after `graphical-session.target` so windows find the display), runs `systemctl --user enable
  --now`, and prints `loginctl enable-linger` for a machine with no login. On macOS it prints
  "run `tatolabd` in a terminal" — the app registers the service later (§Media I/O). `--uninstall`
  stops and removes the unit and the unit files, never the state directory.
- **`brew install tatolab/tap/tatolab`** installs the same tarball (keg in `libexec`, `bin/`
  linked); its `post_install` and caveats do on each platform what the script does.

## MODIFIED: records re-spelled at the fold

- §Product `:122-143` gains the verbs' tools; §Processor model `:1415-1425` the per-stream table;
  `:1434-1446` "persisted graphs" → the state directory. §Networking `:3588-3610` (session config
  from `machine.json`, flags, environment), `:3644-3691` (the runtime name superseded by the machine
  name), `:3765-3790` (the builder's address string; MCP ends). §Control plane `:4345-4398` (the
  tools; `shutdown` per decision 1), `:4446-4466` (registry gone; the state directory beside the
  runtime directory; logs moved). `docs/architecture/` and README's install and quickstart in the
  shipping tickets.

## Inventory — what the old shape leaves, and the slice that ends it

| Old shape | Ends | Slice |
|---|---|---|
| `PUBSUB`'s id-less runtime events, `RUNTIME_GLOBAL` | per-stream topic | S1 |
| `PROCESSOR_REGISTRY` per process; the global shutdown funnel and escalation; the watchdog's process end; `REGISTERED_HELPER_PROCESS_GROUP_IDS`; `APP_ENTRY_DIRECTORY_CAPTURED_BY_THE_LANGUAGE_HOST`; first-wins logging | per stream, or deleted | S1 |
| `GpuContext` per `start()`; one surface service per `Runner` | once per engine | S1 |
| The `ApiServer` processor, `control_plane_host.rs`, `AppState`'s one `RuntimeOperations` | the engine hosts the local API | S1 |
| `tatolabd --stream-graph --project --interpreter`; the CLI's own compile and spawn (#2592) | `run_stream`, the held connection | S2 |
| `node_registry.rs`, `nodes`, `--node`, `local-api-<runtime_id>.sock` | one fixed socket | S2 |
| Logs under `.streamlib/logs` | the state directory | S2 |
| `runtime_name.rs`, `STREAMLIB_RUNTIME_NAME`, `--runtime-name`, the default from the app directory, `duplicate_runtime_name_on_the_mesh.rs` | `machine_name.rs`, `machine.json` | S3 |
| Three-part `MeshPortAddress`, every `@runtime` key, `*_runtime_name` in `graph`, MCP, link requests, ingress and egress; `remote_output(runtime_name, node, port)` | four parts, right-anchored | S3 |
| `meshlink-` ingress per reader | one per machine | S3 |
| Mesh fixtures and rigs naming runtimes (`runtime_mesh_two_processes`, `cross_runtime_link*`, `verify_cross_runtime_link_requests.sh`, `wire_two_runtimes_over_mcp.py`, `cross_runtime_link_rig.rs`) | two machines' runtimes, or two streams on one | S3 |
| `.claude/` skills naming `nodes`, `--node`, runtime names | one operating-model PR | after S3 |

## Left to later changes

| Not here | Because | Lands with |
|---|---|---|
| The app registering the Apple service; Apple permissions credited to Tatolab | the app | step 10 |
| Starting with no GPU | accelerators OPEN | step 5 |
| `machines`, `streams --machine`; router mode and dialing relays (today's five mesh settings only move to `machine.json`) | discovery and stream-map OPENs | step 8 |
| `run <url-or-zip>`; registries | packs OPEN | step 7 |
| The relay role; the URL forms | their OPENs | steps 8 and 9 |
| A Linux aarch64 unit (Jetson, robots) | no aarch64 Linux build exists | its own change |
| A shared, system-wide runtime | not built until needed (`:122-143`) | — |

## Assumptions stated, not asked

- **One engine, a table of streams** — a `LoadedStreamInThisRuntime` per stream inside the one
  `Runner`, rather than a stream id on every graph node; the per-graph machinery already exists.
- **Compiling moves from the CLI to the runtime**, spawned in the project's interpreter, so an
  agent loads a stream with the same tool the CLI uses; the CLI compiles nothing.
- **A held connection is what "attached" means**; a stateless call can only keep.
- **The owner's `expose` is recorded per port and wins over the function's `exposed`**, which stays
  the default — the glossary's "the author's suggestion, the owner's decision".
- **The lock is `/tmp/tatolab-runtime.lock`.** Tests, CI and a developer running a second build use
  `tatolabd --machine-root <dir>`, which moves lock, state and runtime directories under `<dir>`,
  as `dockerd --data-root --exec-root` does; where it bites: a forgotten flag in a test hits the
  machine lock, refused by name.
- **The state directory and the lock path** follow XDG and Apple conventions.
- **The machine id** is random and per state directory, not `/etc/machine-id`, so a container is
  its own machine.
- **`machine.json` settings apply at the next start**; changing a machine's name renames every
  address it serves, so it never happens live.
- **The installer lives in this repository; the tap is `tatolab/homebrew-tap`**, created by the
  owner and bumped per release by the release workflow.
- **`dev`'s watch** polls the project's `.py` files with no new dependency.

## Slices, each deleting what it replaces, tests included

- **S1 — one engine, many streams** (runtime suite: two graphs loaded into one `Runner`, one's
  shutdown, crash budget and graph change leaving the other alone; one `VkDevice`). After #2592.
- **S2 — the stream actions**: `tatolabd` without a stream, the lock, the state directory, the
  tools and verbs, the held connection, re-load and restart, per-stream logs, `nodes` gone.
  Blocked by S1, #2593.
- **S3 — the machine segment**: four-part addresses and keys, the machine id and name, the suffix,
  `set`, cross-stream links, one ingress per machine, the builder's address string, the mesh
  fixtures. Blocked by S2, #2566.
- **S4 — the unit and the installer**: release tarballs, `install.sh`, the unit, the formula, the
  restart criterion on the rig. Blocked by S2.

## REMOVED

- REMOVED: runtime/streamlib-engine/src/core/runtime/runtime_name.rs
- REMOVED: STREAMLIB_RUNTIME_NAME
- REMOVED: --runtime-name
- REMOVED: duplicate_runtime_name_on_the_mesh
- REMOVED: RUNTIME_GLOBAL
- REMOVED: REGISTERED_HELPER_PROCESS_GROUP_IDS
- REMOVED: APP_ENTRY_DIRECTORY_CAPTURED_BY_THE_LANGUAGE_HOST
- REMOVED: STREAMLIB_APP_DIRECTORY
- REMOVED: runtime/streamlib-api-server/src/node_registry.rs
- REMOVED: runtime/streamlib-api-server/src/control_plane_host.rs
- REMOVED: NODE_REGISTRY_SCHEMA_VERSION
- REMOVED: AnnouncedRuntimeIdentity
- REMOVED: input_runtime_name
- REMOVED: created_by_runtime_name
- REMOVED: requester_runtime_name
- REMOVED: reader_runtime_names
- REMOVED: from_runtime_name
- REMOVED: to_runtime_name
- REMOVED: --stream-graph
- REMOVED: tatolab nodes

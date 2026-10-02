# runtime-hosting

Step 3 of the one-runtime-per-machine pivot: one runtime per machine runs many streams. After this
change:
- `tatolabd` is the machine's runtime process. It runs the engine, the local API, the Zenoh
  session and the GPU context once, and imports nothing from any project;
- every stream is its own engine graph inside it: its own events, node registry, interpreter, log
  file, shutdown and processor-interpreter table. One stream's stop, crash budget or graph change
  never touches another's;
- `run` compiles a stream in its project's environment and loads it into the running runtime.
  `run -d` keeps it, and the runtime re-loads it on every start. `stop`, `streams` and `dev` work
  on the streams the runtime holds;
- an address is `<machine>/<stream>/<node>/<port>`. The machine is one runtime, owned by one user,
  and the runtime name is gone;
- streams on one machine link to each other without exposing anything.

**Scale gate — this skill, plus an ADR (`docs/decisions/runtime-hosting.md`, already written).**
What moves:
- the processor model, since registries, interpreters and shutdown become per stream;
- the Python API's public contract: the CLI's `run`/`dev`/`stop`/`streams`, `--runtime-name`
  gone, `--machine` added;
- the wire: the mesh key grammar gains the machine chunk, and the address gains a part;
- the local API's tool set.

**Precondition.** Every entry this delta builds is DECIDED:
- §Product `ARCHITECTURE.md:107-113` (the runtime is required), `:114-128` (loaded and kept; one
  runtime per machine, one owner), `:138-143` (composition);
- §Processor model `:1355-1371` (several streams in one process), `:1372-1384` (failure isolation;
  the state directory);
- §Networking `:3984-3999` (the address), `:4001-4008` (exposure; streams on one machine link
  freely);
- §Control plane `:4479-4483` (stream actions join the tools; one socket per machine).

Not built against:
- what the graph holds beyond nodes, links and exposures (`:1334`);
- the stream package's independence (the lend);
- packs;
- resources;
- the stream map;
- the URL grammar;
- the installer;
- the rename of the CLI and the distributions.

This change lands after stream-graph's `Runtime.load` (#2567) and the local API's socket (#2573),
both of which it builds on.

**Verified against the tree 2026-10-01 (HEAD 56d29b1c4).** Two read-only sweeps: how Python nodes
reach the engine, and every once-per-process piece and the CLI. `E` = `runtime/streamlib-engine/src/core`,
`W` = `sdk/streamlib-python-wheel/src`, `P` = `sdk/streamlib-python-wheel/python/streamlib`, `A` = `runtime/streamlib-api-server`.

**What is once per process today, and what a second engine collides on**
- **Events.** `PUBSUB` is one static (`E/pubsub/bus.rs:12-13`). Every runtime event goes to
  `RUNTIME_GLOBAL` with no runtime id (`E/pubsub/events.rs:10-27`, `:55-64`). So one runtime's
  graph change commits another's compiler (`E/runtime/graph_change_listener.rs:36-66`), and one
  runtime's shutdown stops every run loop (`runtime.rs:1209-1226`).
- **Registry.** `PROCESSOR_REGISTRY` is one static keyed by import path
  (`E/processors/processor_instance_factory.rs:220-248`). It is filled at decoration
  (`W/python_processor_registration.rs:131-155`), and the unknown-type resolver imports the class
  into the app process (`:178-209`, `W/python_runtime_lifecycle.rs:278-302`).
- **Interpreter.** `CAPTURED_LAUNCH_ENVIRONMENT` is a OnceLock read from `sys.executable` and
  `sys.path[0]` (`W/python_helper_process_spawn_host.rs:104-129`). Every helper is
  `<interpreter> -m streamlib._helper`, with the entry directory prepended to `PYTHONPATH` and no
  `current_dir` (`:226-290`).
- **Logs.** `logging::init` is first caller wins (`E/logging/init.rs:102`, `:146-176`). A second
  engine's records land in the first's `<runtime_id>-<ms>.jsonl` (`E/logging/paths.rs:16-24`),
  stamped with the first's id (`E/logging/worker.rs:78-98`).
- **Signals and shutdown.** `SHUTDOWN_SIGNALS_OWNED` refuses a second owner
  (`E/signals.rs:24-27`, `:193-199`). The escalation counter and `request_runtime_shutdown` are
  process-global (`E/runtime/runtime_shutdown_request.rs:59-84`), so the api-server's shutdown
  verb stops every engine (`A/src/handlers.rs:191-201`).
- **Teardown watchdog.** It `_exit`s the process after 15 s (`E/runtime/engine_teardown_watchdog.rs:21-101`,
  `end_the_process_at_once.rs:42-52`).
- **Helper groups.** `REGISTERED_HELPER_PROCESS_GROUP_IDS` is 1024 unowned slots, killed all at
  once (`E/runtime/helper_process_group_registry.rs:20-99`).
- **GPU.** `GpuContext::init_for_platform_sync` creates a device per `start()` (`runtime.rs:565`).
  `VULKAN_DEVICE_FOR_IMPORT` is first-wins (`vulkan/rhi/vulkan_buffer.rs:20-28`). A second
  `VkDevice` while the first has work crashes NVIDIA
  (`docs/learnings/nvidia-dual-vulkan-device-crash.md`).
- **Already per runtime:**
  - the Zenoh session (`E/runtime/mesh/runtime_mesh_membership.rs:133-175`);
  - the control plane (`A/src/control_plane_host.rs:24-47`);
  - the iceoryx2 node (`runtime.rs:363-367`);
  - the surface-share socket (`runtime.rs:1583-1641`).
- Nothing refuses a second `Runner` in one process. A test already runs two (`runtime.rs:2102-2126`).

**How Python nodes reach the engine**
- The decorator stamps `__streamlib_processor_*__` and registers a descriptor
  (`P/_processor_declaration.py:404-432`).
- `ProcessorDescriptor` is serde round-trippable (`sdk/streamlib-processor-schema/src/descriptors.rs:78-114`).
  The execution mode is a separate `ExecutionConfig` (`W/python_processor_declaration.rs:28-29`,
  `:106-133`).
- The constructor closure captures only the import path, the descriptor and the `ExecutionConfig`
  (`W/python_processor_registration.rs:87-115`).
- The parent never constructs a user class; only the helper does (`P/_helper.py:855-880`).
- No production path registers a Python descriptor from data. Only tests call
  `register_descriptor_only` that way.

**Identity, addresses, the CLI**
- `runtime_name`: a constructor argument, then `STREAMLIB_RUNTIME_NAME`, then a default of
  `<hostname>-<app dir>-<short id>` (`E/runtime/runtime_name.rs:24-183`). `--runtime-name` sets it
  (`P/cli.py:924-933`). It is unique on the mesh (`E/runtime/mesh/duplicate_runtime_name_on_the_mesh.rs:39-102`).
- `MeshPortAddress` has three parts (`E/graph/edges/mesh_port_address.rs:22-140`). Every Zenoh key
  is built in one file (`E/runtime/mesh/runtime_mesh_key.rs:28-230`). The chunk grammar is in one
  place (`E/runtime/mesh_address_chunk.rs:9-55`).
- `run` and `dev` are identical, and `dev` has no reload (`P/cli.py:879-976`, `:1414-1425`).
  `launch_app_node` executes the entry file, the engine and `run()` in the CLI's own process
  (`:216-293`).
- No state directory exists. The runtime directory is ephemeral (`E/runtime/streamlib_runtime_directory.rs:32-64`),
  and logs live under the project's `.streamlib/` (`E/streamlib_home.rs:18-45`).

---

## [NEEDS DECISION] 1 — what `tatolabd` is until the installer exists

The plan names the program and says the installer ships it, never pip (§Product `:107-113`). The
installer is the rename step's change, and the lend that lets a stream's environment borrow the
runtime's native part is step 4's. Until both exist, the runtime program has to come from
somewhere.

- **(a) A console script in today's wheel.** `tatolabd` is an entry point of the `streamlib`
  wheel: a Python process that builds the engine through PyO3, hosts the local API, and imports
  nothing from any project.
  - Each stream's environment installs the same wheel version, so the exact-build handshake passes
    as it does today.
  - The runtime ships through pip until the installer change. That is transitional debt against
    `:107-113`, never a design.
  - Nothing user-visible changes when the installer arrives: the same `tatolabd`, from the app or
    brew instead of pip.
- **(b) A native Rust binary now.**
  - The helper spawn host and the registration move out of the PyO3 crate into the engine
    (`W/python_helper_process_spawn_host.rs`, `W/python_processor_registration.rs` are crate-private
    today).
  - Python descriptors arrive only as data.
  - Each stream's environment must get the native part without the wheel. That is step 4's lend,
    and the installer to ship the binary. Both are pulled forward into this change.

**Recommendation: (a).** It is the smallest step that makes the shape real. (b) is where the
installer change ends up, and nothing in (a) has to be undone to get there.

---

## ADDED: §Processor model — one runtime, many stream engines

- **The split.** One machine-level owner holds what `:1355-1371` keeps once:
  - the `GpuContext`, created once and shared (no second `VkDevice`);
  - signal ownership and the escalation ladder;
  - the one Zenoh session;
  - the local API;
  - the surface-share service.

  Each stream is a `Runner` it owns. The per-stream pieces then stop being process-global:
  - **events** carry their stream, and a listener hears only its own stream's graph change and
    shutdown;
  - **the processor registry** is per stream;
  - **the interpreter** is per stream (the stream's `.venv/bin/python`) and the working directory
    is the project's;
  - **logs** go to one JSONL file per stream, `<stream>-<ms>.jsonl` under the state directory;
  - **shutdown** — the api-server's `shutdown` takes a stream;
  - **the teardown watchdog** is per stream. On expiry it kills that stream's helper groups and
    abandons the stream's threads. It ends the process only for a machine-level shutdown;
  - **the helper-group table** is per stream.
- **Python nodes without importing them.** At load, the runtime describes each Python node type
  by running `<stream interpreter> -m streamlib._describe <import path>…`. That prints the
  descriptors and execution configs as JSON. They go into the stream's registry with a
  constructor that needs no class object, which the closure never did. A live `add_processor` of
  a Python import path describes it in that stream's interpreter the same way. The
  resolver that imports into the parent is deleted, so the runtime process never imports a
  project, a pack or a user class.
- **Links between streams.** A link whose ends are two streams on this machine rides iceoryx2,
  exactly as a link inside one stream does, and needs no exposure. The engine chooses the
  transport from the link's ends, as it already does between local and mesh.
- **Failure isolation** stays as decided. A native crash ends the process and every stream, and
  the runtime re-loads the kept ones on its next start.

## ADDED: §Product — `tatolabd`, the state directory, and the verbs

- **`tatolabd`** per decision 1. It starts the machine-level owner, serves the local API on the one
  socket, re-loads kept streams, and runs until signalled. It never detaches itself (the service
  manager starts it). One per machine:
  - a machine-wide lock (`/tmp/tatolab-runtime.lock`, `flock`) is held for life;
  - a second `tatolabd`, under any user, is refused naming the holder's user and pid;
  - the mesh's duplicate-machine check backs this across machines.
- **The state directory.** `$XDG_STATE_HOME/tatolab/` on Linux (falling back to
  `~/.local/state/tatolab/`), and `~/Library/Application Support/Tatolab/` on macOS.
  - It holds `streams/<stream>.json` per kept stream: the graph compiled at load, the interpreter
    path, the project directory and the exposures.
  - It holds the per-stream logs.
  - A start re-loads every kept stream. A stream whose project or interpreter is gone is skipped
    and reported by name, never deleted.
- **The local API's tools**, beside today's:
  - `load_stream {graph, interpreter, project_directory, keep}` refuses a name already loaded,
    naming where the first came from;
  - `unload_stream {stream}` stops it and forgets it if kept;
  - `list_streams` returns each stream's name, kept or attached, project and node count.

  `graph`, `add_processor`, `remove_processor`, `connect`, `disconnect`, `tap` and `logs` take a
  `stream`. `graph` with none returns every stream.
- **The CLI** (still `streamlib` until the rename):
  - `run [entry]` compiles in the current environment (stream-graph's `compile_stream_to_graph`),
    calls `load_stream`, streams the stream's logs, and on Ctrl-C calls `unload_stream`;
  - `run -d` loads with `keep` and returns;
  - `stop <stream>` unloads;
  - `streams` lists;
  - `dev` is `run` plus a file watch that unloads and re-loads on save (`dev` reloads nothing
    today).

  No runtime running is an error naming how to start one. `run` never starts one (`:107-113`).
  `--runtime-name` is gone; `--name` names the stream (stream-graph).

## MODIFIED: §Networking `:3984-3999` — the machine segment

- `MeshPortAddress` becomes `<machine>/<stream>/<node>/<port>`. The chunk grammar and
  `runtime_mesh_key.rs` gain the machine chunk in their one place each.
  - The announcement is keyed by the machine: `streamlib/<mesh>/@machine/<machine>/…`.
  - The offered ports, readers, link requests and data keys gain the stream under the machine.
- The machine defaults to the hostname. `tatolabd --machine <name>` overrides it.
- `runtime_name.rs`, its default derivation, `STREAMLIB_RUNTIME_NAME` and `--runtime-name` are
  deleted. The duplicate check becomes a duplicate-machine check with the same takeover rule.
- `graph.mesh` lists machines; a peer lists the streams and ports it exposes. `nodes` becomes
  `streams` plus the mesh's machines.

## MODIFIED: §Control plane `:4479-4483` — one socket per machine

The socket is `<runtime dir>/local-api.sock`, one per machine. The registry's per-runtime entries
collapse to the one runtime's.

---

## Assumptions stated, not asked

- **Shape of the split.** One `Runner` per stream, under a machine-level owner, rather than a
  stream dimension on every graph node. Engine-wide shared state is hoisted, and the per-graph
  machinery already exists per `Runner`.
- **Descriptors come from describing in the stream's own interpreter,** not from the graph. Putting
  them in the graph is §Processor model's OPEN `:1334`, which this change does not build against.
- **The lock path and the state directory** follow the XDG and Apple conventions above.
- **`dev`'s file watch** uses the standard library's polling of the project's `.py` files, so it
  adds no dependency.

## Expected slices

- **S1 — per-stream events, registry, logs.** Events carry the stream, the registry is per
  stream, and logs are per stream. Several `Runner`s in one process stop crossing. Independent.
- **S2 — per-stream shutdown, watchdog, helper groups; the shared GPU context.** Blocked by S1.
- **S3 — `tatolabd` and the stream tools.** Decision 1, the machine owner (Zenoh, local API,
  surface service once), the lock, `load_stream`/`unload_stream`/`list_streams`, describe-at-load,
  the resolver deleted, cross-stream links. Blocked by S2, #2567 and #2573.
- **S4 — the CLI and the state directory.** `run`, `run -d`, `stop`, `streams`, `dev` with reload,
  and re-load at start. Blocked by S3.
- **S5 — the machine segment.** Four-part addresses, the keys, `--machine`, the runtime name
  deleted, the mesh fixtures. Blocked by S3.

## REMOVED

- REMOVED: runtime/streamlib-engine/src/core/runtime/runtime_name.rs
- REMOVED: STREAMLIB_RUNTIME_NAME
- REMOVED: --runtime-name
- REMOVED: duplicate_runtime_name_on_the_mesh
- REMOVED: CAPTURED_LAUNCH_ENVIRONMENT
- REMOVED: register_processor_class_by_import_path
- REMOVED: launch_app_node
- REMOVED: execute_app_entry_file
- REMOVED: RUNTIME_GLOBAL
- REMOVED: REGISTERED_HELPER_PROCESS_GROUP_IDS

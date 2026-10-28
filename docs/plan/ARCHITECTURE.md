# StreamLib Architecture Plan

The single source of architectural decisions. Sessions implement this plan; they do not make
architecture. A decision missing here stops work and comes back to the owner — it is never
inferred from existing code, consumers, or history. This document and the diagrams under
`diagrams/` (Mermaid `.mmd`, the committed source — Excalidraw files are generated views,
never round-tripped back) move together: every DECIDED entry is represented in the diagram.

Legend: **DECIDED** — build exactly this. **OPEN** — do not build; needs an owner decision.

Reading rule since the 2026-09-30 pivot (`[one-runtime-per-machine]`): the glossary's vocabulary
is the plan's. Until the rename change re-spells older entries, read them through it — "app" as
**stream**, "processor" on a user surface as **node**, "node" meaning a live runtime as
**runtime** (one per machine), "display name" as **name**, "control plane" as **local API**,
"helper" and "helper process" as **processor interpreter**, "app-process" as **runtime
process**. Older entries are facts about the shipped tree; the pivot's entries say what changes.

## Product (the MVP sentence) — IN-FLIGHT (→ runtime-description; unbuilt: `H264Encoder`'s CPU arm)

- **DECIDED** — A Python developer on Linux with an NVIDIA GPU or with none, or on Apple
  Silicon, installs the Tatolab runtime once — `curl | sh` on Linux, `Tatolab.app` on a Mac,
  each registering it as a per-user service — installs `tatolab-stream` from PyPI into an
  ordinary uv-managed venv, runs `tatolab new` then `tatolab dev`, sees Tatolab's sample
  stream live in their browser within a minute, on every floor — and on the software device
  once `H264Encoder`'s CPU arm lands (§Media I/O; owner, 2026-10-17) — then points it at their
  camera and makes the pipeline theirs by editing the scaffolded node — zero ceremony: no
  manifest, no `main()`, no schema wrangling, a fast edit loop. The
  zero-ceremony clauses bind both floors alike. Apple Silicon is a supported floor, not a
  developer machine: CI gates it on every PR, its runtime unit is proven in the release
  closure, and a macOS-only regression blocks a release as a Linux one does. macOS
  security prompts are part of the experience and do not breach zero ceremony; needing an
  app bundle to obtain them would. Every ticket traces to this sentence or does not exist.
  [importable-python-library — SHIPPED #1683, #1684, #1711; macos-platform-floor —
  SHIPPED #2357, #2359, #2361, #2362; package-split-and-lend — SHIPPED #2673, #2693, #2697;
  runtime-hosting — SHIPPED #2721, #2722; optional-accelerators — SHIPPED #2761, #2763,
  #2796; moq-relays — SHIPPED #2853, #2864, #2866]
- **DECIDED** — Terms of the sentence: Tatolab is a pure-Python stream package,
  `tatolab-stream` from PyPI, beside a native runtime — `tatolabd`, the `tatolab` CLI and
  the `tatolab.runtime` portion processor interpreters borrow, one unit never shipped
  through pip (§Packages, the package split and the lend); a stream is a normal Python
  codebase — one venv, one Python version, ordinary PyPI dependencies, nothing dynamically
  downloaded; `dev`/`run` load the sole `@stream` in `stream.py` by convention —
  `run <file>.py:<fn>` or `run <module>:<fn>` loads one, `-f <file>` overrides the file,
  `--name` the stream's name — and a directory holding an `app.py` but no `stream.py` is
  refused naming `stream.py` and `-f`; nodes are Python classes written in the project or
  imported from pip-installed packages, and `stream_builder.add` takes the class; the
  builder is a `StreamBuilder`, its API `add`/`connect`/`expose`.
  [importable-python-library — SHIPPED #1683, #1707, #1708; stream-graph — SHIPPED #2618,
  #2655; package-split-and-lend — SHIPPED #2673, #2686, #2693; authoring-names — SHIPPED
  #2682]
- **DECIDED** — The zero-ceremony bar (the sentence is untrue until all hold): no
  manifest authoring; no boilerplate entry; messages/schemas fixed (no engine schema
  matching, cast-at-read, no versions at the code layer); scaffolding for app and
  processor; the scaffold pins `.python-version` (3.12) and the wheel supports a small
  Python version range. [importable-python-library — SHIPPED #1684, #1711; schema-free-ports
  — SHIPPED #1814]
- **DECIDED** — Rust authoring stays a supported capability: a Rust app is a plain
  cargo project depending on the `streamlib` crate — no wrapper generation, no special
  format; third-party Rust processors for Rust apps are ordinary cargo dependencies,
  source-compiled. [importable-python-library — SHIPPED #1715]
- **DECIDED** — The two floors are one product surface. A Python node written
  against `tatolab.stream` runs on both; where it cannot is a short closed
  list that refuses by name before a frame flows — at load, or in `setup()`,
  naming the platform — never mid-frame: ray-tracing kernels (MoltenVK has no
  `VK_KHR_ray_tracing_pipeline`; each constructor refuses at `setup()` naming the absent
  tier), `VirtualCameraSink` (refused at load), the CUDA Array Interface, and the
  fd-shaped raw handles (`export_dma_buf`, `export_opaque_fd`, `import_dma_buf` exist on
  macOS and refuse pointing at `export_iosurface`). The scaffold and the examples use
  only the portable surface. The guarantee is mechanical, never prose: what a node is
  handed is declared once in `tatolab.stream`, as Protocols and runtime-backed functions
  a conformance gate holds both binaries to member for member in CI, and the rest of the
  native module once in `_engine.pyi`, gated by `stubtest` against both binaries, so no
  class or method exists on one floor and not the other; the three suites — stream,
  runtime and integration (the entry below) — run on both floors, their GPU-free halves on
  both CI lanes; on Linux their `requires_gpu` halves run on the lane on the unit's own
  software device, and a test stays on the rig only when it carries
  `rig_only_capability(reason=…)`, whose reason `test_platform_markers.py` holds to a closed
  list — Vulkan Video, DMA-BUF modifiers, CUDA, a camera, an audio device; on macOS
  the `requires_gpu` half runs on that floor's rig until the parity ticket that first makes
  it green; and a test skipped off
  Linux carries `linux_only_capability(reason=…)`, whose
  reason `test_platform_markers.py` holds to the closed list plus one named group — a
  test whose body is itself a Linux mechanism (`XDG_RUNTIME_DIR`, v4l2loopback and udev,
  the boot-session file, X11/Wayland, SIGHUP and SIGINT hand-back). A test red on macOS
  for any other reason is a parity bug, never a skip.
  [macos-capability-parity — SHIPPED #2400, #2403, #2405; package-split-and-lend — SHIPPED
  #2671, #2686; optional-accelerators — SHIPPED #2778, #2780]
- **DECIDED** — The scaffold models the pathway: pixels on the GPU, logic on the CPU, the
  pixel view explicit — and it is the sample stream and only that. `tatolab new` writes one
  `@stream`, `TestPatternSource` (the branded animation, §Media I/O) → `HalftoneLensEffect`
  → `H264Encoder` → `expose(…, PUBLIC)`, and two nodes, each in its own module under
  `nodes/` — the `HalftoneLensEffect` over `GlslPixelEffect`, a drifting lens that turns what
  it covers into dots (owner, 2026-10-19), and a numpy `BrightnessMeter` on a fan-out of the
  effect's output that reads the frame through `frame.cpu()` and logs its mean once a
  second, paced on `ctx.time` — with dependencies `tatolab-stream` and `numpy>=2.1`,
  nothing more, the same on both floors. `tatolab new` takes no `--camera` and no
  `--test-pattern`, and no template holds a camera: camera projects live in the docs' sample
  projects and, later, an examples folder the app installs, and the in-repo camera examples
  stay examples (owner, 2026-10-16). The files render from the template files in
  `sdk/tatolab-stream/scaffold_template/`, which the `tatolab` CLI embeds, each
  placeholder its template's own default value so the templates stay importable and
  checkable; ruff runs over every render, pyright over the template tree, and the
  cross-floor check gates the output.
  [engine-steps-for-effects-and-model-input; engine-steps — SHIPPED #2434, #2438;
  stream-graph — SHIPPED #2612, #2618; package-split-and-lend — SHIPPED #2693; moq-relays —
  SHIPPED #2866]
  Rejected: a camera in the scaffold, as the default or as a flag — a user whose camera does
  not open meets a failure as the first thing (owner, 2026-10-16).
- **DECIDED** — `tatolabd` takes no stream and no argument: it is the machine's one runtime
  process — one engine, its built-ins, the local API on `<runtime directory>/local-api.sock`,
  the engine's signal ladder, and no Python in its process. It runs in the foreground and
  never detaches. At start it takes the machine lock, opens the state directory, resolves the
  machine name (§Networking) and re-loads every kept stream neither stopped nor failed; one
  that cannot re-load is recorded `failed`, never deleted. `tatolab` is a native CLI beside
  it, linking no engine, and every stream verb is a local-API call. `run` sends
  `run_stream {project_directory, stream_function, name, keep}` — `stream_function` as `run`
  takes it, none for the sole one — and the runtime finds `<project>/.venv/bin/python` —
  absent, refused by name, pointing at `uv sync` — runs `tatolab.stream`'s compile entry in
  it with the project as the import root (the graph's JSON on stdout, a failed compile's
  traceback as the error), describes the Python types and loads; the compile's cross-floor
  warnings come back in the reply and the CLI prints them on stderr. The runtime compiles, so
  an agent loads exactly as the CLI does. `graph`, `logs`, `mcp`, `relays`, `relay` and
  `--version` speak the local API, and `new` writes the embedded scaffold. Only
  `<project>/.venv/bin/python` is taken; a relocated venv is refused by name until a need
  appears. Owner, 2026-10-02.
  [package-split-and-lend — SHIPPED #2693, #2697; runtime-hosting — SHIPPED #2711;
  moq-relays — SHIPPED #2871; runtime-description]
- **DECIDED** — The machine lock and the state directory. On Linux the lock is the abstract
  socket `@tatolab-runtime`, so a container sharing the host's network is this machine to
  it. On macOS it is the loopback HTTP listener's own socket, `127.0.0.1:4446`, which
  `tatolabd` binds at start, before the state directory, and the listener later serves. No
  file is involved and nothing is created by root, so on macOS a held port refuses the
  runtime and never leaves it running without the listing. A second `tatolabd`, or any
  squatter, is refused naming its user, pid and executable. On Linux these come from peer
  credentials. On macOS the holder answers its pid on `GET /machine-runtime-lock-holder` and
  the prober reads the user and executable for that pid from libproc. Something on the port
  that does not answer is refused naming the port. Any user may take the one runtime first,
  and on macOS any process may hold the port first: a weaker guarantee than a root-owned
  file, accepted so that first run needs no administrator password (owner, 2026-10-24). The
  CLI names the holder too. The state directory is
  `$XDG_STATE_HOME/tatolab/` (else `~/.local/state/tatolab/`), or `~/Library/Application
  Support/Tatolab/`: `streams/<stream>.json` per kept stream, written atomically with mode
  0600 — the graph compiled at load, the project directory and interpreter, the stream
  function, `stopped` or `failed` and why, its crash count and the owner's exposure rulings —
  and, under `logs/`, the runtime's own log, which belongs to no project. A stream's logs and
  pipeline cache stay under its project's `.streamlib/`. Owner, 2026-10-02 and 2026-10-24.
  [runtime-hosting — SHIPPED #2711, #2713; machine-lock-without-administrator — SHIPPED #2929]
  Rejected: a lock file on macOS — root-owned behind a first-launch administrator prompt, or
  world-writable in `/tmp` or `/Users/Shared` — the first asks a password at first run, the
  second gives the port's guarantee plus a stale file to clean up (owner, 2026-10-24).
- **DECIDED** — The tests divide as the code does. The stream suite
  (`sdk/tatolab-stream/tests`) is pure Python in a venv holding only `tatolab-stream` —
  declarations, the builder and its name resolution, compile, the generated built-ins, the
  data types, the cross-floor check, the scaffold templates — with runtime-backed objects as
  test doubles of their Protocols; never a GPU, never the runtime. The runtime suite is Rust —
  the engine crates, `tatolabd` and `tatolab` — fed graph data and native built-ins only, with
  no Python node. The integration suite (`tests/stream-on-runtime/`) runs both on purpose: it
  starts `tatolabd`, loads each fixture stream with `tatolab run` (the runtime compiling it in
  the suite project's venv) and drives it over the local API — processor
  interpreters, the lend, describe, escalate ops, pixel and device exchange, the CLI end to
  end. The division is structural, not a lint: each CI job installs only what its suite may
  touch, so a stream test reaching for the runtime fails to import. On Linux the
  `requires_gpu` halves run on the lane on the unit's own software device, a test staying on
  the rig only under `rig_only_capability(reason=…)` (the entry above). Testing a user's
  stream without a runtime is separate work. Owner, 2026-10-02. [package-split-and-lend —
  SHIPPED #2686, #2689, #2693; optional-accelerators — SHIPPED #2778, #2780]
- **DECIDED** — A stream is the unit a person writes and runs: today's app — a directory whose
  `stream.py` defines one or more streams, each a function decorated `@stream` that builds that
  stream's graph of nodes and links from the classes it imports (`@node`), plus `pyproject.toml`
  and one venv; a stream may expose ports. Each machine runs one runtime, which runs many streams — written by different people
  or teams — owns the accelerator when one is
  present, serves the machine's local API, schedules across streams, sends every exposed
  port to relays through one publisher, and runs the local relay's two instances as its
  children. Every Python node keeps its own process, and agents keep changing live graphs.
  Owner, 2026-09-30. [one-runtime-per-machine; stream-graph — SHIPPED #2612, #2618, #2655;
  moq-relays — SHIPPED #2842, #2853]
- **DECIDED** — What Tatolab is for: a simple SDK for defining live streams as pipes and
  sharing them with people and agents. Tailscale users are the first customers, and Tatolab
  builds nothing Tailscale already does — nor reads or configures it: a tailnet reaches a
  machine's local relay as any network does. Robotics and physical-machine systems are out of
  scope, and Tatolab never replaces a team's own ROS or Zenoh stack. Owner, 2026-10-04 and
  2026-10-16. [moq-on-the-tailnet; moq-relays — SHIPPED #2842]
- **DECIDED** — The runtime is required and always on, installed once per machine by an
  installer — the desktop app, a package manager, a `curl | sh` script or a distro package —
  which ships the runtime, the CLI and the native portion processor interpreters borrow as one
  version-matched unit and registers the runtime as a per-user service. `run` never starts a
  runtime; it loads a stream into the one that is running. pip distributes the pure-Python
  stream package and packs of nodes and streams, never the runtime. Owner, 2026-09-30: "treat
  it like Docker"; "pip is just for distributing the packaged streams". [one-runtime-per-machine]
- **DECIDED** — Who starts the runtime. With no runtime running, every `tatolab` verb that needs
  one fails at once, unable to reach the local API's socket, naming the socket and how to start
  the runtime — Docker's shape when its daemon is down; nothing a verb does starts a runtime. On
  Apple, `Tatolab.app` registers `tatolabd` as its login service, as Docker Desktop does, and a
  `run` against a running runtime connects as usual; a developer may still run `tatolabd` in a
  terminal. On Linux the installer — `curl | sh` or a distro package — registers
  `tatolabd` as a systemd user service, the service being the end state on Linux, not a stand-in
  for the app. Owner, 2026-10-02. [runtime-hosting — SHIPPED #2711, #2721, #2722]
- **DECIDED** — How a stream is loaded and kept. `tatolab run <stream>` loads it attached — its
  logs in the terminal, Ctrl-C unloads it; `tatolab run -d` loads it to keep — the runtime
  records the graph its function compiled to at that load, the project's venv path and the
  exposures in its state directory, and re-loads that recorded graph on every start, a crash's
  restart included. There are no restart policies: a kept stream always comes back unless stopped
  or failed. `tatolab
  stop` unloads a kept stream and remembers it as stopped, across restarts too; `tatolab start`
  re-loads a stopped stream from its record; `tatolab rm` forgets a stream, the only verb that
  loses one (owner, 2026-10-02). An attached
  stream is never recorded: a runtime crash ends it, and its `run` exits with an error naming
  the crash and the runtime's log. Live edits are never recorded, so the
  function wins on the next start, and picking up a changed source is another `run -d`;
  a kept stream implicated in the runtime's last two crashes in a row — a clean stop of the
  runtime or a manual restart is no crash and resets the count — or one that cannot re-load at
  start (a missing venv, a type that will not describe) is `failed`, shown with its reason and
  skipped at start until `tatolab start` retries it; a first load that fails is refused
  (owner, 2026-10-02). `tatolab streams` lists all four — attached, kept,
  stopped, failed; `tatolab dev` is `run` reloading on edit, and after a crash it waits for the
  runtime and loads again. Where no installer
  put a runtime, `tatolabd` runs in a terminal or as a container's entrypoint; there is no
  `up` or `down`. One runtime per machine, owned by one user — whoever installed or started
  it; another user's runtime on the same machine is refused at start naming the holder, and a
  server or robot runs it as one service account. A shared, system-wide runtime is not built
  until a real shared-machine need appears. Constraints kept (owner): streams get their own
  compute; every stream is addressable by URL, somewhat in the manner of Plan 9; no second
  mode unless it solves a real problem; it runs on very low-power devices; a stream that
  exposes devices stays long-running. Testing a stream without a full runtime is separate
  work, outside this pivot. Owner, 2026-10-01. [one-runtime-per-machine; runtime-hosting —
  SHIPPED #2711, #2713]
- **DECIDED** — The stream actions, as built. `keep: false` binds a stream to the connection
  carrying the call — `/mcp/stdio`, which `tatolab run` and `tatolab mcp` both hold — and it
  unloads when that connection closes (Ctrl-C, a closed terminal, a killed CLI, an MCP host
  exiting); a one-shot `POST /mcp` can only keep. `run` follows the stream's records with
  `logs {stream, after}`, `after` a record sequence number, and a runtime crash closes the
  connection so `run` exits 1 naming it. A name loaded or kept — stopped or failed included —
  is refused naming its project, `--name` the way out; a `keep` load of the same project and
  function replaces the record and keeps the owner's rulings. The tools: `stop_stream`
  (unload; record `stopped` for a kept stream, end an attached one's `run`), `start_stream` (a
  stopped or failed one), `remove_stream` (unload, forget), `list_streams` (name, attached /
  kept / stopped / failed and why, project, node count) and `expose_port {stream, node, port,
  level}`, `level` one of `internal`, `private`, `public`, which sets the port's level live,
  cutting at once every reader the level no longer allows, and records the owner's ruling
  for a kept stream — the owner's `expose` wins over the function's `exposed`, which is the
  default for a port without a ruling. A restriction on a kept stream
  is recorded before it changes live, so a crash can only tighten a port, never reopen it.
  `dev` is `run` plus a watch re-loading on save, and after a crash it waits and loads again.
  The service restarts a crashed `tatolabd` and its kept streams return. Owner, 2026-10-02.
  [runtime-hosting — SHIPPED #2711, #2713]
- **DECIDED** — Several streams in one project or package: a stream is a decorated function,
  `@stream def camera_rig(stream_builder: StreamBuilder)`, and a file or a package may define
  as many as it likes; the bare `setup(stream)` retires rather than living beside it, and a
  package may ship
  both runnable streams and composable nodes (owner, 2026-09-30). The name defaults to the
  function's and its docstring is the description an agent reads; `run` with no argument
  runs the sole `@stream` in `stream.py` and refuses by name when there are several
  (owner, 2026-10-01); `run stream.py:camera_rig` or `run acme_rover:camera_rig` runs one. A
  package declaring its streams under an entry-point group is the packs OPEN's.
  [one-runtime-per-machine; stream-graph — SHIPPED #2618; authoring-names — SHIPPED #2682]
- **DECIDED** — Composition inside a stream is plain Python: a function that takes the builder,
  adds nodes, connects them and returns port references is a reusable fragment. The graph
  stays flat and addresses stay `<machine>/<stream>/<node>/<port>`; no group label and no nested
  subgraph is built until an editor or an agent needs to see a fragment as one thing. Across
  streams, linking to another stream's port is the composition. Owner, 2026-10-01.
  [one-runtime-per-machine; runtime-hosting — SHIPPED #2715]

## Packages & extension model — SHIPPED

- **DECIDED** — PyPI and cargo are the package systems. The custom module system is
  deleted in full: `streamlib_modules/`, the `.slpkg` format, `streamlib.lock`, the
  package source, the `add`/`install`/`link`/`pkg` verbs, `BuildOrchestrator` and all
  runtime downloading or compiling. Compilation happens at publish time, by the
  author, with standard tools (maturin/CI for wheels, cargo for crates) — StreamLib
  never compiles user code.
  [importable-python-library; importable-python-library-ripout — SHIPPED #1715;
  schema-free-ports — SHIPPED #1813; processor-class-identity — SHIPPED #1837, #1841]
- **DECIDED** — The plugin ABI is deleted: no dlopen'd processor cdylibs, no `repr(C)`
  vtable surface, and none of that ABI's load-time machinery — no dlopen load handshake, no
  cdylib build fingerprints. The scope is the deleted ABI and nothing wider: a processor
  interpreter imports the lent `tatolab.runtime` and checks that it is its parent's build —
  true by construction and kept as the backstop — which is the handshake §Processor model
  states, not an ABI surface returning. The extension paths are
  Python packages and Rust source crates only — and an extension wheel is a Python
  package: Rust inside, loaded across the CPython ABI, never dlopen'd by the engine
  (extension-model, 2026-09-04).
  [importable-python-library; importable-python-library-ripout — SHIPPED #1715;
  local-transport-hardening — SHIPPED #2262; package-split-and-lend — SHIPPED #2689]
- **DECIDED** — Third-party native code (closed-source included) ships as an ordinary
  Python package whose native internals expose capabilities to Python as handles —
  frames, FDs, exportable device allocations, buffers — wrapped by a Python
  processor. It never links the engine and never speaks streamlib internals; the
  CPython ABI is the only
  binary boundary, and no process ever holds two streamlib engines — the runtime process
  runs the one engine, and a processor interpreter imports the lent `tatolab.runtime` as
  its node's bindings, never as a second engine. Handles it exposes must be genuinely
  transferable across a process boundary (an fd, an exportable allocation) — an
  address-space-local pointer is not a handle.
  [importable-python-library — SHIPPED #1710, #1756, #1757; package-split-and-lend — SHIPPED
  #2689, #2693; amended by one-runtime-per-machine: one runtime process per machine]
- **DECIDED** — First-party optional capabilities ship the same way third-party native
  code does: as separate PyPI extension wheels — Rust inside for speed, a Python
  node as the binding for any node the wheel supplies — depending on `tatolab-stream` by a
  minimum version and never on the runtime. Optional means a stream can be complete
  without it. The engine is not the home of every capability; it is the home of what
  belongs in core. Owner, 2026-09-04. [extension-model; package-split-and-lend — SHIPPED
  #2691]
- **DECIDED** — One extension mechanism, and no package extends the engine. A *processor
  extension* is a Python node class in a pip-installed package whose per-frame work runs in
  native code the same wheel carries: `stream_builder.add(TheClass)` adds it, as for any
  Python node; it runs in its own processor interpreter under the one placement rule; and it
  calls its own package's Rust directly — the engine does not call extension code on the
  data path, and there is no node-to-engine-to-wheel round trip. The runtime is the host and
  streams are its guests: one shared runtime serves every stream on the machine, and code
  inside it could crash, read or send out every stream's data, so nothing a package carries
  runs in the runtime process. A package does its own setup where its nodes run, at import or
  on first use — `tatolab-webrtc` brings up its network runtime and TLS provider that way; a
  node's lifecycle methods are unchanged; an outside program watches the local API. An
  engine-grade capability — specialised graphics processing, a transport, a device class —
  enters as a built-in under the criterion below. Pure Python stays a complete way to write a
  node; this is an additional pathway. It compiles at publish time with maturin, is never
  dlopen'd by the engine, and the CPython ABI stays the only binary boundary.
  Rejected: a capability-extension hook — an entry point the runtime reads and runs in its
  own process, or in an interpreter embedded in `tatolabd` for hooks alone — since a native
  `tatolabd` runs no Python and a package inside the shared runtime reaches every stream
  (owner, package-split-and-lend decision 2, 2026-10-02).
  [extension-model; package-split-and-lend — SHIPPED #2691]
- **DECIDED** — The criterion for a built-in, stated so that the next one is
  contestable: a first-party capability is a built-in — its native half in `tatolabd`, never
  in a pip distribution, its generated class in `tatolab-stream` — only if (a) its per-frame
  path has a deadline the helper hop cannot meet — a device audio callback — or (b) it needs an engine-only primitive the handle-shaped surface does
  not export, or (c) it presents an OS-facing device to the other applications on the
  machine — a virtual camera; a virtual microphone would be the same case — which
  installing the runtime alone must make available, with no further package to install;
  and in every case (d) a named consumer exists. Everything else is an
  extension. What an extension needs and the engine does not yet expose is engine work,
  done as engine code inside the extension's own change, rather than by the extension
  reaching past the surface. Codec sessions are not exported to Python; an extension that
  needs one brings that export as engine work. [extension-model; virtual-camera-sink —
  SHIPPED #2196, #2197, #2198; package-split-and-lend — SHIPPED #2684, #2697]
- **DECIDED** — An extension wheel is built the way a third party would build one, which
  is the dogfooding the pivot exists for: a standalone maturin project under `packages/`
  with its own workspace root and lockfile — not a member of the engine workspace —
  depending on `tatolab-stream` by a minimum version and on `pyo3`, and on no engine crate;
  its nodes run in processor interpreters, where `tatolab.runtime` is lent. While released
  from this repository it carries Tatolab's one version and is published to PyPI beside
  `tatolab-stream` (§Distribution & versioning). Distribution names take the
  `tatolab-<name>` form and import as `tatolab.<name>`. Its gates are its own CI lane —
  stubtest over its own `.pyi`, pyright, its crate's tests — since the engine workspace's
  gates do not walk a non-member. A Rust-side extension SDK is not owed by the first
  extension, whose Rust handles bytes and no engine object; it lands with the first
  extension that needs one. [extension-model; tatolab-names; package-split-and-lend —
  SHIPPED #2673, #2691, #2697]
- **DECIDED** — The engine's handle-shaped primitive surface is the public contract
  for native interop: DMA-BUF / OPAQUE_FD import and export on Linux and IOSurface export
  on macOS, texture rings, codec byte pumps, the audio clock, color
  resolution — surfaced to the Python ecosystem as DLPack and the CUDA Array Interface
  (DLPack first; the CUDA Array Interface is CUDA by nature and never offered on macOS).
  A graph frame's natural DLPack side is the device on both floors. On Linux it is
  `kDLCUDA`, and the contract is zero-CPU-copy stated honestly: tiled engine textures
  reach a linear tensor via one GPU blit into an exportable OPAQUE_FD staging buffer,
  because DLPack expresses strided linear memory only — and that blit reads the
  surface's pooled backing whenever one exists; a producer-internal texture never
  sources a cross-process export. On macOS it is `kDLMetal` over a no-copy `MTLBuffer`
  on the frame's own IOSurface — zero copies and no staging, because unified memory
  makes the surface's bytes the device's bytes. `torch.from_dlpack` yields `cuda` on one
  and `mps` on the other, and `mx.from_dlpack` consumes the same Metal capsule. The
  stub states the consumer floors where the capsule is minted — torch ≥ 2.10 by source
  (2.9 maps `kDLMetal`, 2.10 fixes a sliced import; measured on 2.14) and MLX ≥ 0.32 —
  and no refusal names them, since the wheel imports neither. The Vulkan↔CUDA and
  Vulkan↔GL interop adapters survive as in-process capabilities (torch/cupy and GL
  consumers); only their cross-DSO `-abi` halves die with the plugin ABI.
  [importable-python-library — SHIPPED #1710; surface-id-lifetime-contract — SHIPPED
  #1868; macos-capability-parity — SHIPPED #2404]
- **DECIDED** — Raw-handle export is public contract for both flavours, gated by
  the Full capability surface: a raw memory fd is minted only by
  `GpuContextFullAccess` — `export_dma_buf` for the DMA-BUF flavour,
  `export_opaque_fd` for OPAQUE_FD — on every minting path, escalate ops included,
  and the gate bounds use as well as minting: per-frame data-plane reach, read or
  write, through a held raw fd is out of contract — an interim bound; the zero-copy
  per-frame hand-off is OPEN below — and the per-frame doors are surface ids and
  the engine-ordered device-tensor scope. A raw handle names the
  allocation, never the frame: the caller owns each freshly-dup'd fd (adopted by a
  successful foreign import, closed by the caller otherwise), the surface-id
  lifetime guarantees end at export, pixels under a held fd after checkout release
  are whatever the pool hands the slot next — the pool bucket is shared across
  processors, so possibly another processor's frames — and allocations born after
  an export set was taken (pool growth) are outside it. A raw fd is write-capable:
  from a pooled allocation's first export onward, the immutable-frame guarantee
  for frames that allocation backs rests on the importer honouring the use bound,
  outside the engine's enforceable envelope. `export_opaque_fd` returns a typed
  export object carrying the allocation-stable shape — whole-allocation byte size,
  extent, format, the image-creation recipe (tiling, usage, mip/layer/sample
  counts; `None` for a pixel buffer, which exports too), dedicated-allocation
  status, the exporter's memory type index, and the
  exporting device UUID — and no per-frame state (no image layout, no timeline
  edges); `export_dma_buf` keeps `(fd, byte_size)` and refuses the OPAQUE_FD
  flavour by name, pointing at `export_opaque_fd`. An fd flavour's export is taken from a
  resolved surface, never from a name: the fd reaches a helper at checkout, so a
  texture acquired but not yet resolved is refused telling the caller to resolve
  its surface id first, and every other refusal likewise names the flavour's own
  door. The recipe travels because a raw allocation is consumed as an image — a
  linear buffer mapping over tiled memory yields block-linear bytes, never pixels —
  and a successful import pins the payload past the exporter destroying the texture
  it came from.
  On macOS the raw handle is the IOSurface flavour: `export_iosurface` on the Full
  surface returns an `IOSurfaceMachPortExport` — a fresh Mach send right per export,
  owned by the caller and given back with `mach_port_deallocate` — carrying allocation
  byte size, row pitch, extent, format, and for a texture the image recipe (tiling,
  usage, mip/layer/sample counts; `None` for a pixel buffer, which exports too). It
  carries no device UUID, memory type index or dedicated-allocation status, which name
  nothing on one unified-memory device. It is gated, owned and bounded as the fd
  flavours are: a held right keeps the surface reading as in use, pinning its pool slot
  as a held fd pins a DMA-BUF. `export_dma_buf`, `export_opaque_fd` and `import_dma_buf`
  exist on macOS and refuse by name pointing at `export_iosurface`; `export_iosurface`
  exists on Linux and refuses by name pointing back. A raw handle is platform-shaped by
  nature; choosing one is visible in the code.
  [raw-handle-export-contract — SHIPPED #1900; macos-capability-parity — SHIPPED #2400,
  #2405]
- **OPEN** — Zero-copy per-frame consumption by a foreign GPU stack: intended, do
  not build until designed. Direction: export a surface's slot set once at setup,
  name the current frame per-frame by surface id, signal the hand-off with an
  exported timeline edge under the same Full gate; retires the per-frame blit for
  raw-fd consumers. [raw-handle-export-contract]
- **DECIDED** — A published surface id names an immutable frame: from publish until
  every holder releases it, the pixels under that id change only through the
  surface's own write-back protocol (an explicit, engine-ordered edit other holders
  are meant to observe) — never through producer reuse. The id itself is per-frame:
  each pool acquisition publishes `<slot>#<generation>`, the `#<digits>` suffix is
  reserved to that grammar (the surface-share service refuses any other registration
  carrying one), and recycling the slot retires the previous generation's id — a
  stale id fails loudly at resolve and checkout as a recycled-frame error, never
  resolving to the slot's newer pixels. The pool slot backing a held surface is
  never rehanded to a producer — in-process via the existing refcount, cross-process
  via a checkout lease minted by the surface-share service at checkout, released
  explicitly by the consumer and reclaimed on connection drop. The claim is taken
  at the typed cast — the moment a consumer names what it is holding — and released
  when that object drops; the read offers the constructing type the means to take
  one, on terms equally open to any authored class, and takes none for a consumer
  that reads the payload untyped. Publish-to-claim transit rides pool depth, and so
  does an untyped read: the strictness dial is also the safety dial — depth bounds
  the window, and outwaiting it loses the frame, never serves somebody else's pixels:
  the claim refuses the id by name as recycled, and the consumer counts the message
  as a drop on the link it arrived on, a loss and never an error (owner,
  2026-10-24). The
  engine inspects no message content anywhere. The producer never waits on a consumer:
  its node's output pool skips leased slots and grows to its cap; at the cap, or past
  the runtime's memory budget (§Graphics), the producer drops its own frame — a slow
  consumer costs memory, then its own node's frames, never another processor's
  cadence; only the runtime's memory budget is shared between nodes. A
  producer-internal transient (a frames-in-flight ring
  texture) never backs a cross-process export: the export blit sources the
  surface's pooled backing whenever one exists, read-only; texture-backed export
  remains for surfaces with no pooled backing (kernel outputs).
  [surface-id-lifetime-contract — SHIPPED #1868, #1869, #1871, #1877;
  shared-pools-bounded-by-memory-budget — SHIPPED #2741]
- **DECIDED** — The cast object is the tensor-protocol producer: a cast type that
  claims its surface exposes pixel access on the object itself, and the performance
  gradient is spelled, not policed. The bare object speaks `__dlpack__` /
  `__dlpack_device__` as the read path — GPU-resident, zero ceremony,
  `torch.from_dlpack(frame)` is the shortest and fastest spelling. Validity rides the
  claim the typed cast takes: the frame is immutable while the object lives and the
  view ends when it drops. A write through the bare view is out of contract — the
  write doors are the scopes: `with frame.writable() as t:` for GPU edits and
  `with frame.cpu() as img:` for CPU reach, the slow path saying so in its name.
  Whether a frame takes an edit at all is the engine's one answer for both doors —
  a write-back belongs to a pooled frame whose allocation is its only backing, or
  to a registered texture that takes a recorded copy in — and a frame that cannot
  take one refuses `writable()` by name and reaches `cpu()` as a read-only array,
  numpy-enforced, rather than accepting a write that lands where other holders
  cannot see it. A producer never creates that shape by publishing its own
  internals: a published id names one picture to every consumer, in-process or
  not, so a producer's private scratch (a capture ring) is never registered under
  it.
  `writable()` keeps the one write-scope rule already decided for the device-tensor
  scope, rebased onto the cast object: it edits a staging, the block edge is the
  publication point, the engine orders the write-back ahead of its own next read,
  and leaving by a propagating exception discards the write without suppressing the
  exception. `cpu()`'s array follows its backing: over a pixel-buffer frame it is
  the surface's own coherent host mapping — no staging between a store and the
  frame — so publication is per store, and a raise mid-edit leaves a complete edit
  of fewer pixels; over a texture backing it is the surface's host-visible export
  staging, publishing at the block edge and discarding on a propagating raise
  (§Graphics states the staged door). Across both, the block edge ends the write
  intent, a raise never suppresses, and no door publishes a torn frame.
  The wheel ships the protocol as one public composable piece any cast type composes
  (`ClaimedSurfacePixelAccess`), over the unchanged claim seam — `VideoFrame` is itself
  built from it, which is the proof it holds no privileged position over any library or
  user cast type. The surface a type claims is the field it declares, defaulting to
  `surface_id` and never guessed: the wheel inspects message content no more than the engine
  does. The bare
  protocol binds a type that claims exactly one surface: a type claiming several gets
  no bare `__dlpack__` — the ambiguity is refused by name — and reaches each surface
  through that surface's own protocol object (`PixelAccessToOneClaimedSurface`, one per
  declared field). `cpu()` yields a numpy array writable
  exactly when the frame can take a write-back — the engine's answer, asked once per
  pool slot and binding both doors of the cast object.
  Wheel-layer grammar only over the shipped staging, export and escalate
  primitives — no engine change.
  [cast-object-tensor-protocol — SHIPPED #1926, #1927;
  texture-backed-cpu-reach — SHIPPED #1942]
- **DECIDED** — The portable path for GPU math in a Python processor is torch over the
  frame's own DLPack capsule: `torch.from_dlpack(frame)` on every floor, the device taken
  from that tensor or from `torch.accelerator`, and never spelled by name. Any DLPack
  consumer may read a frame on the floor that supports it — cupy and jax on Linux, MLX on
  macOS — and the engine refuses none; only the torch path is portable, and the cross-floor
  check says which is which. Array-API wrappers are neither modelled nor refused.
  Landing a frame in a texture is the engine's `copy_surface_to_surface` (§Graphics),
  never an array library. The scaffold and the shipped examples model this path and no
  other for GPU math — the kernel examples and `camera-python-effects` land frames with
  the engine copy and depend on no cupy, `fisheye-object-detection` takes its device from
  `torch.accelerator.current_accelerator()` and pins `torch>=2.10`, and
  `camera-virtual-camera`'s processors are portable while its app is Linux-only by
  `VirtualCameraSink`; torch is a dependency of neither the wheel nor the scaffold.
  [portable-gpu-interop — SHIPPED #2420, #2422, #2423, #2424]
- **DECIDED** — A frame's DLPack export honours a request for the host side on every
  floor: `dl_device=(kDLCPU, 0)` hands back the surface's host mapping, so
  `numpy.from_dlpack(frame, device="cpu")` is one line on both floors — on macOS the same
  IOSurface pages the Metal capsule aliases, not a copy. `copy=True` stays refused by name
  at every door — the surface handle's `__dlpack__` and the device-tensor scope — and
  `as_numpy()` rides the same host request, one mapping, not two copies.
  [portable-gpu-interop — SHIPPED #2404]
- **DECIDED** — The cross-floor check reads a Python processor's source and its
  `pyproject.toml` for what binds it to one floor and names the file, line and portable
  spelling of each. It runs inside `tatolab dev` and `tatolab run` as a warning that
  never blocks a start, gates in CI the stream package's and the runtime's own Python and
  the scaffold's output, and
  runs over `examples/` when a change ships; it is no CLI verb of its own. It reads
  source, not behaviour: a dynamic import or a dependency's own device choice is left to
  the same Python suite running on both floors. As built, `tatolab.stream._cross_floor_check`
  (stdlib `ast` and `tomllib`) reads every `.py` under the app anchor — skipping
  dot-directories, `.venv`/`venv` and any directory holding a `pyvenv.cfg` — and the
  anchor's `pyproject.toml`, and flags: an import of `cupy`, `pycuda`, `numba.cuda`,
  `torch.cuda` (also reached as an attribute) or `mlx`; `"cuda"` or `"mps"` passed as a
  device (`device=`, `.device(…)`, `.to(…)`) and `.cuda()`; a closed-list name
  (`VirtualCameraSink`, the three ray-tracing constructors, `export_dma_buf`,
  `export_opaque_fd`, `import_dma_buf`, `__cuda_array_interface__`) where it is used,
  never where it is imported nor where a `typing.Protocol` class body declares it,
  reported as allowed on its floor with the other floor's peer; and a `cupy*` or `mlx*`
  dependency with no `sys_platform`/`platform_system` marker. Nothing under a
  `sys.platform` guard is flagged, in either branch.
  The compile entry `tatolab` runs in the project's interpreter prints the block on stderr
  before it compiles — nothing when clean, a failure of the check itself reported, a start
  never blocked; on Python 3.10 the dependency rule is skipped and the block says so. CI
  gates the stream package's and the runtime's own Python and both scaffold variants on
  both lanes. [portable-gpu-interop — SHIPPED #2421; package-split-and-lend — SHIPPED #2693]
- **DECIDED** — streamlib offers an extension point for an external control client, and runs
  complete without one. [one-runtime-per-machine]
- **DECIDED** — The package split and the lend. `tatolab-stream`, importing as
  `tatolab.stream`, is pure Python and is everything a stream module imports: `@stream`,
  `@node` with its `@node.input` and `@node.output`, the `StreamBuilder` and its references,
  the built-in node classes with their config shapes, and the data types; a stream is
  written, type-checked and
  compiled with no runtime installed. `tatolab.runtime` is the engine's native part — the
  engine, the bindings a node's calls go through while it runs, the processor-interpreter
  bootstrap, and on macOS the bundled Vulkan driver — and ships only with the runtime, never
  through pip. A stream's venv installs `tatolab-stream` and holds no engine; the runtime
  starts each of the stream's processor interpreters from that venv's interpreter with one
  directory prepended to its `PYTHONPATH` — the runtime's lend directory, which holds
  `tatolab/runtime/` — so PEP 420 merges that `tatolab/runtime/` with the venv's
  `tatolab/stream/`. No distribution ships `tatolab/__init__.py`, and
  `tatolab/runtime` is a regular subpackage. The bootstrap is the runtime's own entry, never
  `-m` from the project directory, and the exact-build handshake stays as the backstop. A
  stream never names a runtime version and nothing of the runtime enters its `pyproject.toml`:
  the runtime loads what it understands and refuses by name anything in a graph it does not —
  a node type it lacks, a setting it does not know — and a newer runtime loads every graph an
  older stream recorded. Compiling happens in the project's interpreter, never in the
  runtime process. Owner, 2026-10-02. [one-runtime-per-machine; package-split-and-lend —
  SHIPPED #2671, #2673, #2686, #2689, #2693, #2697; authoring-names — SHIPPED #2682]
- **DECIDED** — `tatolab-stream` as built. A pure build backend, `requires-python >=3.10`,
  depending on what its own modules import and never on the runtime. A module belongs in it
  exactly when a stream or node module imports it: the declarations (`@node`, `@stream`, the
  `StreamBuilder`, the references, the compile entry), the config schema deriver, the data
  types (`AudioBlock`, `VideoFrame` and its colour types), the composable pieces (`ClaimedSurfacePixelAccess`,
  `PixelAccessToOneClaimedSurface`, `GlslPixelEffect`, the `ModelInputTensor` family,
  `NodeOutputTextureRing`), `clock`, `log`, and the cross-floor check the compile entry runs.
  `@node` registers nothing — it writes the class's declaration stamps — and the proof is the
  stream suite itself, run with no runtime on any path. [package-split-and-lend — SHIPPED
  #2686]
- **DECIDED** — Built-in nodes reach Python as plain classes generated from the runtime: `cargo
  xtask generate-built-in-node-classes` writes `tatolab/stream/_built_in_nodes.py` from each
  built-in's descriptor and its config's JSON Schema — its `type`, its doc line, its ports and
  a `TypedDict` of its config keys, so pyright checks `stream_builder.add(CameraSource,
  config={"device_id": "/dev/video2"})` — and CI fails when a regeneration differs. One
  declaration, the Rust one; no native marker class exists. [package-split-and-lend — SHIPPED
  #2684]
- **DECIDED** — What the runtime backs is declared once, in `tatolab.stream`: the contexts,
  `LinkInputDataReader`, `LinkOutputDataWriter`, `NodeLinkDataAccess`, the `GpuContext`
  capabilities, `GpuSurfaceHandle`, the kernels, `MonotonicTimer`, the texture exports,
  `monotonic_now_ns`,
  `gpu_limited_access_of_the_typed_read_in_progress`, and `start_monotonic_timer(interval_ns)`,
  the one way a node starts a timer — `MonotonicTimer(n)` is gone, with no alias (owner,
  2026-10-07). A class is a `typing.Protocol` carrying the native signatures; a function binds
  the runtime's on first call and, with nothing lent, raises `RuntimeError` naming itself and
  saying it runs in a processor interpreter. `_engine.pyi` covers only the bootstrap's private
  surface, and a conformance gate holds every class a node is handed to its Protocol member
  for member against the signatures pyo3 publishes. [package-split-and-lend — SHIPPED #2686]
- **DECIDED** — `tatolab.runtime` and the lend directory, as built. `tatolab/runtime/` is a
  regular package: `__init__.py` names the bundled ICD before `_engine` loads; `_engine`
  (`module-name = "tatolab.runtime._engine"`) holds the bindings alone;
  `_processor_interpreter_bootstrap.py` is the processor interpreter's entry; on macOS
  `_vulkan_driver/` sits beside them. It holds no `Runtime`, no CLI, no test harness and no
  control-plane client: the Python-hosted engine is deleted. maturin builds it as a wheel that
  `cargo xtask build-runtime` unpacks into `target/tatolab-runtime/lib/tatolab/lend/`, beside
  `bin/tatolabd` and `bin/tatolab` — the install prefix's own shape, built once for
  developers, CI and later the installer, never installed into a venv and never published. A
  gate fails any `tatolab/__init__.py` in the tree or in a built lend, and `dladdr` finds
  `_vulkan_driver/` beside `_engine` unchanged. [package-split-and-lend — SHIPPED #2689,
  #2693, #2697]
- **OPEN** — How an external control client plugs in. What it hands the runtime is settled: a
  relay's address and credential per adapter, and nothing else (§Networking, the adapters);
  how it plugs in stays open. Known (2026-10-04): no stream map, router or peer
  identity exists to push. [one-runtime-per-machine; moq-relays]
- **DECIDED** — The names, Tailscale-shaped. The runtime's program is `tatolabd`; the CLI is
  `tatolab`; the desktop app is Tatolab (`Tatolab.app`); the installer ships all three as one
  unit, and users still call the program "the runtime" (owner, 2026-10-02); the bare name `tatolab` is the
  product's and no pip distribution takes it — PyPI holds it with a placeholder that installs
  nothing (owner, 2026-10-02). The one distribution pip installs to write
  streams is `tatolab-stream`, importing as `tatolab.stream`. `tatolab.*` is a PEP 420 namespace
  shared by Tatolab's own distributions only: `tatolab.stream`; `tatolab.runtime`, the native
  portion `tatolabd` lends and no user installs; and optional first-party extensions, each
  `tatolab-<name>` importing as `tatolab.<name>` (`tatolab-webrtc` → `tatolab.webrtc`). No
  distribution ships `tatolab/__init__.py`. A third party's pack uses its own name, never the
  `tatolab` namespace. Which built-ins, extensions and packs ship inside the app or through pip
  is §Packages' packs OPEN. The Rust crate for writing streams is `tatolab-stream`. Owner,
  2026-10-01. [tatolab-names; one-runtime-per-machine; package-split-and-lend — SHIPPED #2673,
  #2691]
- **DECIDED** — No public Python name Tatolab publishes says "processor". The move from
  `streamlib` to `tatolab.*` re-spells every one still standing, at once:
  `ProcessorOutputTextureRing` → `NodeOutputTextureRing` (its module
  `node_output_texture_ring`), `ProcessorLinkDataAccess` → `NodeLinkDataAccess`, the GPU
  capabilities' `acquire_texture_from_processor_output_pool` and
  `acquire_storage_buffer_from_processor_output_pool` → `…_from_node_output_pool`, and the
  contexts' `processor_id` → `node_id`, the id `graph` renders on the node. A name an earlier
  change deletes is deleted, never renamed. The engine's Rust identifiers and the wire keep
  "processor" until the rename step. Owner, 2026-10-02. [tatolab-names; package-split-and-lend
  — SHIPPED #2673]
- **DECIDED** — The authoring names, with no alias. The object a `@stream` function is handed is
  a `StreamBuilder`, and the scaffold, the docs and every refusal model the parameter as
  `stream_builder`: `@stream def main(stream_builder: StreamBuilder)`; the decorator keeps
  `@stream` and a stream stays the decorated function. A node declares its ports with
  `@node.input` and `@node.output` — attributes of the `@node` decorator, so a node module
  imports `node` alone for them and no name it imports shadows Python's builtin `input()`. A
  Rust node's attribute macro keeps `input(…)` / `output(…)`, already the same word, and a node
  reference keeps `input(name)` / `output(name)`. Rejected: `@incoming` / `@outgoing` for
  ports — vocabulary no Python dataflow or media framework uses; `@input_port` /
  `@output_port` — a word the declaring side alone would carry (owner, 2026-10-06). Owner,
  2026-10-06. [authoring-names — SHIPPED #2682]
- **OPEN** — Packs, a registry, and loading a stream from a source. Direction (review, not
  decided; the owner wants to distribute what they build and update the app separately): the
  unit of distribution is a pack — one ordinary Python distribution carrying nodes and streams,
  published to a registry with globally unique names and immutable versions, its only metadata
  `pyproject.toml` plus an optional `[tool.tatolab]` table (publisher, display name, icon, the
  runtime range it needs) that is never required to run a stream; `add <pack>` installs it into
  a project, or into a default project the runtime manages when there is none, and never beside
  the runtime; `run <url-or-zip>` fetches the project into the runtime's state directory, runs
  `uv sync` there (uv creating the venv and fetching a missing Python), and loads it like any
  project — kept, the `run -d` way, since a stream from a URL, a zip or an index is one the
  runtime should keep running (owner, 2026-10-02) — and a project with no venv gets `uv sync`
  first. Environments are provisioned by the
  standard toolchain only — `pyproject.toml`, uv, a package index, git — never by machinery of
  streamlib's, which is what importable-python-library deleted. The runtime process imports
  nothing from a pack or a project; their code runs only in processor interpreters started from
  that venv. Undecided: the registry's owner and
  standards, a manager UI, and what the app's own catalog does. [one-runtime-per-machine]

## Consumers — examples & packages — SHIPPED

- **DECIDED** — `examples/` is the in-repo showcase and living documentation of the
  current authoring idiom, converted gradually and never a contract source: engine
  contracts are stated in the engine and proven by engine tests, and no example is read
  to infer what the engine guarantees. An external examples repository (the
  framework-repo/examples-repo model) remains a possible later move and is not decided
  now. [consumer-tree-disposition — SHIPPED #2052, #2053, #2054, #2055, #2056, #2057,
  #2058, #2059]
- **DECIDED** — `packages/` holds first-party extension wheels — the optional
  capabilities §Packages & extension model decides ship outside the wheel, with its
  built-in criterion deciding which side of the line a capability lands on. Each is an
  ordinary pip-installable Python package depending on `tatolab-stream`, never linking the
  engine.
  In-repo consumers (examples included) link a package locally as a Python path
  dependency — no publish loop stands between an example and the package it uses.
  Externally, packages publish to PyPI beside `tatolab-stream` (owner, 2026-10-08).
  `test-fixtures` remains as the tree's one engine-adjacent Rust crate.
  [consumer-tree-disposition — SHIPPED #2052; extension-model; package-split-and-lend —
  SHIPPED #2697]
- **DECIDED** — Conversion is a from-scratch rewrite in the current idiom, never an
  in-place upgrade: start from the `tatolab new` scaffold, mine the old directory for
  its logic only, author against today's full surface (delivery profiles, window
  contracts, cast objects, kernels-as-objects), and delete the old directory in the same
  PR. Every pre-pivot consumer neither deleted nor held below is conversion backlog under
  this doctrine, and that backlog is executed in full: `audio-mixer-demo`,
  `microphone-reverb-speaker`, `raytracing-showcase` and `fisheye-object-detection` are
  Python, and `camera-compute-kernel` and `camera-halftone` are kernel examples.
  `examples/` stands at ten converted beside one held. A showcase authored in the
  current idiom is an ordinary addition under the convention below, not conversion backlog:
  `camera-webrtc-publish` sends camera and microphone through the codec blocks to
  `WhipPublisher`, credentials from the environment; and `camera-virtual-camera` is the
  virtual camera's showcase — one `CameraSource` fanned to a `VirtualCameraSink` and to a
  Python effect feeding a second one, so a graph appears as two named cameras in any other
  application on the machine and both are gone at Ctrl-C. No example opens a window: an
  example's picture is shared through an exposed `H264Encoder` and watched at `/watch`
  (owner, 2026-10-20).
  Rejected: a recording showcase (`CameraSource → H264Encoder` beside `MicrophoneSource →
  OpusEncoder`, exposed and recorded to MP4 with a MoQ client's fmp4 export) and a codec
  round-trip showcase (a sender's encoded track read back through `RelayTrackSource →
  H264Decoder`) — the owner chose no showcase for either (owner, 2026-10-20).
  [consumer-tree-disposition — SHIPPED #2053, #2054, #2055, #2056, #2057, #2058, #2059;
  networking-extension-wheels — SHIPPED #2153; virtual-camera-sink — SHIPPED #2198]
- **DECIDED** — An example shows a person writing and running a stream: the scaffold's
  idiom, built-ins, nodes, links and exposure. It never shows engine work. That means no
  engine embedded in a host process, no driving the executor or its async runtime, and no
  reaching into engine internals. Showing an engine to a stream author is irrelevant to
  them and misleads them into thinking they have to do engine work, the way a Tailscale
  user would be misled by an example of modifying Tailscale. An engine-facing example is
  deleted, never converted. Engine contracts are proven by engine tests, as the first entry
  in this section states. Owner, 2026-10-09. [examples-show-streams-only — SHIPPED #2718]
  Rejected: porting `tokio-integration` to the new graph shape (#2615): it keeps
  alive an example of engine embedding that no stream author needs (owner, 2026-10-09).
- **DECIDED** — The GPU examples run on the portable path: `camera-compute-kernel`,
  `camera-halftone`, `camera-virtual-camera` and `camera-python-effects` land frames with
  `copy_surface_to_surface` and carry no cupy; `camera-python-effects` decimates through
  `numpy.from_dlpack(frame, device="cpu")`, and each dependency was measured on macOS
  arm64, so none is platform-marked and no processor is Linux-only (`mediapipe` 1.0.1,
  which aborts on macOS, is excluded by pin); `fisheye-object-detection` takes its device
  from `torch.accelerator` and prepares its detector input with `ModelInputTensorKernel`
  (`fit="pad_bottom_right", pad_to_multiple_of=32`), its boxes staying in frame
  coordinates. The cross-floor check found nothing in any example at ship time save
  `VirtualCameraSink` in `camera-virtual-camera`'s app and the ray-tracing constructors in
  `raytracing-showcase`, both on the closed list. [portable-gpu-interop — SHIPPED #2422,
  #2423, #2424; engine-steps — SHIPPED #2433]
- **DECIDED** — Retired in one sweep:
  `examples/pipelines`, `examples/camera-deno-subprocess` (its halftone effect rebuilt as
  `examples/camera-halftone`), `examples/camera-python-subprocess`,
  `examples/polyglot-manual-source`, `examples/camera-rust-plugin`,
  `examples/vulkan-video-roundtrip-cdylib-camera`, `examples/dynamic-reconfigure`,
  `examples/api-server`, `examples/api-server-demo`, `examples/runtime-graph-json-demo`,
  `examples/hello-streamlib` (the `tatolab new` scaffold is the hello; `camera-display`
  is the canonical minimal example), and `packages/audio`, `packages/camera`,
  `packages/display`, `packages/frame-tap`, plus the `packages/core` stub. A test owns its
  fixtures: CI reaching into `examples/` would make a consumer a contract source.
  [consumer-tree-disposition — SHIPPED #2052]
- **DECIDED** — A consumer blocked on an undecided domain is held in-tree until the
  align covering that domain mines it for logic; its deletion rides that change's own
  ship. Held on audio plugins: `packages/clap`. Held on screen capture:
  `packages/screen-capture`, `examples/screen-recorder`. [consumer-tree-disposition — SHIPPED
  #2052]
- **DECIDED** — No additional native-processor *distribution* mechanism is owed
  pre-1.0: an extension wheel is an ordinary Python package on the ordinary index, and
  closed-source Rust processors for Rust apps are deliberately not a path — a
  closed-source vendor ships the Python package whose native internals expose handles.
  [consumer-tree-disposition — SHIPPED; extension-model]
- **DECIDED** — Lag-by-design ends for a converted consumer: when an engine change
  breaks one, the breakage is filed as tracked backlog at the consumer and never
  blocks the engine change.
  The showcase is kept current by convention, with no CI presence — a compile/import
  smoke check is a later ticket-level choice if rot appears. One exception, rare by
  design: an example serving as the deliberate canary of in-flight work is updated
  in-stream at the most appropriate time; a canary is normally planned as a separate
  path an example later adopts, so in-stream example surgery stays the exception.
  [consumer-tree-disposition — SHIPPED]

## Processor model & scheduling — SHIPPED (OPEN: resources across streams)

- **DECIDED** — A link is pure plumbing: output port → input port, carrying a message whose
  payload is bytes. The engine has no type layer and applies no encoding: ports carry no
  type declaration, connect never inspects or compares types and never warns, no read
  path examines a tag, and the frame header carries no schema ident. `write(port, payload:
  bytes, timestamp_ns=, starts_group=)` and `read(port) -> bytes` move bytes, with
  `read(port, into=T)` calling `T.from_bytes`; msgpack is a helper the stream package ships,
  `tatolab.stream.msgpack` — `pack`, `unpack`, and `MsgpackCast` and
  `msgpack_typed_dict_cast`, giving a dataclass or a TypedDict its `from_bytes` — never an
  encoding the engine or the runtime portion applies behind a call. A built-in writes bytes
  in the encoding its port's catalog entry names: the camera's frame record stays a msgpack
  record `VideoFrame` reads, and the encoders write the stack's media frame (§Media I/O).
  The envelope carries the group flag (§Networking), which Python and Rust readers see alike
  beside the stamp. Consuming is a cast at read time; a mismatch surfaces as a decode failure
  at the consuming processor. One carve-out, and only one: declaring an audio window contract
  **is** that port's opt-in to the engine reading its messages as `AudioBlock`, on the input's
  own declared contract and never in the forwarding path, so the engine inspects a payload on
  exactly the ports that asked it to. A link into a port with
  no contract is unchanged in every respect — still pure plumbing, `connect` still
  compares nothing, and the frame header still carries no schema ident. Owner, 2026-10-16
  and 2026-10-17. [schema-free-ports — SHIPPED #1814; audio-port-window-contract — SHIPPED
  #2033; moq-relays — SHIPPED #2821, #2850]
  Rejected: an encoding applied inside `write` and `read` — it stood between an encoder's port
  and a stock MoQ player (owner, 2026-10-16).
- **DECIDED** — A port declares three things, plus an optional window contract on an
  audio input and an optional catalog entry on an output, and nothing else: name,
  description, and — on an input — delivery profile, beside which an audio input may declare
  the window contract §Media I/O states; an output port carries its producer-written catalog
  entry — a hang entry, the section and that section's own fields — documentation `connect`
  never compares (§Networking, the catalog). Type
  information belongs to the authoring language and never reaches the engine: in Python
  the port method's return annotation is the declaration, read by humans and type
  checkers only, with `ctx.inputs.read(port)` yielding the payload's bytes and
  `read(port, into=T)` the opt-in strictness dial, casting the bytes through `T.from_bytes`
  and raising at read; in Rust the read target's `Deserialize` impl is the validation,
  always on, with no free-cast mode. [schema-free-ports — SHIPPED #1816, #1812; moq-relays —
  SHIPPED #2821, #2840]
  Rejected: a port declaring a shape or a convention name the engine compares — the 2026-09-10
  ruling, whose reason (agents on a mesh reading dynamically coded nodes) no longer exists;
  the entry is documentation, not a contract (owner, 2026-10-16).
- **DECIDED** — A read can name the inbound link it drained. Beside `read_raw`, a reader
  offers a read that returns the message, its stamp and the *inbound link* it arrived on,
  named by the source channel name the link subscribed to — `<lowercased producer
  processor id>/<output port>`, the name `graph` already shows. The mailbox
  already queued each frame holding its link's identity for drop attribution; this
  exposes the identity the per-link counters are keyed by, so no frame carries anything
  it did not carry before and counting is unchanged. In Python `LinkInputDataReader`
  gains the same read, in two spellings — `read_from_inbound_link(port, into=T)`, handing
  back the cast and the link name, and `read_from_inbound_link_with_timestamp(port,
  into=T)`, handing back the producer's stamp beside them — so a Python-authored
  many-input sink is possible rather than deferred, and one that restates a producer's
  timing downstream has the stamp to restate. A destination can also enumerate its
  inbound links at `setup()`
  (`inbound_link_names(port)`), which is how a sink learns how many tracks it owes. A message
  the port never enumerated a link for is refused by name rather than borrowing one.
  A helper-placed destination is *told* its link's name rather than deriving it: the wiring
  envelope's input entry carries `inbound_link_name` — the channel's own name — beside
  `channel_service_name` for every link, and `wire_input_link` takes it as a required
  positional — not defaulted,
  because a default meaning "the channel name" is the back-compat shim the doctrine bans and
  would make a silently wrong name reachable. A helper-protocol change, made safe by the
  build id above.
  Every read also tells what its link lost: `MessageReadFromInputPort` carries
  `lost_since_last_read`, the inbound link's dropped count less its value at this reader's
  previous message from that link, so a cumulative counter answers "since my last read" with
  no zeroing; Python's `read_message(port_name, *, into=None)` hands back a frozen
  `Message(payload, timestamp_ns, starts_group, lost_since_last_read, inbound_link)` from
  `tatolab.stream`, beside the four reads above. Owner, 2026-10-16.
  [opus-mp4-recording-rung — SHIPPED #2124; networking-extension-wheels — SHIPPED #2150;
  cross-runtime-links — SHIPPED #2287; moq-relays — SHIPPED #2830]
- **DECIDED** — The delivery profile is the whole of channel policy: one word, declared
  port-locally at the consuming input port. Every input port declares its delivery profile explicitly — there is no default
  and nothing left to infer one from, so an input port without one is a wiring error.
  Ring depth and overflow policy are engine-chosen and are not authorable: no port
  declares a depth, a leak policy, or a queue element, and there is no second surface
  that tunes one. [schema-free-ports — SHIPPED #1811; delivery-profile-vocabulary —
  SHIPPED #2024, #2025]
- **DECIDED** — The delivery profile names a read policy and nothing else. There are
  exactly two: `newest` — the consumer drains to the most recent message, older ones are
  passed over — and `ordered` — the consumer receives messages in publication order. Both
  drop under sustained pressure. Neither promises delivery, because on a link whose head
  is a device that will not wait, no port-local declaration can: backpressure only
  relocates the loss to the device edge. `lossless` is retired — the word promised what
  the runtime does not do. [delivery-profile-vocabulary — SHIPPED #2024]
- **DECIDED** — No loss is silent. A message dropped at a port is counted by the port that
  dropped it and is readable over the control plane in `graph`, alongside the processor's
  other metrics. Drops are counted per link, never as one blended total, so a future
  reflection of a link's count to its producer stays possible without recounting. A count
  is cumulative for the life of one wiring, not of the link id: disconnect takes it with
  the link and reconnecting the same id starts from zero, because a count outliving its
  link would name something `graph` no longer has. A drop is a normal, reportable event
  on a realtime link, never an error and never invisible — a run that lost most of its
  messages must not read as a healthy one. A `newest` port
  passing over messages to reach the most recent is the profile working, not loss at the
  port, and is deliberately uncounted. In the tree, the mailbox eviction, the subscriber
  ring's overwrite, the two receive-seam
  discards and a write refused at the channel ceiling are each counted where they happen and
  rendered under `metrics`, for a helper-placed processor as for an app-process one, and so
  is a message its reader counts lost after reading it. A
  `graph` that reports no drops is proof that none happened, with the two residuals the
  entries below state.
  [delivery-profile-vocabulary — SHIPPED #2023; loss-visibility — SHIPPED #2268, #2269,
  #2270]
- **DECIDED** — A message the iceoryx2 subscriber ring overwrites is counted. Every channel's
  data service carries a per-channel 64-bit sequence number in an iceoryx2 user header —
  never in the frame header, which stays exactly as it is. A number is consumed once a
  sample reaches the send, unless the send fails before delivering to anyone; a message refused
  at the ceiling or never loaned consumes none. Each subscriber keeps the last number it
  received per producing publisher, the first sample after wiring its baseline, so a
  restarted producer is never read as a gap; the numbers a jump skips — the new number
  minus the last, minus one — are added to that inbound link's dropped-message count, on
  `ordered` ports only, since a `newest` port passing over messages is the
  profile working. The number is engine-internal: no processor reads it and no message carries
  it. Six readings the build settled, none of them a mechanism the entry did not already
  state. **Per-channel is per producing publisher**: each publisher numbers its own sends
  from zero and each subscriber keeps its last number keyed by the publisher's id, so a
  publisher recreated when a source port's last link goes starts a new baseline rather than
  a gap. **"Fails before delivering to anyone" names exactly two send errors** — the two
  that fail before any delivery consume no number; the other three consume one, because
  iceoryx2 does not report a partial delivery, and a subscriber the failed send never
  reached therefore reads a real loss. **The header is one engine type**,
  `DataChannelBagUserHeader`, with its iceoryx2 type name pinned so a move
  never changes its identity, and a source-walking gate refuses a data `publish_subscribe`
  builder outside the node wrapper so no site can open a service without it. **A gap and an
  eviction never double-count**: a ring gap is a message never received and an eviction is a message
  received and then displaced, and each lands once on the link's one counter. **A `newest`
  port counts neither** — the eviction at a skip-to-latest mailbox is the profile working.
  And **a message
  dropped at the receive seam is counted on its link**: a frame too short for a header and a
  frame bound to a port with no mailbox each consume a number and reach no reader, so no gap
  could show them.
  Two stated residuals: a message lost after a link's last receive and before its disconnect is
  not counted; and messages the ring overwrites before a link's first receive are not counted
  either, because that first sample is the baseline the gap is measured from. Counts land on
  the next receive for a native and a Python consumer alike — a consumer inside a long
  `process()` receives nothing, so its overwrites reach `graph` when it next reads, and no
  timer is added to shorten that.
  [loss-visibility — SHIPPED #2268]
- **DECIDED** — A write refused at the channel ceiling is counted on the producer, per
  output port. The ceiling is per channel and the refusal happens before the message reaches any
  link, so it consumes no sequence number and no destination can ever see it; one counter at
  the single send seam serves a native and a Python producer alike, so a Rust author's `Err`
  is counted too, and `graph` renders `refused_bags_by_output_port: {port: n}` on the
  producing node. The cost is stated rather than hidden: a reader looking only at a
  destination's link does not see this loss. Rejected: adding the refusals into each
  outbound link's `dropped_bags_by_link` at its destination — it would need per-link
  baselines at the producer and a merge of two processes' counters at render (owner,
  2026-09-14). [loss-visibility — SHIPPED #2268]
- **DECIDED** — A message a writer declares lost upstream is carried in the user header beside
  the sequence number and added to an `ordered` reader's `lost_since_last_read`, never to its
  link's dropped count; the writer counts it on itself, per output port. Only engine-internal
  writers declare one. A loss declared after the writer's last write reaches no reader, the
  producer's count excepted. Owner, 2026-10-20. [relay-track-reading — SHIPPED #2900]
- **DECIDED** — A message its reader counts lost after reading it is its link's drop. A
  reader that took a message and found what it names gone — a frame its producer recycled
  before the claim — calls `count_the_message_last_read_as_lost(port)` on its inputs, one
  method in Rust and Python. It adds one to the dropped count of the link that delivered the
  port's last message, mirrored onto a helper's board like every count, and the next message
  read from that link carries it in `lost_since_last_read`. It counts on `ordered` and
  `newest` ports alike, because the reader took that message rather than passing over it.
  Each message counts once: a call with nothing read since the last count, a bag no link
  delivered, or a window is refused by name. The reader knows the message is gone; the
  engine only counts it, and still inspects no payload. Owner, 2026-10-24.
  [recycled-frame-is-a-counted-drop — SHIPPED #2933]
- **DECIDED** — A helper-placed destination's per-link counts reach `graph`. The parent
  creates one blackboard per helper spawn; the helper is its only writer, one entry per
  inbound link holding that link's dropped-message count, and the parent reads it whenever
  `graph` renders, without waiting on the child. The last counts a crashed helper wrote stay
  readable; a dead writer can no longer update its entries, so a respawned helper gets a
  fresh board. A helper's write refused at the ceiling is
  counted the same way. The node's `metrics` key then renders for a helper-placed processor
  as it does for an app-process one.
  Five readings the build settled. **An entry is a slot carrying its wiring**: a
  blackboard's keys are fixed at creation and links arrive live, so the board declares one
  slot per inbound link the cap allows, and the parent assigns each link a slot and a wiring
  generation carried in its setup envelope entry or its `wire_link`. Each value holds that
  generation beside the link's dropped messages and discarded samples, and the parent renders a
  slot only while its generation matches the link it assigned — so a late write for an
  unwired link is ignored and a reused slot starts from zero, which is what makes a count
  cumulative for the life of one wiring rather than of the slot. An output port's entry
  carries a generation too, assigned per channel, so a reopened port never renders the total
  of the channel before it, including while the helper has not yet answered. **The board is
  named per spawn, never per processor**: the parent creates it before the child starts and
  holds its creator and reader, dropping them at processor removal and never at helper
  death, so a crashed helper's last counts render until the node goes. **The helper writes
  at the seam that moves the count** — mirrored into its slot as the counter increments, on
  the helper's own receive, with no loop flush and no timer. **The reader is wrapped for
  `graph`** and read lock-free under the graph lock, so `graph` never waits on the child.
  And **the flush count rides the same slot**, because a windowed port on a helper is where
  both losses happen together.
  `tatolab.stream`'s `NodeLinkDataAccess` Protocol carries `open_loss_count_board` and one
  optional keyword on each of its two link-opening methods, which the helper passes and no
  processor author ever names. The engine half of the
  proof is CI-run; the end-to-end wheel arm — an overrun helper, a ceiling refusal and a
  SIGKILL'd helper's last counts, all read off a running node's `graph` — is
  `requires_gpu`, run on the Linux CI lane on the bundled software device.
  [loss-visibility — SHIPPED #2270]
- **DECIDED** — No link ever blocks a producer: no profile resolves to producer-blocking and
  no overflow policy parks a producer. A processor publishing to a slow consumer loses messages at
  that consumer's port, counted as the entry above states; it is never parked. A parked
  producer cannot observe shutdown.
  [delivery-profile-vocabulary — SHIPPED #2025]
- **DECIDED** — Loss-handling knowledge lives at the link's endpoints, never in the
  engine. The engine's whole role is to count a drop at the port that dropped it and
  surface it; it never inspects a payload — save on a port whose window contract is
  exactly that opt-in — never knows a message holds a reference frame, and never acquires a
  drop rule that depends on content. The carve-out buys no drop rule either: the windowing
  stage reads samples, never a frame's role in a stream, and a windowed port drops on the
  same counted-mailbox terms as every other port. A producer that can make loss
  cheaper reacts at the source — an encoder under downstream pressure declines to encode
  raw frames and resumes at its next sync point, which is where loss belongs and costs
  least. A consumer on an encoded stream must bound loss — this is a requirement, not an
  option: a consumer that sees a gap discards until the producer's next sync point, and
  never commits or forwards a stream it knows is broken. No consumer drops or passes on
  encoded frames blindly. The information that makes both possible travels as
  ordinary message fields the producer writes and the consumer casts, never as a tag in the
  frame header and never as engine-visible type. [delivery-profile-vocabulary]
- **OPEN** — Reflecting a link's drop count back to its producing port, so a producer
  can react to pressure it cannot otherwise see: intended, do not build until the first
  encoded-domain link exists — nothing in the tree reads it before an encoder does.
  Direction — only drops at `ordered` inputs (a `newest` input passing over messages is the
  profile working and must never throttle a producer); rides the link's own notify path,
  never the control plane; a read-only count the producer polls, no callback, no
  configuration dial. The per-link counting decided above is the only piece today's work
  must honor. [delivery-profile-vocabulary]
- **DECIDED** — There is no schema-first layer on ports: no JTD, no schema registry the
  engine consults, no codegen from a schema, no generated type classes, no schema
  identity grammar, nothing a port declares and nothing `connect` compares — anywhere
  in the engine or the authoring surfaces. A JSON Schema *derived from* a type the
  author already wrote and served as documentation is not that layer: it is
  code-first, it names nothing on a link, and no engine path reads it. The entry below
  is the only such schema. [schema-free-ports — SHIPPED #1813, #1815;
  processor-class-identity — SHIPPED #1841; agent-readable-processor-catalog — SHIPPED
  #2224, #2226]
- **DECIDED** — A processor's config shape is a JSON Schema derived from its config
  type, carried on its descriptor and rendered by the control plane in the processor
  catalog. In Rust the `config =` type derives `JsonSchema` — the SDK re-exports the
  derive so a processor crate adds no dependency — and the `#[processor]` macro refuses
  a config type without it, naming the fix. In Python a processor's config is one
  class, named by the annotation on its `__init__`'s `config` parameter: any class
  constructible from the config's keys with annotated fields — a TypedDict or a
  dataclass yields the schema from its annotations and defaults; a model that carries
  its own schema contributes it — and the helper constructs that class from the
  configuration and hands the object in. Keyword-argument configuration is deleted,
  not kept beside the class form. A processor that takes no config declares none and
  refuses one. Reconfiguration takes the same object.
  [agent-readable-processor-catalog — SHIPPED #2224, #2226]
- **DECIDED** — The Rust half, as built. `ProcessorDescriptor.config_schema` is
  `Option<serde_json::Value>` — the config type's document, `None` only on a descriptor
  built by hand without one — and `ProcessorDescriptorOutput` mirrors it, so
  `/api/registry` serves the same document the MCP catalog does. `#[processor]` emits it
  through `ProcessorConfigJsonSchema`, a blanket trait over `schemars::JsonSchema` whose
  `#[diagnostic::on_unimplemented]` note names the derive and the SDK's re-export at
  `streamlib::sdk::schemars`, so a config type without the derive fails on a message
  naming the fix rather than on a bare trait bound. `EmptyConfig`, what a processor with no
  `config =` gets, publishes an object with no properties and `additionalProperties:
  false`, and refuses a non-empty map naming the key that had nowhere to go. Every
  in-tree config type derives the schema — the built-ins' configs and their enums — and a
  field's `///` doc is its `description`, a serde default its `default`, a field with neither
  `required`. Every catalog document, from either language,
  is JSON Schema draft 2020-12
  with no `$schema` key: schemars 0.8 emits draft-07, so the Rust seam writes its
  definitions under `$defs` with references pointing there and a tuple's positional
  schemas as `prefixItems`. The two languages keep two spellings that are both valid
  2020-12 — Rust writes a nullable as `"type": [T, "null"]` and stamps a root `title`,
  Python writes `anyOf` with null and no title — and neither is converted.
  [agent-readable-processor-catalog — SHIPPED #2224]
- **DECIDED** — The Python half, as built. `@node` reads `__init__` through
  `typing.get_type_hints(..., include_extras=True)`: a `config` parameter annotated with
  a class names the config class; an `__init__` taking nothing beyond `self`, or none at
  all, declares no config; every other signature — a keyword parameter or several,
  `*args` or `**kwargs`, a positional-only or unannotated `config`, an annotation that
  is not a class (a parameterised generic, `Any`) or cannot be resolved — is refused at
  decoration naming the class, the parameter and the fix. The class and its schema are
  stamped as `__tatolab_node_config_class__` and
  `__tatolab_node_config_schema__`. The deriver is stdlib-only and emits 2020-12
  directly, inlining nested classes and never writing a `$ref`: a TypedDict yields its
  annotations, inherited keys included, and `required` from `__required_keys__`; a
  dataclass yields its constructor's inputs — its `init=True` fields and `InitVar`s, a
  `default_factory` field optional with no default, and a default the wire cannot carry
  dropped rather than rewritten — with `additionalProperties: false`, since a dataclass
  refuses an unknown key and a TypedDict does not; `Annotated[T, "text"]` is a
  description; a class exposing `model_json_schema()` contributes that document minus
  `$schema` and `title`, recognised by the method rather than by importing pydantic; an
  annotation the deriver does not know renders `{}`, a config class of a kind it cannot
  read is accepted with an open schema, and a self-referential class stops at an open
  object rather than recursing. The helper constructs
  `processor_class(config=config_class(**configuration))` and whatever the config class
  raises is what the author sees; a class declaring no config takes an empty
  configuration and refuses a non-empty one by name; reconfiguration calls
  `configure(config_class(**configuration))`. Construction is the only check — the wheel
  carries no validator, and how strict it is stays the author's choice of config class.
  On the wire, `stream_builder.add(cls, config={…})` carries a dict, the graph node stores its JSON, and
  `ctx.config` is that mapping. [agent-readable-processor-catalog — SHIPPED #2226]
- **DECIDED** — Port rendering in the control plane is name, description, delivery
  profile, direction, on an audio input that declared one its window contract, and on an
  output that carries one its `catalog_entry`, in `graph` and `/api/registry` alike; no port
  carries a type in `graph` or any snapshot. A port that declared nothing
  renders no `audio_window` key at all rather than a null. A `match_device` port renders
  the five values its device settled — machine-dependent because the device format is,
  which is truer than a static lie — and renders the sentinel itself while nothing has
  settled it. [schema-free-ports — SHIPPED #1816; audio-port-window-contract — SHIPPED
  #2032, #2034; moq-relays — SHIPPED #2840]
- **OPEN** — What a port reports about the messages it produces or accepts, and how a message
  shape is described and served at all — by any author, the built-ins included — so an
  agent can wire a custom processor it did not write: an input that only receives and
  may be fed several shapes, an output that may write a duck-typed message, nothing
  required, and a form that survives a transport where no link names the producer.
  Undecided; the shapes considered and set aside are recorded in
  `docs/research/2026-09-10-bag-shape-hints-for-agents.md`. Do not build; ports render
  exactly as the entry above states until this closes. [agent-readable-processor-catalog]
- **DECIDED** — Three execution modes (reactive / manual / continuous); one dedicated
  OS thread per processor with descriptor-driven priority (realtime / high / normal);
  synchronous lifecycle traits; Full/Limited capability typestate on the phase axis
  (setup/teardown vs process). [execution-model; reopened by one-runtime-per-machine: scheduling across streams]
- **DECIDED** — Reactive and continuous execution pace the same on both floors. A reactive
  processor waits on one readiness queue — epoll on Linux, kqueue `EVFILT_READ` on macOS,
  level-triggered on both, with one 500 ms bound — holding its inbound listener and its
  shutdown wake (an `eventfd` on Linux, a close-on-exec pipe written once on macOS), so a message
  or a shutdown wakes it on arrival. The 100 ms channel-poll loop survives only as the
  fallback for a platform with neither queue or a waiter whose setup failed, and a wake that
  cannot be created leaves channel-only shutdown rather than a panic. `MonotonicTimer`, which
  a node starts with `start_monotonic_timer(interval_ns)`, runs on both floors: `timerfd` on
  Linux; on macOS a one-shot
  kqueue `EVFILT_TIMER` with `NOTE_MACHTIME | NOTE_ABSOLUTE | NOTE_CRITICAL`, re-armed after
  each fire at the next absolute deadline, `first + k·interval` on `MediaClock`, converted to
  Mach ticks and rounded up so a deadline never fires early. `wait()` returns the deadlines
  passed, `0` on timeout and `-1` once closed, as on Linux.
  Three readings the build settled. **`NOTE_MACH_CONTINUOUS_TIME` is never set** — only
  without it is an absolute `NOTE_MACHTIME` deadline in the `mach_absolute_time` epoch, the
  engine's clock. **`NOTE_CRITICAL` is load-bearing**: without it the kernel's coalescing
  makes wakes about 1.3 ms late on average. **Two waiters on one timer differ by floor, both
  bounded**: on macOS the one that misses the one-shot waits until its own timeout or the next
  deadline, on Linux it returns `0` at once; the helper waits from one thread.
  [macos-capability-parity — SHIPPED #2408, #2409]
- **DECIDED** — Helper-process placement is the only execution placement. Every Python
  processor runs in its own child process — its own interpreter, its own GIL — spawned
  by the Rust engine as an exec of the stream's own venv interpreter with the runtime's
  lend directory, which holds `tatolab/runtime/`, first on its `PYTHONPATH`: never fork
  (GPU contexts are fork-unsafe), never `multiprocessing` or a worker pool (the engine
  owns the child's lifecycle from its compiler ops and needs no GIL to manage it).
  In-process hosting of a Python processor does not exist — not as a default, a
  fallback, an optimisation, or an engine choice. Isolation, not latency, is the
  optimised axis: no processor may ever block, stall, or degrade another. Same user
  code, one venv per stream — each stream's interpreters start from its own project's venv
  inside the one runtime process — no per-processor environments, no placement surface of
  any kind.
  A child imports the lent `tatolab.runtime`, never a copy in its venv — one native
  artifact. Every processor class
  must be import-addressable from a module whose import is side-effect-safe; there is
  nothing to equalize and nothing to move between, because there is no second
  placement. [helper-process-placement-only — SHIPPED #1714; package-split-and-lend —
  SHIPPED #2689; runtime-hosting — SHIPPED #2704, #2711]
- **DECIDED** — A surface crosses to a helper on Apple over raw Mach. The surface-share
  service above the transport does not change: its verbs, its per-slot-not-per-frame shape,
  the checkout lease and the retired-frame refusal are one platform-neutral core both arms
  call (`surface_share_wire_verbs`). On Linux the transport is a Unix socket with
  `SCM_RIGHTS`; on Apple each request or reply is one complex Mach message — port descriptors
  plus a length-prefixed JSON payload — with a surface crossing as an IOSurface Mach port.
  iceoryx2 stays the data plane on both. There is no XPC, no launchd service, no plist and no
  bundle. Rendezvous is a dynamic `bootstrap_check_in` name,
  `com.tatolab.streamlib.surface-share.<runtime id>`, registered at construction and refusing
  a live duplicate as the socket bind does; the child receives it in
  `STREAMLIB_SURFACE_MACH_SERVICE`. The name is listed in the user's launchd domain, so the
  peer check is the gate, not defence in depth: a connect is admitted only for the engine's
  own process or a pid its spawner admitted right after the spawn and keeps admitted until the
  child is reaped; the pid version, from the kernel audit trailer, is pinned at first contact
  and checked on every message; and each admitted connection gets its own unlisted request
  port. Surfaces are private and never `kIOSurfaceIsGlobal` — a global surface is readable by
  any process on the machine — so the port carries the surface and never the id. A client's
  death arrives as a dead-name notification and releases its checkout leases, and its
  registrations too if it was another process. A pooled pixel-buffer slot is a private
  IOSurface imported as the slot's buffer; it carries no timeline on either floor, and the
  pool rehands it only when no lease holds it and the kernel reports its surface not in use —
  a Mach port in flight or a use count in any process; a helper raises the use count while it
  holds a frame, and the kernel clears it when that process dies.
  Four readings the build settled. **The admission wait is bounded**: a helper's connect can
  arrive before its spawner learns its pid, so an unadmitted connect waits up to 5 s, at most
  64 at once, and is then refused and given no port. **A killed holder's in-use answer clears
  promptly but asynchronously** — 100–400 µs after the reap under load — so a slot is skipped
  for one more acquire, never rehanded early. **A cached `IOSurfaceRef` does not pin a
  slot**: the helper caches one import per pool slot, raises the use count only while it holds
  a frame, and empties the cache when the service dies. **A Mach request has no response
  timeout** (the Linux socket has 10 s); a pending request wakes when the service dies, never
  on a timer. The capability-secure rendezvous, an unlisted `mach_ports_register` stash,
  needs helpers spawned by `posix_spawn`, which the spawn host's `pre_exec` closures prevent;
  that move is #2368, its own change with its own Linux proof.
  [macos-platform-floor — SHIPPED #2360, #2361]
- **DECIDED** — Where a surface carries a cross-process timeline on Apple, it carries it as
  Metal shared events, and no engine GPU wait is ever on a value a peer signals. The pair
  crosses with every texture a helper acquires through the escalate path: the engine mints
  `produce_done` / `consume_done` exportable (`VkExportMetalObjectCreateInfoEXT`), each
  exports as a Mach send right — through a subclass of the public `NSXPCCoder` that captures
  the one port `MTLSharedEventHandle` encodes — and rides the Mach channel beside the IOSurface
  port, and the helper imports each at `initialValue = 0`, which joins the producer's value
  because a shared event ignores a decrease. Values advance and are never reset. A pooled
  frame carries no timeline on either floor: publication orders producer to helper, and the
  lease plus the kernel's in-use answer order reuse (owner, 2026-09-22). A wait for a helper's
  release is a host wait of at most `CROSS_PROCESS_TIMELINE_WAIT_BOUND` (2 s), well under the
  ~5 s after which IOGPU kills an unsatisfied command buffer and MoltenVK loses the device for
  good; past the bound the engine signals the value itself and names the frame stale.
  Host-side ordering is the same pair's fallback, chosen at runtime and never at build time:
  the producer host-waits its own GPU work before the hand-off, and the helper's release
  returns over the channel as `signal_consume_done`; a pair that falls back never reverts.
  Four readings the build settled. **A texture crosses only with both edges**: one whose pair
  will not export is refused at registration, and a helper whose import refuses fails the
  check-out naming the edge, because a consumer outside the pair is an unsynchronised reader —
  so the host-side fallback is reached only through a pixel-buffer registration carrying a
  pair, which no production path makes today; tests prove it. **Nothing signals
  `produce_done` yet**: every escalate GPU op retires its work before any consumer learns the
  id; the helper holds the edge for a producer that later starts signalling, and signals
  `consume_done` host-side once, at release. **A release reported past what was produced is
  refused**, and a reported signal and a forced one are serialised, so a late report after a
  forced release is harmless. **The engine's device-side waits on a peer stay Linux-only**
  (`copy_texture_to_storage_buffer_and_signal`); none is reached on macOS.
  [macos-platform-floor — SHIPPED #2360, #2361; macos-capability-parity — SHIPPED #2401,
  #2402, #2404]
- **DECIDED** — The helper's importer is `streamlib-consumer-rhi` on both floors, and it stays
  Vulkan. Metal appears only as exported handles at the boundary, never as an API a helper
  writes against — owner, 2026-09-22, over a Metal-direct importer, which would be a second
  system beside the consumer RHI. The measured cost is inside the helper startup budget: a
  MoltenVK device takes ~0.5 s cold and 26–70 ms warm per helper. The MoltenVK arm opts its
  instance into portability enumeration, carries a per-platform required-extension list —
  `VK_EXT_external_memory_host` on macOS, the DMA-BUF and fd set on Linux — and treats
  `VK_EXT_metal_objects` as optional. A pooled pixel buffer imports as a
  `ConsumerVulkanBuffer` over the IOSurface's own pages by host-pointer import, through the
  same create/bind/map core the fd imports use; a texture imports as a
  `ConsumerVulkanTexture` through `VkImportMetalIOSurfaceInfoEXT` — `OPTIMAL` tiling, bound to
  a device-local memory type that is not host-visible, chosen by query, one MoltenVK contract
  (`iosurface_backed_image.rs`) shared by the engine's allocation and the helper's import; a
  timeline edge imports as a `ConsumerVulkanTimelineSemaphore` through
  `VkImportMetalSharedEventInfoEXT`. The Mach channel carries what the Unix socket carries: a
  texture registration crosses whole — its `vk_image_*` recipe, its layout cell and its two
  timeline ports beside the IOSurface port — within the four ports a message reserves;
  `lookup` and `check_out` echo all of it, and `update_layout` exists.
  [macos-capability-parity — SHIPPED #2361, #2401, #2402]
- **DECIDED** — Shutdown always ends, and a cooperative processor's `teardown()` always
  runs. The engine stops every helper at once, never one after another, each on the same
  ladder: `stop` and `teardown` are sent together; a Python callback still running after one
  second is interrupted with `KeyboardInterrupt`, after which `stop()` and `teardown()` still
  run and the message in flight is lost; `teardown()` then has five seconds; the helper's whole
  process group is terminated, then killed; and the child is reaped, or abandoned and named.
  Native code a callback is inside is interrupted only when it returns. Budgets are
  engine-chosen and not authorable. A processor's descendants die with it: its process group
  goes at every helper exit — shutdown, removal, or a crash the engine detects by the process
  itself rather than by its socket — and a helper inherits no descriptor beyond its escalate
  socket and its standard streams, which are pipes the engine reads, never the app's own
  output. At every helper exit the engine shuts its end of the escalate socket and stops
  waiting on those pipes, so nothing the helper started can hold the app's output open, delay
  the app's exit, or keep issuing the helper's privileged operations. A descendant that leaves
  the process group on purpose is the stated residual: it survives, holding none of the app's
  descriptors and reaching no engine operation.
  Seven readings the build settled, each binding where an implementer would otherwise choose
  inline. **Any** Python callback interrupted at shutdown is followed by `teardown()`,
  `setup()` included — a `setup()` that raises by itself keeps the no-teardown rule it
  already had, and a `teardown()` touching state `setup()` never built raises and is logged
  like any hook failure. The second interrupt gives a Python helper no `teardown()` at all:
  it terminates the helper's process group. A live `remove_processor`
  whose native thread outlives its budget still removes the processor — the node and its
  links go, the thread is abandoned, and the call fails naming it, so the caller learns the
  change did not end cleanly. The engine's ends of a helper's stdout and stderr are detached
  at its exit rather than closed, so a surviving `setsid` descendant's writes are still
  logged rather than raising SIGPIPE, and each reader thread lives only while a survivor
  holds its pipe. And the ladder runs on both floors: process groups, `waitid` and
  CLOEXEC-at-source compile on both, with macOS closing descriptors one at a time where
  `close_range` is absent until the spawn moves to `posix_spawn` (#2368). On macOS SIGINT and
  SIGTERM escalate the same way, from handlers installed once for the process's life; SIGHUP
  is not owned there and no disposition is handed back — named platform differences in the
  test closed list, not gaps. **A helper never outlives its app on either floor.** On Linux
  the kernel ends it: the spawn host sets `PR_SET_PDEATHSIG` to `SIGKILL`. Darwin has no such
  signal, so a macOS helper arms its own watch at boot, before its processor's module
  loads — kqueue `EVFILT_PROC` with `NOTE_EXIT` on the parent's pid,
  belt-and-braces with a dead-name notification on a boot-time connection to the
  surface-share service. Whichever fires first shuts the escalate socket, so the helper reads
  the end of its channel and runs the `stop` and `teardown()` the engine can no longer send,
  while a thread that needs no GIL walks the ladder's own budgets: it interrupts a callback
  still running after one second and kills the process group of whatever is alive at 6.5 s,
  skipping the terminate rung because the teardown budget is already spent. A `SIGKILL`ed app
  therefore leaves no helper on either floor, and on macOS the helper's `teardown()` still
  runs. **A third interrupt exits 130 on both floors**: the run loop parks for good while the
  process is being ended at once, so killing the helper groups can no longer let the run
  return inside the log-flush grace and exit 0. The runtime itself is started by the per-user
  service its installer registers and never detaches.
  [shutdown-ladder; local-transport-hardening — SHIPPED #2264, #2266; macos-platform-floor —
  SHIPPED #2357; macos-capability-parity — SHIPPED #2410; runtime-hosting — SHIPPED #2721,
  #2722]
- **DECIDED** — A link onto a helper reads `wired` only once the helper says so. The helper
  answers every `wire_link` it receives with a link-scoped `wired` or `wire_failed` reply,
  `wire_failed` carrying the reason its open failed; the reply rides its own rpc tag, which
  the bridge routes to that link's state and never to the lifecycle reply queue. A link
  carried in the startup envelope needs no reply of its own — `ready` confirms it, and a
  failure there still refuses the processor's start by name. `connect` does not wait for the
  reply: it returns with the link `pending`, and the helper's answer flips it to `wired`, or
  to `error` carrying the helper's own reason, which `graph` renders under `error_reason`
  until the link is disconnected. The caller learns
  whether its change took by reading `graph`, as the MCP instructions already tell it to,
  and those instructions carry the `pending` and `error` cases. A bounded wait was rejected:
  a helper reads commands only between callbacks, so waiting would put a control-plane call
  behind user code, and the isolation axis settles it over the caller-learns-its-outcome
  one. A helper that dies with a link unconfirmed takes the link down on the same death path
  the ladder runs; the link never reads `wired`. A link an engine-to-helper wire has not yet
  confirmed is never re-planned as unadded.
  [local-transport-hardening — SHIPPED #2265]
- **DECIDED** — A helper refuses to start unless the engine it imported is its parent's
  build. The build id is the crate version, the git sha — `unknown` where the build has no
  `.git` — and a per-build nonce, compiled into `_engine.abi3.so` and `tatolabd`:
  `cargo xtask build-runtime` mints one and hands it to both compiles of a runtime unit (the
  lend's `_engine` and `tatolabd`) through `STREAMLIB_RUNTIME_UNIT_ENGINE_BUILD_NONCE`; any
  other build's engine build script mints its own. The parent passes its own in the helper's
  environment; the helper compares before it opens any channel or socket, and on a mismatch
  writes a refusal naming both ids to raw stderr and exits, so the parent reports that
  processor's start as refused and names the helper's stderr. An absent id is a refusal too,
  never a silent pass.
  The lend gives a processor interpreter its runtime's own build by construction; the check
  stays as the backstop.
  Rejected: a hand-bumped subprocess protocol version beside the build id — it never caught a
  helper built against a different iceoryx2 patch or a stale wheel on the helper's `sys.path`
  (local-transport-hardening, 2026-09-14).
  [local-transport-hardening — SHIPPED #2262; package-split-and-lend — SHIPPED #2689]
- **DECIDED** — The MVP edit loop is re-running `dev` (warm restart is sub-second by
  construction). Reload-on-save is a nicety, not MVP-gating, and when built it is
  processor-granular — stop the processor, respawn its helper (a fresh interpreter
  re-imports the class), rewire its ports — never module-loading machinery.
  [importable-python-library — SHIPPED #1711]
- **DECIDED** — A processor's identity is its class, named by its fully-qualified
  import path (`my_app.filters:BlurProcessor` in Python, the type path in Rust; a built-in
  by its class's import path in `tatolab.stream`, the entry below) —
  derived mechanically, never authored, and the same string in the registry, in the
  control plane's type field, and for spawning the processor's helper process — which
  is how every Python processor runs. A processor defined in the entry file run as
  `python <script>.py` identifies as `__main__:<Type>` and is a wiring error at `stream_builder.add`,
  with an error naming the fix (move the class to an importable module and import it
  from the entry file — one import line). The entry file itself may still run as
  `__main__`; only processor classes may not live there.
  `@node` declares execution, interval, scheduling
  priority, and description only. Mechanically means at the authoring seam, never from a
  runtime reflection API: Rust captures the type path where the macro expands, because
  `std::any::type_name`'s output format is unspecified across compiler versions and must
  never key a registry; Python joins `__module__` and `__qualname__` with a colon. Nothing
  derives a per-processor isolation tier: every processor runs at the one tier the engine
  assigns. The `FullAccessGrant` moat is a compile-time guarantee
  about who may mint an in-process `RuntimeContextFullAccess`, never a placement question.
  [processor-class-identity — SHIPPED #1837, #1839, #1840, #1841;
  helper-process-placement-only — SHIPPED #1714; built-in-node-type; package-split-and-lend —
  SHIPPED #2684]
- **DECIDED** — A built-in node's type is the import path of its class in `tatolab.stream` —
  `tatolab.stream:CameraSource` — the same string in the runtime's registry, on the stream
  library's class, in every graph and over the local API, from Python and from Rust alike. The
  runtime registers each built-in under that string; no table translates it to another name,
  and a built-in's Rust module path never reaches a graph. Every node's type is thus the
  import path of the class a stream names: a type under `tatolab.stream` is one of the
  runtime's built-ins, and any other is imported in the stream's own interpreter. A type
  changes only when its class's public name does — renaming or moving the runtime's crates
  and modules never changes one — and the first golden graph a runtime is held to already
  carries it. Owner, 2026-10-02. [built-in-node-type; package-split-and-lend — SHIPPED #2684]
- **DECIDED** — `@node` registers nothing: it writes the class's declaration stamps and
  returns the class, so a declaration is readable with no engine anywhere. A Python node class
  reaches the node catalog when a graph that names it loads — or a live `add_node` names it —
  and the runtime describes it in the stream's interpreter, registering its descriptor and its
  constructor together.
  [agent-readable-processor-catalog — SHIPPED #2228; package-split-and-lend — SHIPPED #2686,
  #2689]
- **DECIDED** — A node's name is the human-facing label — passed at `add`, readable off the
  returned `NodeReference`, and the prefix on its log records; it defaults to the class's
  short name. It is also the node's part of its port address (§Networking, the address), so
  renaming a node re-addresses its ports. Being part of that address is what bounds it: a
  name is cast to the address grammar, never refused for its spelling, and is unique in its
  graph once cast. A defaulted duplicate takes the next unused `-2`, `-3` …; a duplicate the
  author typed — two typed names casting alike — is refused by name and adds nothing, at the
  builder's `add` naming both on the author's own line, and at the engine's live add. The
  builder resolves every name before the graph is emitted, because only it knows a typed
  name from a defaulted one; the engine applies the same cast, `-2` and refusal to a live add,
  one rule at two seams proven on the same inputs.
  Identity is never derived from it — and neither is the default: a descriptor carries
  the class's short name as its own validated field rather than the engine splitting one
  out of the import path, because splitting would re-invent an identity grammar.
  [processor-class-identity — SHIPPED #1838, #1841; runtime-mesh — SHIPPED #2282;
  stream-graph — SHIPPED #2614, #2618; runtime-hosting — SHIPPED #2715]
- **OPEN** — Additional execution flavors to scale processor count (lightweight /
  green-thread style): intended, do not build until designed; hard constraint — no new
  configuration dials. [execution-model]
- **DECIDED** — A graph is data: a stream's `@stream` function compiles to the stream's graph, a
  serializable description a runtime runs — nodes by class import path with their config and
  name, the links, and what the stream exposes. It is one shape with what `graph` renders,
  never a second format: the spec keys are `stream`, `nodes` (each `name`, `type`, `config`),
  `links` (each `source` and `target`, an end in this stream being `{node, port}` by the
  node's resolved name, and a source in another stream on the machine naming that stream too)
  and `exposed` (each `node`, `port`, `level`), and `graph` renders exactly these with its
  live keys — `id`, `ports`, link state, counters — beside them. A load reads the spec keys
  and skips the live ones, so `graph`'s own output is a loadable graph; there is no saver,
  because the render is the export. A load refuses by name a node name two nodes cast to, an
  unregistered `type`, an end in this stream naming a node or port the graph does not hold,
  and an exposure naming a node or an output the graph does not hold; a source in another
  stream is that stream's to hold and is not looked up here.
  [one-runtime-per-machine; stream-graph — SHIPPED #2614, #2618, #2708]
- **DECIDED** — The graph is emitted, never authored. A stream's function produces it, the
  runtime exports the live one in the same shape, and nobody writes one by hand as the source
  of a stream — a stream's source is its Python, which keeps the retired manifest retired. On
  the next start the function wins: an agent that edits a running stream over the local API
  changes the live graph, and keeps the change only by changing the code (owner, 2026-09-30).
  "The function" is the graph it compiled to at its last load: a kept stream re-loads that
  recorded graph, and picking up a changed source is another `run -d` (§Product, how a
  stream is loaded and kept). The only writers of a graph are the builder and the render; a
  load accepts a document because the render is one, and nothing documents hand-writing it.
  [one-runtime-per-machine; stream-graph — SHIPPED #2614, #2618]
- **DECIDED** — The runtime refuses by name what a graph holds that it does not understand,
  at load and before a frame flows. Every built-in's config takes
  `#[serde(deny_unknown_fields)]`, and the refusal names the node, its `type` and the setting;
  a `type` the runtime lacks is refused naming it; a built-in absent on this floor is refused
  naming the floor (`VirtualCameraSink` is refused at load, not at `stream_builder.add`); and
  a graph key the loader neither reads as spec nor knows as one of `graph`'s live keys is
  refused naming it, the live keys being skipped. A golden graph holding every key and every
  built-in `type` is checked in and loads on every later build; a shape change adds a golden
  beside it and never edits one. [package-split-and-lend — SHIPPED #2671]
- **DECIDED** — A stream's environment is its project directory and that directory's venv
  interpreter. It is recorded beside the graph when the stream is loaded — never inside it, so
  the same graph loads from another checkout — and every processor interpreter of the stream
  starts from it, its working directory the project. The runtime adds nothing per stream and
  nothing from the caller — no `.env`, no `-e`, no caller's shell; a stream's compile, describe
  and processor interpreters inherit the runtime's own environment minus every `PYTHON*`
  variable — and a stream reads its own settings from its project as any program does.
  Provisioning an environment is the packs OPEN in §Packages. Owner, 2026-10-02.
  [package-split-and-lend — SHIPPED #2689, #2693; runtime-hosting — SHIPPED #2711]
- **DECIDED** — Spawn and describe live in the engine, beside `subprocess_bridge.rs`, and
  run no Python in the runtime process. A loaded stream's environment is recorded as
  `StreamEnvironment { project_directory, interpreter }`. A processor interpreter's command is
  `<interpreter> <lend>/tatolab/runtime/_processor_interpreter_bootstrap.py`, run by path and
  never `-m` or `-c`; `PYTHONPATH` is the lend directory, then the project directory; every
  other `PYTHON*` variable is removed; the working directory is the project; the `STREAMLIB_*` variables
  are unchanged. The bootstrap first drops its own directory from `sys.path`, then imports
  `tatolab.runtime` — an interpreter that cannot load it (not CPython, below 3.10,
  free-threaded, a foreign architecture) writes a refusal to raw stderr naming its path,
  implementation, version, free-threading, architecture and the import error, and exits —
  then checks the build id, then runs its node. `tatolabd` finds the lend relative to its own
  executable, at `../lib/tatolab/lend`, or at `Contents/Resources/lend` inside `Tatolab.app`. Describe is the same command with `--describe <import
  paths>`, run at load for every Python `type` a graph names and at a live `add_node` for one
  not yet described; the bootstrap prints each declaration as JSON, and a type that will not
  import or carries no stamp is refused by name, quoting the interpreter's stderr. It costs one
  interpreter start per load and per live add of an undescribed type. [package-split-and-lend
  — SHIPPED #2689; runtime-hosting — SHIPPED #2711, #2721]
- **OPEN** — What the graph holds beyond nodes, links and exposures: a stream's needs.
  Direction (review, not decided): a camera, a microphone, the accelerator, network
  exposure — derived from its nodes' declarations (built-ins carry theirs; a user node that
  opens a device directly says so on `@node`) and carried in the graph, never authored; the
  runtime requests exactly those at load and refuses by name what the machine cannot grant, so
  nothing enumerates every possible device up front. Decided with the resources entry below.
  [one-runtime-per-machine]
- **DECIDED** — Several streams in one runtime process: one engine, a table of streams.
  `Runner` is the engine — `tatolabd` builds one, and the library type stays constructible so
  the runtime suite runs many per test binary. It makes once, for every stream: the
  `GpuContext` (so `VULKAN_DEVICE_FOR_IMPORT` names the only device), the iceoryx2 node, one
  tokio runtime, the surface-sharing service, one logging subscriber, signal ownership, and the
  local API, which it hosts itself. Each stream is a `LoadedStreamInThisRuntime`, keyed by its
  cast name: its compiler (graph, transaction, commit lock), status and graph-change listener,
  and a `RuntimeContext` sharing the engine's GPU, node and tokio. Per stream: events, which
  carry their stream on a per-stream topic; the Python node types, described in its own
  interpreter, while built-ins register once for the machine and the node catalog lists both;
  its log file, records routed by stream; shutdown and its escalation, the machine's signal
  ladder walking every stream at once; the teardown watchdog; exposures; the virtual camera's
  stable id; and the pipeline cache. The process-group table of processor interpreters stays
  one lock-free machine table, each slot tagged with its stream — so one stream's shutdown,
  crash budget or graph change never touches another's. A stream's watchdog, on expiry, kills
  that stream's groups, abandons its threads and unloads it, never recording it `failed`; past
  an engine-chosen bound of abandoned threads in the process, the runtime exits 124. A stream
  reads another stream's private or public port on the same machine through the local relay's
  loopback listener and the relay source node (§Networking); no link crosses a stream's edge.
  `tatolabd` keeps once for the machine the exposed-port publisher, the relay adapters and
  the local relay's two children (§Networking). Surfaces are shared across every stream's
  processor interpreters on both floors, all being
  the runtime's children. Streams needing conflicting Python
  packages each start from their own venv (the package split and the lend, §Packages). Owner,
  2026-10-01; the surface clause confirmed 2026-10-02. [one-runtime-per-machine;
  runtime-hosting — SHIPPED #2704, #2715; moq-relays — SHIPPED #2842, #2853, #2855]
- **OPEN** — Resources across streams: requests and limits, realtime priority across streams,
  admission control, and how a stream states what it needs. Direction (review, not decided):
  what a stream needs is read from its graph (the derived needs above) and granted per stream
  at load by the runtime — on Apple behind the OS prompt, on Linux by the runtime's own grant —
  and remembered; a control client reads the same list and never has to predict it.
  [one-runtime-per-machine]
- **DECIDED** — Failure isolation: one engine for every stream on the machine. A native crash
  in a built-in — camera, codec, audio — ends every stream on the
  machine, and the runtime restarts and re-loads every kept stream neither stopped nor failed
  from its persisted graph, while an attached stream ends with its `run` (§Product); a hang is
  bounded per node by the existing abandon budget and ends only that stream; Python crashes and hangs
  stay in their own process. "Restart the runtime and the streams come back" means re-loading
  from persisted graphs, never re-attaching — processor interpreters die with their parent by
  construction and surfaces cannot outlive the device's owner — so the runtime keeps each kept
  stream's graph in its state directory (§Product), the runtime directory staying ephemeral.
  A kept stream implicated in the runtime's last two crashes in a row is recorded `failed`
  (§Product): a node's threads, those it spawns, its escalate worker and a stream callback on
  an OS-owned thread carry its stream, and alternate-stack signal handlers, a hook on an
  escaping panic and the watchdog's exit 124 write the crashing thread's stream through a
  file opened at start; a crash outside any stream's work, SIGKILL and an OOM kill implicate none, and a crash no stream owns
  restarts and is a bug to file. Owner, 2026-09-30: a driver-level fault taking every stream
  down for a few seconds is the equivalent of a fundamental Docker issue crashing everything;
  one engine is easier to run and to get bug reports for, and an engine per stream multiplies
  the ways things can fail. An engine per stream is not a fallback. [one-runtime-per-machine;
  runtime-hosting — SHIPPED #2704, #2713]

## Graphics (RHI / GPU) — SHIPPED

- **DECIDED** — All Vulkan lives in the RHI (`vulkan/rhi/` + `streamlib-consumer-rhi`); one
  kernel abstraction per pipeline kind; consumers go through `GpuContext` only. Vulkan is the
  one RHI on every supported platform: MoltenVK is the macOS driver, reached through the same
  `HostVulkanDevice` and, in a helper, the same `ConsumerVulkanDevice`. Metal appears only as
  handles exported from Vulkan objects (`vkExportMetalObjectsEXT`) — the `MTLBuffer` behind an
  imported IOSurface, the shared event behind a timeline. There is no second backend, no
  per-platform RHI, and no backend selector at build or run time.
  [macos-platform-floor — SHIPPED #2355, #2356; macos-capability-parity — SHIPPED #2402, #2404]
- **DECIDED** — Instance and device creation are one probed path on every platform. Host and
  helper instances request `VK_KHR_portability_enumeration` with `ENUMERATE_PORTABILITY_KHR`,
  and both devices request `VK_KHR_portability_subset`, wherever the driver advertises them.
  Both request one instance API version, `REQUESTED_VULKAN_INSTANCE_API_VERSION` (1.4), shared
  from `streamlib-consumer-rhi` so host and helper cannot resolve different entry points
  across the IPC seam. It is the floor that makes the promoted 1.3 entry points resolve and is
  never inferred from a device query — MoltenVK clamps a device's reported `apiVersion` to
  whatever the instance asked for — and a source gate bans branching on the reported version,
  tests included. [macos-platform-floor — SHIPPED #2356]
- **DECIDED** — A capability a driver does not implement is an absent tier, never a failure or
  an abort. Ray tracing on MoltenVK: `supports_ray_tracing_pipeline` and the capability
  snapshot answer on both floors, and every ray-tracing kernel and acceleration-structure
  constructor — in Rust, and from Python at `setup()` through the escalate pre-check — refuses
  with one typed message naming the tier and its extension. Vulkan Video on macOS is
  compile-time absent, the codec seam routing to VideoToolbox (§Media I/O); on a Linux device
  without it, session construction refuses with `Error::GpuError` naming the missing
  direction, classified for all seven codec operations. A kernel whose SPIR-V declares a
  subgroup operation the driver does not serve, or uses one in a stage it does not serve,
  refuses at `create_*_kernel` naming the driver, the operation and the stage (MoltenVK serves
  none in vertex, measured) — the one class of GLSL construct found that MoltenVK cannot serve.
  [macos-platform-floor — SHIPPED #2356, #2357, #2374; macos-capability-parity — SHIPPED #2403]
- **DECIDED** — The engine's kernel primitives are exposable to Python as configured
  blocks: shader/compute source and binding config passed from Python, compiled and
  executed by the engine on its device — no user-side Vulkan, ever.
  [importable-python-library — SHIPPED #1717; python-kernel-surface — SHIPPED #1773, #1775]
- **DECIDED** — Python reaches every kernel kind Rust authoring reaches, per floor: compute,
  graphics and ray-tracing kernels, acceleration structures, and CPU readback. Python names
  and drives; the engine allocates, compiles, binds, and dispatches. No kernel kind is
  Rust-only. On macOS, compute and graphics run — `GpuContext`'s kernel and buffer surface and
  every kernel escalate family build on both floors, and no op answers "only available on
  Linux" — while ray tracing and acceleration structures are the typed absent tier above. CPU
  readback there is the IOSurface's own rows, and the five export-staging ops and
  `acquire_image` refuse naming why. Pipeline state and buffer resources inside a kind are a
  narrower claim, and the three a Python processor cannot reach are named rather than left
  silent: vertex and index buffers with indexed draws — no escalate op mints either buffer,
  and no consumer in either language binds one; uniform-buffer bindings — Rust consumers in
  the engine tree hold them, and a dispatch refuses one by name as a kind it cannot bind by
  surface id; and a storage buffer bound to a ray-tracing kernel, which the trace refuses by
  name. All three are undesigned. Storage buffers bind by surface id for compute and
  graphics, as the tensor buffer below.
  [python-kernel-api; python-kernel-surface — SHIPPED #1773, #1774, #1777;
  kernel-kind-parity-bar; macos-capability-parity — SHIPPED #2403; engine-steps — SHIPPED
  #2430]
- **DECIDED** — A kernel's output is an engine-owned texture that Python names by
  surface id and passes downstream in a message, and that a third-party GPU library in its
  own Python package reaches through a scope. On Linux, entering blits the texture to a
  linear view (`kDLCUDA` over DMA-BUF / OPAQUE_FD) and leaving blits any write back and
  orders it on the surface's timeline ahead of the engine's next read. On macOS there is no
  blit: the view is a `kDLMetal` capsule over a no-copy `MTLBuffer` on the texture's own
  IOSurface, strided at the surface's row pitch, and leaving retires the write before the
  scope closes — torch's MPS queue is drained on exit, the exception path included, and an MLX
  write is `mx.eval`ed inside the scope — so it is complete before the id can be published.
  The engine owns that ordering — no fence or timeline vocabulary reaches Python. On Linux,
  leaving the scope by a propagating exception discards the write instead: a half-written
  view blitted back publishes a torn frame that surfaces as corrupt pixels somewhere
  downstream rather than at the `raise`, so the engine keeps the complete frame it already
  holds and lets the exception propagate — one rule for both device-write scopes, the CPU
  pixel-buffer scope included (the surface handle's scope and its pending *device* write —
  distinct from the cast object's `cpu()`, whose coherent-mapped stores publish per store;
  its staged arm over a texture backing follows this same discard rule — see the cast-object
  entry in §Packages), and discarding never suppresses the exception. On macOS the scope
  writes the surface in place, so a raise keeps the stores that landed — published per store,
  as the pixel-buffer door is everywhere — and still never suppresses the exception. A
  write-back is always an edit of a frame the processor read, never a fresh-frame write: the
  engine refuses a write-back into a staging that has not first read that same frame, because
  it cannot tell a consumer's write from uninitialised memory and one staging spans every
  frame its pool slot publishes. Cross-process texture import is part of the capability, and
  importability is an allocation flavour the engine derives per acquisition, never a Python
  dial. Linux: OPAQUE_FD is taken only where that image pool exists and the device reports
  the request's format and usage exportable as OPAQUE_FD. On a device that exports no
  DMA-BUF, every such single-plane request — render targets included, whatever the format —
  takes OPAQUE_FD, and one the device cannot export that way is refused at the acquisition,
  naming the device, the format and the usage. On a device that exports DMA-BUF,
  single-plane render-attachment usage takes explicit-modifier DMA-BUF where the
  render-target modifier probes available, and a CUDA-mappable format whose usage sits
  inside the OPAQUE_FD set takes OPAQUE_FD.
  macOS: any single-plane format on a device with `VK_EXT_metal_objects` takes an `OPTIMAL`
  image over a private IOSurface the engine creates — never `kIOSurfaceIsGlobal`, which MoltenVK's own export path sets —
  bound to a device-local memory type that is not host-visible, chosen by query; a
  host-visible type would eagerly allocate a private `MTLBuffer` per image. Everything else
  keeps a non-importable allocation. A planar format is refused an IOSurface-backed image by
  name: NV12 render targets are absent on macOS, since MoltenVK refuses a biplanar 4:2:0
  IOSurface as a multi-planar image and no driver patch is carried (owner, 2026-09-26). A
  flavour the device or format cannot take falls back at derivation rather than failing the
  acquire — except on a Linux device without DMA-BUF, above — and the later cross-process
  import refuses by naming the flavour.
  [python-kernel-api; python-kernel-surface — SHIPPED #1778, #1779; macos-capability-parity
  — SHIPPED #2402, #2404]
- **DECIDED** — CPU reach into a texture-backed surface goes through the same doors
  as every surface — the cast object's `cpu()`, the surface handle's CPU lock and
  `as_numpy()` — routed over the surface's host-visible export staging; no separate
  readback vocabulary, and no door names the backing. The helper child checks out
  and maps that staging itself; pixel bytes never cross the escalate socket.
  Entering the staged CPU door always reads the current frame in — a pure write
  included, which is what makes its write-back legal — and taking the host side is
  what enters it: the lock alone reads nothing in, so a device-tensor scope under
  the same lock costs no host copy. A writable staged array publishes at the block
  edge, ordered ahead of the engine's next read; leaving by a propagating exception
  discards the edit without suppressing the exception, and a second distinct
  staging source inside one lock scope is refused by name rather than replacing the
  first — neither staging holds both edits, so there is no publication order that
  does not overwrite one. The door's one contract across both backings: a raise
  leaves the frame the engine already held or a complete edit of fewer pixels,
  never a torn frame — which of the two is the backing's own, and code that must
  not publish on failure edits outside the scope. Every staging copy blocks, and no
  author sees a `contended` answer. On Linux the readback staging allocates host-cached from a third
  OPAQUE_FD pool (probed HOST_ACCESS_RANDOM), falling back to the sequential-write
  pool on a device with no cached exportable memory type — slower there, never
  refused. Every OPAQUE_FD checkout binds the exporter's stated memory type index:
  the staging registration puts it on the surface-share wire as texture
  registrations already do, and an importer that cannot bind the stated index is
  refused by name — a conforming OPAQUE_FD import has no fd-properties query to
  derive one from. Python's `acquire_texture` implies `copy_src` and `copy_dst`;
  Rust's descriptor stays explicit; a texture whose usage still cannot take the
  copy refuses the door by name.
  On macOS the door has a direct arm: the image stays declared `OPTIMAL`, but its storage is
  the IOSurface's own linear rows — MoltenVK treats the tiling as metadata — so `cpu()` and
  the surface handle's host side read and write the surface itself under `IOSurfaceLock`,
  taken by the first host-side accessor and never by the lock alone, with no export staging
  and no readback copy. What that narrows is stated (owner, 2026-09-22, over a Linux-identical
  staging): the texture door's edit publishes per store, as the pixel-buffer door's does
  everywhere, so a raise keeps the stores that landed; the engine never reads a torn frame,
  but a second concurrent holder can observe an edit mid-flight.
  [texture-backed-cpu-reach — SHIPPED #1940, #1941, #1942; macos-capability-parity — SHIPPED
  #2402, #2403]
- **DECIDED** — The RHI imports a caller's own host mapping as memory the GPU writes
  into: one primitive, `GpuContextFullAccess::import_host_mapping_for_gpu_writes` over
  `HostMappingWrittenByGpu`, with the tier inside the abstraction and never at the caller.
  Imported tier — `VK_EXT_external_memory_host`, enabled by the optional-extension pattern
  with `minImportedHostPointerAlignment` snapshotted at device create — binds the
  page-aligned range as a `STORAGE_BUFFER`, so a kernel's writes land in that memory and no
  CPU touches them; publishing is a buffer barrier to the `HOST` stage. Staged tier — the
  extension absent, or the driver declining that particular range — allocates host-cached
  staging of the same length and publishes with one `memcpy`; never the write-combined
  sequential-write allocation, whose cost the decode path already measured. The holder
  reads `tier()` and `fallback_reason()` and logs which branch it took, once. This is what
  lets a built-in hand the GPU a device mapping it does not own — the virtual camera's
  loopback buffers are the first — rather than reading pixels back to copy them in.
  [virtual-camera-sink — SHIPPED #2196]
- **DECIDED** — `RhiColorConverter` runs both directions over one shader pair: the
  existing YUYV-buffer-to-RGBA-image pass, and its inverse writing an RGBA or BGRA image
  into a YUYV buffer at a stated destination stride, with the encoding matrix the algebraic
  inverse of the decoding one and the range taken from the frame's own `ColorInfo`. And a
  converter is ownable, not only cached: `GpuContext::color_converter()` hands every holder
  the same kernel, and a kernel owns one descriptor set, so two holders dispatching it
  concurrently rewrite each other's bindings under a submitted command buffer — a live
  cross-camera corruption in `CameraSource` as much as in `VirtualCameraSink`. Any per-processor
  path that dispatches conversion on its own thread takes `create_color_converter` instead.
  [virtual-camera-sink — SHIPPED #2196]
- **DECIDED** — A consumer of a *published* frame barriers it out of whatever layout it
  was observed in into the layout its own descriptor declares, and republishes that layout
  on the registration; only `UNDEFINED` is refused, because only `UNDEFINED` is free to
  discard the picture. Hard-coding the arriving layout is wrong: a frame
  published from the app process arrives in `SHADER_READ_ONLY_OPTIMAL` and one published
  from a helper arrives in `GENERAL`, so a sink that admits only the first silently
  consumes nothing from any graph with a Python processor in it — and sampling in place
  instead is undefined, since the sampled descriptor states one layout whatever the image
  is in. [virtual-camera-sink — SHIPPED #2198]
- **OPEN** — Serialising a `TextureRegistration`'s layout cell. The engine's contract is
  read-then-barrier-then-update with no lock held across the submit, which the encoder, the
  offscreen compositor and the escalate path all follow; two app-process consumers fanned
  from one producer can therefore observe the same layout and both barrier out of it. It
  is confined to the same-process texture cache — a re-imported registration and a private
  host texture share no cell — and whether the fix is a lock, a per-consumer view, or a
  narrower contract is an engine-wide call, not one a built-in makes for itself. The surface
  copy follows the same contract, and each escalate op family records under its own recorder
  lock, so a copy and a dispatch from two helpers are two such consumers of one cell.
- **DECIDED** — Python spells a kernel as an object: constructed in `setup()` where the
  capability typestate is Full, dispatched per frame in `process()`. Construction is
  registration and dispatch is a method call; no kernel handle string reaches Python.
  Compute takes a general N-binding array like graphics and ray tracing — a Python
  compute kernel reads one surface and writes another, at parity with Rust. A binding
  mismatch raises before any GPU work is submitted, and the message names the shader's
  declared bindings: an undeclared name, an unsupplied one, a name supplied twice and a
  kind mismatch are refused at dispatch — the kernel holds no binding state, so there is
  no implicit default and no carried-over value — while a stage mismatch and
  name-stripped SPIR-V on the escape hatch are refused at construction. Every refusal is
  checked engine-side, so the wheel is never the only guard.
  [python-kernel-api; python-kernel-surface — SHIPPED #1773, #1777]
- **DECIDED** — Compute, graphics, ray tracing, and CPU readback are always-present
  capabilities of `GpuContext`, reached the same way by every caller. No kernel capability
  can be absent at runtime, and no application glue supplies one.
  [python-kernel-api; python-kernel-surface — SHIPPED #1773, #1774, #1777]
- **DECIDED** — GLSL is the shader source contract: Python passes GLSL text and the
  engine compiles it at kernel construction, and re-creating an identical kernel is free —
  compilation is cached under a key covering everything that changes the output (source,
  stage, entry point, target environment, compiler version), never source alone.
  Pre-compiled SPIR-V stays accepted as an escape hatch. Authoring a kernel requires no
  toolchain beyond the installed wheel, for every kernel kind. The wheel carries a C++
  GLSL compiler (shaderc / glslang). [python-kernel-api; python-kernel-surface —
  SHIPPED #1775]
- **DECIDED** — Dispatch is synchronous: it returns when the GPU work has retired and
  the writes are visible, and no fence or timeline vocabulary reaches Python. Several
  dispatches batch into one submission with barriers between them and a single fence at
  the end — the Python equivalent of the command-recorder flow. The batch accumulates its
  dispatches and sends them as one op on leaving the scope, never holding the privileged
  gate open across user Python; a raise inside the scope sends nothing. Two constraints
  ride it while bindings still stash on the kernel, both refused by name and both
  retiring with the Rust convergence below: one kernel may appear only once per batch,
  because a kernel owns a single descriptor set and a second bind would silently hand the
  earlier dispatch the later one's bindings; and one surface may not be bound at two
  kinds in a single dispatch, because no image layout satisfies both a sampled and a
  storage descriptor. [python-kernel-api; python-kernel-surface — SHIPPED #1773, #1776]
- **DECIDED** — One kernel spelling in both languages: bindings are passed at dispatch,
  by name, and never persist on the kernel object. Rust's stateful numeric-slot setters
  go; the command-recorder flow keeps its seam by carrying bindings to the recorder
  rather than stashing them on the kernel. The Rust convergence is its own change,
  sequenced after the Python surface. [python-kernel-api]
- **DECIDED** — Python copies one surface into another through the engine:
  `copy_surface_to_surface(source_surface_id, destination_surface)` on the Limited GPU
  capability, and so on Full, stated on both stub classes — the escalate op
  `copy_surface_to_surface`, on both floors. Any backing pair, same format and extent, no
  conversion: the engine resolves both surfaces through the any-backing resolver and picks
  buffer→buffer, buffer→image, image→buffer or image→image. The last rides the RHI's
  `record_copy_image_to_image`: whole-image, each image barriered from its known layout,
  refusing by name a missing image, one image on both sides, a format or extent mismatch, a
  source in `UNDEFINED`, and missing copy usage. Wherever a pixel buffer is in the pair,
  format compares on the one-buffer pixel shape, so a pool `rgba` frame and an `rgba8_unorm`
  texture are one format; two textures match exactly. The copy is ordered ahead of the
  destination's next read by recording, submitting and waiting on the host before the reply,
  as dispatch and every other escalate GPU op are, on both floors — one round trip per copy,
  so copy-then-dispatch pays two (owner, 2026-09-22, over a signalled timeline, which becomes
  its own change if the round trip shows in a measured budget). Both resolved backings are
  held across record and wait, which keeps the allocations alive but takes no lease: a caller
  copying a frame its producer may recycle holds `claim_surface_against_producer_reuse`. A
  destination texture's settled layout is updated and republished, as a dispatch does.
  Refused by name: a format or extent mismatch, a retired frame generation, a destination
  that cannot take a write-back — answered by the same `resolved_backing_takes_a_write_back`
  rule `writable()` and the stagings use, never by minting a staging — a source and
  destination that are one allocation, and a texture source nothing has written. A frame
  lands in a kernel's input texture this way, with no array library.
  [portable-gpu-interop — SHIPPED #2420]
- **DECIDED** — Python holds a tensor buffer: `acquire_storage_buffer(shape, dtype)` on the
  Limited GPU capability, and so on Full, mints an engine-owned storage buffer of a declared
  contiguous row-major shape and element type (`float32`, `float16`, `uint8`, `int32`), named
  by surface id like every surface. One method in both languages — Rust's takes a
  `TensorStorageBufferLayout`, the byte form spelled `TensorStorageBufferLayout::of_bytes` —
  and the engine derives the allocation flavour per acquisition: a caller-held Rust buffer
  stays HOST_VISIBLE; one that crosses to a helper takes DEVICE_LOCAL OPAQUE_FD on Linux and a
  private byte-shaped IOSurface on macOS (packed 16 KiB one-byte rows, imported as host memory
  spanning exactly the tensor). On the wire and in the surface store it is its own kind,
  `resource_type` `storage_buffer`, never a pixel buffer in disguise. Its registration carries
  `shape`, `dtype`, `exporting_device_uuid` and `vk_memory_type_index`, and is refused when it
  lacks its layout or its surface cannot hold the tensor; lookup and checkout echo the layout.
  The escalate op `acquire_storage_buffer {request_id, shape, dtype, processor_output_pool?}`
  answers `shape` and `dtype` on `EscalateResponseOk`; the handle registers as
  `RegisteredHandle::StorageBuffer` and releases through the shared release path. A
  parent-wide surface id → `StorageBuffer` map beside `texture_cache`, gated by the
  retired-generation check, lets any helper bind any helper's tensor. A compute kernel or a
  draw binds it at dispatch as `storage_buffer`, by id; the batch recorder barriers each bound
  buffer on every touch, and dispatch stays synchronous. It leaves the processor as a DLPack
  capsule of its declared shape over the engine's own memory — `kDLCUDA` on Linux, CUDA
  importing the tensor's exact byte size once; `kDLMetal` on macOS, over the `MTLBuffer`
  exported from the helper's import — with no staging and no copy, writable for the acquirer
  and read-only for a resolver. Closing the handle, and any dispatch, batch or draw that binds
  the handle, orders the acquirer's torch writes ahead of every other holder (CUDA's
  device-wide synchronize; a drained MPS queue); an MLX write is ordered by the `mx.eval` it
  owes inside the scope, and a tensor bound by bare id string is not ordered. Downstream,
  `resolve_surface` yields a handle stating `shape` and `dtype` and no pixel geometry —
  `width`, `height`, `format`, `lock`, `as_numpy`, `as_device_tensor` and `bytes_per_row`
  refuse by name — whose bare `__dlpack__` is the read path. It is held to the surface-id
  lifetime contract every surface id is (§Packages): a tensor published downstream comes from
  the processor output pool below, whose held slots are never rewritten. Uniform buffers
  trail it; push constants carry per-dispatch parameters meanwhile.
  [engine-steps-for-effects-and-model-input; engine-steps — SHIPPED #2429, #2430, #2431]
- **DECIDED** — Every processor output ring hands out slots from the engine's lease-aware
  pool; no wheel ring rotates on its own. One slot ring, `LeaseAwarePoolSlotRing`, serves the
  pixel-buffer pool and the processor output pools alike: it mints `<slot>#<generation>` per
  publication, skips a slot any consumer has checked out or this process holds, retires the
  previous id in-process and at the surface-share service, and fails closed on a poisoned
  lease table. A processor output pool is per helper, keyed by a pool key the helper mints,
  and holds textures or tensor storage buffers:
  `acquire_texture_from_processor_output_pool` and
  `acquire_storage_buffer_from_processor_output_pool` on both GPU capabilities, riding an
  optional `processor_output_pool {pool_key, rotation_depth}` on the two acquire ops. It
  rotates through `rotation_depth` slots, grows while consumers hold frames to a cap of 16,
  and at the cap refuses by name so the producer drops its own frame; this cap is the only
  count on a node's pooled frames, the shared pools below capping no shape. A descriptor
  change replaces the pool, bridge teardown releases every slot and frees each one no holder
  still has, and a retired id is refused as recycled at resolve. `ProcessorOutputTextureRing` keeps its spelling and asks this pool for
  every frame; `ModelInputTensorKernel` asks its tensor side directly. Handing out a reused
  slot is bookkeeping only — a lease scan and a generation mint, entering neither the
  escalate gate nor a device-idle wait; only growth allocates under the gate. Nothing
  therefore drains a slot's earlier readers before its next write, so every write into a slot
  orders itself: a draw's colour-target barrier sources `ALL_COMMANDS` / `MEMORY_WRITE`, as
  the compute path's entry scope does, so it waits on the one queue for any in-flight work on
  that slot. Owner, 2026-09-22, over a wheel ring asking a "still held?" op per
  rotation, which would be a second system deciding slot reuse.
  [engine-steps — SHIPPED #2427, #2429, #2503, #2546; shared-pools-bounded-by-memory-budget —
  SHIPPED #2741, #2743; amended by
  tatolab-names: on the Python surface the two acquire methods become
  `…_from_node_output_pool` and the ring `NodeOutputTextureRing` (§Packages, no public Python
  name says "processor")]
- **DECIDED** — Frame count is per node; memory is per runtime. A node's frames are limited by
  the node alone: its pooled frames by the cap of each output pool it keys, its one-off
  acquisitions — a texture, storage buffer, pixel buffer or image taken outside that pool — by
  a count of 16 held per shape, kept for the node and refused by name at that count, for a
  processor interpreter and a native node alike. A native built-in's per-frame acquisitions
  draw from an output pool of its own on the same terms. The runtime's shared texture and
  pixel-buffer pools reuse allocations by shape and cap no shape by count. What bounds them is
  device memory, read per heap from the allocator's budget with `VK_EXT_memory_budget` enabled
  wherever the device advertises it: every frame allocation — pooled, one-off, or unpooled (a
  kernel output, a render target) — is refused by name before it allocates when the heap's
  reported usage plus the request would pass its budget less a small headroom, or when the
  process nears the device's `maxMemoryAllocationCount`; the check and the allocation are one
  step, so concurrent allocations never pass on one reading. A refusal reads a fresh budget and
  names the heap, its budget, its usage and the bytes the pools hold; the node drops that
  frame. Without the extension the budget is the allocator's estimate of 80% of the heap, also
  capped by the system's available memory for a heap that is not `DEVICE_LOCAL`, and the
  runtime says once that its bound is an estimate. On macOS the bound is Metal's
  `recommendedMaxWorkingSetSize`, and usage is the Metal device's `currentAllocatedSize`,
  which counts the IOSurfaces the runtime mints. On Linux, memory a helper process allocates
  reaches the runtime only as a smaller budget; on macOS it does not reach the bound, since
  `currentAllocatedSize` is per process and the working-set size is fixed. A smaller budget
  never takes back a held frame. No acquisition waits on another holder. Memory is shared, so
  a stream that grows into the budget can cost another stream its frames; isolation is by
  count, per node. [shared-pools-bounded-by-memory-budget — SHIPPED #2741, #2748]
  Rejected: raising the shared pool's per-shape cap — any count shared across nodes lets one
  node's held frames drop another node's frames (owner, 2026-10-12).
  Rejected: bounding the shared pools only by the driver's allocation failure — NVIDIA reports
  a fake out-of-memory, and nothing would return an ended stream's memory (owner, 2026-10-12).
- **DECIDED** — The shared pools give memory back. A pooled entry is freed once it is unheld —
  no in-process handle, no checkout lease, on macOS no kernel in-use count, and every
  submission made before its release has completed — and either its stream has stopped, its
  node's output pool was torn down, or it has sat unused past an idle window of a few seconds.
  Freeing releases the entry's surface-share registration and the parent's caches and retires
  its id in the same step. Idle entries are found on acquire and on release, a bounded number
  per call, so freeing never stalls a producer. Freeing never takes an exportable memory
  flavour to zero live allocations. An allocation an exported raw handle still names stays
  counted against the budget until that handle's holder drops it.
  [shared-pools-bounded-by-memory-budget — SHIPPED #2743]
- **DECIDED** — A pixel effect is written as a shader body: `GlslPixelEffect`, pure wheel
  Python over `create_compute_kernel`, the processor output texture ring,
  `copy_surface_to_surface` and dispatch, with no engine, wire or stub change. The user writes
  one GLSL function, `vec4 effect(vec4 source, ivec2 at)`;
  `GlslPixelEffect.compile(gpu_full_access, effect_glsl=, dials=)` in `setup()` builds an
  ordinary compute kernel around it, and `apply_to_frame(gpu_limited_access, frame, dials=)`
  in `process()` lands the frame with the engine copy, dispatches, and returns the output message
  (`surface_id`, `width`, `height`, `timestamp_ns`, `color_info`). One single-plane RGBA
  source, output at its extent in `rgba8_unorm`. Dials are push constants typed `float`,
  `int`, `vec2` or `vec4`, read as `dials.<name>` and laid out std430 after the template's
  elapsed-seconds member — `vec3` refused by name, and a block past 128 bytes refused naming
  the dial that crosses it — declared at compile and supplied at every apply, checked before
  any GPU work. Pre-declared: `streamlib_extent`, `streamlib_elapsed_seconds` (monotonic, zero
  at the first apply), `streamlib_source_at` (clamped at the edge) and `streamlib_source_uv`
  (bilinear). `#line 1` above the body makes a compiler diagnostic name the user's own line.
  Refusals name the line the user can fix: a missing `effect` signature, a malformed or
  reserved dial name, a `vec3`, an oversize block, an undeclared, missing or mistyped dial,
  and a frame the copy refuses. No Rust peer until a Rust consumer names one.
  [engine-steps-for-effects-and-model-input; engine-steps — SHIPPED #2428]
- **DECIDED** — Model input is prepared on the GPU by `ModelInputTensorKernel`, pure wheel
  Python over a compute kernel, the tensor side of the processor output pool and binding by
  surface id — no engine or wire change. `ModelInputTensorKernel.compile(gpu_full_access,
  width=, height=, fit=, pad_to_multiple_of=, channel_order=, layout=, dtype=, scale=, mean=,
  std=)` in `setup()`; `apply_to_surface(gpu_limited_access, surface)` in `process()` runs one
  pass from an 8-bit RGBA surface (`rgba32` or `rgba8_unorm`) into a pooled tensor and returns
  a `ModelInputTensor`: `tensor_surface`, which `torch.from_dlpack` reads zero-copy, and
  `geometry`, whose `boxes_to_source` maps xyxy detections back to source coordinates. Fit
  `stretch` or `letterbox` produces the model's `width` × `height`, bilinear with no
  antialias, letterbox padding black; `pad_bottom_right` takes no size and never resizes —
  each tensor is the frame's own extent rounded up to `pad_to_multiple_of`, and the pool
  re-sizes when the extent changes (owner ruling on #2432, 2026-09-27). Channel order `rgb` or
  `bgr` with alpha dropped, layout `nchw` or `nhwc`, `float32` or `float16`,
  `(x * scale - mean) / std`. Refused by name: an unknown fit, layout, dtype or channel order;
  a size given with `pad_bottom_right`, or `pad_to_multiple_of` with any other fit; a tensor
  or non-RGBA source; a float16 tensor with an odd element count; an extent past one
  dispatch. No colour conversion: a YUV frame is converted by the engine before it is
  published. [engine-steps-for-effects-and-model-input; engine-steps — SHIPPED #2432]
- **OPEN** — Everything else, including the two graphics capabilities no language can
  render: depth attachments — Rust constructs a depth-testing pipeline that Python cannot
  name, and no pass in either language renders against one — and MSAA, refused for every
  caller in every language with the pipeline hardcoded to a single sample. Both are
  unbuilt engine capabilities rather than Python-reach gaps; equalising the construction
  surface with no pass to render against would buy nothing.
- **DECIDED** — Accelerators are optional: a stream that needs no GPU runs on a machine without
  one, and the engine never runs without a device. The Linux runtime unit carries the Vulkan
  loader and a software device — Mesa's lavapipe, built with LLVM linked in — in
  `tatolab/runtime/_vulkan_driver/` beside `tatolabd`, named to the loader additively as the
  macOS unit names MoltenVK, both opened by the engine and by every processor interpreter;
  macOS is unchanged, every Apple Silicon machine having a GPU behind MoltenVK. The runtime
  starts and serves with no device built; the first stream to start builds the one
  `GpuContext`, shared by every stream, on the first device the engine can build on in the
  loader's order — discrete, integrated, virtual, then CPU-type — so the software device is
  taken only when no hardware device builds. A device the engine cannot build on — a required
  feature absent, no graphics queue, a sentinel allocation failing in a flavour the device
  advertises — is passed over, and the runtime's log names the device it built on and why each
  device before it was passed over; the loader's own device-selection variables are the one
  override, the engine adding no dial. A runtime that can build no device at all — with the
  bundle, a broken install — keeps serving and refuses each stream's start by name, as today.
  The adapter crates stay under `adapters/`; nothing here moves one. Owner, 2026-10-13.
  [one-runtime-per-machine; optional-accelerators — SHIPPED #2761, #2763]
  Rejected: a device-less engine mode — CPU pixel buffers and a CPU path in every built-in — a
  second path beside every GPU one (owner, 2026-09-26, reaffirmed 2026-10-13).
  Rejected: SwiftShader as the software device — a 2 GiB heap, no memory-budget extension, no
  exportable timeline semaphore, no tagged release (owner, 2026-10-13).
- **DECIDED** — What a stream gets on the software device. Every stream starts and every node
  runs; a capability the software device lacks is an absent tier refused by name at the node
  that asks, as on any device, never at the stream's start: Vulkan Video, so the codec
  built-ins refuse at setup; the CUDA device tensor and the CUDA Array Interface, refused
  naming the device while `cpu()` reads; the GL adapter and every DMA-BUF-shaped handle. The
  DMA-BUF flavour is absent on the software device by rule, never by what `/dev/udmabuf`
  permits: its frames cross to processor interpreters as OPAQUE_FD, the pixel-buffer pool
  taking that flavour where DMA-BUF is absent, and the virtual camera's loopback door takes
  the host-pointer import tier. A device that exports no semaphore — the software device does
  not — crosses no timeline: a texture or staging registration carries none, a processor
  interpreter waits on none, and every copy an escalate op makes on that device is retired on
  the host before the reply, as on macOS. Camera capture takes its MMAP path; the bundled
  driver carries no window-system code; compute, graphics, ray tracing and pixel effects run
  at the speed the CPU gives; the runtime names the device once and warns no further. A
  processor
  interpreter imports on the runtime's device: the runtime hands each interpreter the device
  it built on and the loader environment reaching the bundled driver at spawn, and an
  interpreter that resolves another device refuses by name. The memory budget on the software
  device is the extension's reading of the system-memory heap, as the budget entry above
  states for any device. Owner, 2026-10-13. [optional-accelerators — SHIPPED #2766, #2770,
  #2772, #2776, #2789, #2794, #2796]
  Rejected: a software device built with window-system support — it links libxcb, libX11-xcb,
  libxshmfence and libwayland-client, so a minimal container or headless server, which lacks
  them, cannot open the driver at all (owner, 2026-10-13).
  Rejected: carrying both a headless and a window-system build, chosen at start — roughly
  doubles the driver bytes (owner, 2026-10-13).
- **DECIDED** — The Linux CI lane runs the engine on the bundled software device. A test is
  rig-only only when it needs what a GitHub runner cannot give — a capability the software
  device lacks (Vulkan Video, DMA-BUF modifiers, CUDA) or a real camera or audio
  device — and its marker's reason says which; every other engine, runtime and integration
  test that builds a device runs on the lane. Owner, 2026-10-13, over the 2026-08-03 rig-only
  ruling. [optional-accelerators — SHIPPED #2778, #2780]

## Media I/O — camera, audio, codecs — SHIPPED (unbuilt: `H264Encoder`'s CPU arm)

- **DECIDED** — First-party camera and audio are native built-in processors
  in the engine tree, statically linked into `tatolabd` — pre-built named blocks added and
  configured from Python (`stream_builder.add(CameraSource)`) through the classes generated
  into `tatolab.stream`, whose per-frame paths never enter an interpreter. Lag-by-design
  ends: built-ins ship inside the runtime, current by construction. This names the shipped
  set, not a rule: a further first-party capability is a built-in only under the criterion
  in §Packages & extension model, and is otherwise an extension wheel.
  [importable-python-library — SHIPPED #1709; extension-model; package-split-and-lend —
  SHIPPED #2684, #2693]
- **DECIDED** — A virtual camera is a fifth device class and a built-in under criterion
  (c): `VirtualCameraSink`, in the media built-ins crate, Linux only — with no Apple port,
  stated rather than deferred: a macOS virtual camera is a CoreMediaIO Camera Extension
  inside a bundled, entitled, notarised app, which the floor rules out under any
  justification, and the DAL plug-in stopped loading in macOS 14.1; on macOS the runtime
  refuses a graph naming it at load, naming the platform, and `tatolab enable-virtual-camera`
  refuses by name. As
  many instances as the graph adds. Each instance is one camera that exists only
  while its processor runs: created at `setup()`, removed at `teardown()`, a camera plugged
  in and pulled out from every other application's point of view, whose frames are
  whatever the graph writes. It takes video on an input `video` declared `newest`. Config:
  `name`, the camera's name in every picker, defaulting to
  `StreamLib Camera` followed by a four-character id derived from the stream's project
  directory, the stream's name and the node's name — distinct across nodes and streams, the
  same on every run of the same stream, so unnamed cameras never collide and a device left
  behind is still recognised; `door`, optional, `auto` by default. Two doors, one per instance,
  chosen at `setup()` and logged once. The **v4l2loopback** door creates its own device
  through the module's control node (`/dev/v4l2loopback`, `CTL_ADD` with the label set to
  `name`, capture-only capabilities announced, four buffers) and removes it with
  `CTL_REMOVE` at teardown; it is the door every application sees — `/dev/video*`
  readers directly and portal-based readers through the session manager's V4L2 mirror —
  and it is taken whenever the control node is writable by the process. The **PipeWire**
  door registers a `Video/Source` node with `media.role = Camera` and the configured name,
  destroyed at teardown; it needs no module and no root and is what a fresh install gets
  when the control node is absent or not writable. Never both for one instance, since the
  mirror would list the camera twice. The permission behind the loopback door is the
  standard udev grant a desktop hands its seat user for a device node — the module loaded
  with no devices, and a rule tagging its control node `uaccess` — installed once by the
  user through the CLI's `enable-virtual-camera` verb behind the desktop's own password
  prompt (polkit), never by the engine, which runs unprivileged always. Without it, `auto`
  takes the PipeWire door and says so; `door = "v4l2loopback"` refuses at `setup()` by
  name, saying the sink lacks permission to create a camera and naming the verb to run —
  the processor never reaches Running and the runtime keeps running. A device the
  sink cannot remove at teardown because a reader still holds it is left in place and
  reclaimed by label at the next `setup()`; a device left behind by a crash is reclaimed
  the same way. Loopback door: memory-mapped output streaming, YUYV, the device format set
  from the first frame's extent and re-negotiated when the extent changes, the frame's
  monotonic timestamp passed through on every queued buffer, and `S_FMT` carrying
  `V4L2_PIX_FMT_PRIV_MAGIC`, without which the V4L2 core zeroes the three extended
  colorimetry axes and every reader derives limited range for a full-range picture.
  PipeWire door: the engine's DMA-BUF textures offered with their modifier beside a
  shared-memory fallback, the consumer choosing, the frame's stamp on every buffer in
  `SPA_META_Header.pts` — the only place a consumer reads a stamp from, and the only one
  that survives this arm's PipeWire 0.3.50 floor, since `pw_buffer.time` is a trailing
  field of a struct the *host's* libpipewire allocates and arrived in 1.0.5. The loopback
  door's per-frame path is one GPU pass: the RGBA→YUYV conversion kernel writes straight
  into the mapped loopback buffer through the RHI's host-pointer import, so no CPU touches
  a pixel; where the driver refuses that import, the same kernel writes into cached host
  staging and one copy lands it. The platform floor settled on the import: NVIDIA 595.84
  takes the loopback's own character-device mapping, so `imported_host_pointer` is the tier
  this platform runs and the staged tier is a proven fallback rather than the norm. The
  loopback device is reached through V4L2 ioctls alone and PipeWire through the engine's
  existing `dlsym` shim — one process-wide loader and one entry-point list serving the
  audio and the video half both: no user-space library is linked and the wheel's
  `DT_NEEDED` set does not grow. An odd frame width is refused by name, since YUYV packs
  two pixels to a macropixel and the module recomputes `bytesperline` from the width
  whatever a caller states. Owner rulings, 2026-09-06. Proven on the rig for the
  loopback door — two named cameras from one graph, read back as YUYV with matching
  colorimetry, and both gone at a shutdown no reader was holding, which is the removal's
  good case and not a proof against the `EBUSY` path above. The PipeWire door is proven as
  far as registration and negotiation (WirePlumber lists the node beside its V4L2 cameras;
  the offer carries the modifier and its shared-memory sibling) and no further: no consumer
  reachable on the rig negotiates a PipeWire camera at all — `pipewiresrc` fails identically
  for WirePlumber's own V4L2 devices, and Chrome 152 ships the flag off — so which door a
  consumer takes and what stamp it observes is unproven, and closing it needs a machine with
  a working PipeWire camera consumer. [virtual-camera-sink — SHIPPED #2196, #2197, #2198;
  macos-capability-parity; runtime-hosting — SHIPPED #2704]
- **OPEN** — Two `VirtualCameraSink` behaviours the loopback door shipped with, each a
  stated placeholder the implementation left for a ruling rather than deciding inline.
  Re-negotiation keys on the extent alone, so a source that changes its `color_info` at the
  same extent keeps the first frame's description on the device — the pixels stay
  consistent with what the device was told, so the exposure is mis-signalled metadata and
  not wrong pixels, and the alternative costs every reader a re-plug mid-stream. And a
  device-configuration refusal — an `S_FMT` or `REQBUFS` that fails — latches for the
  processor's life, so one transient device failure ends that camera's run; whether a retry
  is owed, and whether it belongs to this built-in or to the runtime's restart policy, is
  undecided.
- **DECIDED** — Built-ins are written against the same handle-shaped hardware
  primitives third parties get — DMA-BUF / OPAQUE_FD / IOSurface import-export, audio clock, color resolution, and the audio device, video device and video codec
  backend seams — never against private engine guts;
  the layering wall survives the ABI's deletion as internal discipline.
  [importable-python-library — SHIPPED #1709, #1710]
- **DECIDED** — Capture is a video device backend seam with two arms — V4L2 on Linux,
  AVFoundation on Apple — built on the audio seam's own pieces rather than beside them:
  `VideoDeviceBackend` lists capture devices and opens a `VideoCaptureStream` whose hand-off
  delivers each frame already converted into a pooled `Rgba32` pixel buffer (its
  `PublishedPixelBufferFrameId`, extent, H.273 colour and capture instant) and which carries
  the same `DeviceStreamLivenessReport` an audio stream does. The arm is chosen by the one
  generic first-arm-that-opens walk audio uses, probed once per process and logged once, with
  no dial and no environment override; a platform with no arm lands on a refusing backend that
  lists nothing and refuses every open by name. `CameraSource` is platform-free above it and
  opens its stream at `setup()`, refusing there by name a named `device_id` that is not
  attached and listing the ones that are. The published contract: a
  `VideoFrame` message on port `video` whose `surface_id` names a pooled buffer, colour as the
  H.273 four-tuple resolved through the one table in `core::color`, an undescribed axis still
  an empty map. The V4L2 arm is `linux/v4l2_video_device_backend.rs`, with its EXPBUF/DMA-BUF
  probe, MMAP fallback and virtual-device skip list. The AVFoundation arm lists
  built-in cameras first, takes a camera's `uniqueID` as `device_id`, negotiates the most
  pixels within the `max_width`/`max_height` cap, then the highest frame rate, then
  `420v`/`420f`, reads colour from CoreVideo's own H.273 table, runs each device on its own
  serial control queue so `start()` never waits on camera power-up and a stop never waits on
  the session, and ends liveness on a runtime error or a disconnect. CoreVideo's pixel-format
  dictionary is initialised once per process ahead of both a camera's device input and any
  VideoToolbox session, because its first initialisation races `AVCaptureDeviceInput`'s.
  [media-io-layering; macos-platform-floor — SHIPPED #2358, #2359]
- **DECIDED** — A camera frame carries one capture instant on both of its stamps. The
  seam's hand-off carries `capture_timestamp_ns`, resolved under one rule every arm shares
  (`VideoCaptureInstantResolver`), and `CameraSource` assigns it to the frame's own
  `timestamp_ns` **and** passes the same value to `write_msgpack_with_timestamp` — never
  the implicit write, whose `MediaClock::now()` stamps publication, and never the call swap
  alone, which sets only the envelope. The encoder reads the payload's stamp; both name the
  instant of capture on both floors. A device stamp is trusted only
  when it is on the machine's monotonic clock and non-zero — V4L2's dequeued-buffer stamp with
  its timestamp flags, AVFoundation's sample presentation stamp converted from the session's
  synchronisation clock to host time; anything else falls back to the dequeue instant and is
  reported once per device, naming it. **A stamp ahead of the dequeue instant is clamped to it
  and counted**, the first reported: no real capture happens in the future, and trusting one
  silently ships a frame-period of audio-video skew. Owner, 2026-09-19, on evidence that
  `vivid` sets the monotonic flag honestly and still stamps roughly nine tenths of a frame
  period ahead; what a real UVC device reports is unmeasured, and the clamp is what makes that
  gap safe to carry. [macos-platform-floor — SHIPPED #2359]
- **DECIDED** — Camera and microphone permission on Apple is requested, never merely
  queried, and never awaited on a path that stalls the graph — one privacy gate serves both
  devices. The graph starts under a pending prompt and frames or blocks begin once the user
  allows it; a prompt still unanswered after ten seconds is warned about once. The engine is
  not the permission subject — the application that launched it is, found by walking up the
  process tree — so a refusal names that responsible application and the Camera or Microphone
  setting to change, and never says "Python". A microphone's AUHAL unit is bound only after
  access is granted, because binding an input-enabled unit blocks inside coreaudiod's privacy
  check until the user answers. The engine is never daemonised: detaching breaks the
  attribution chain and silently costs device access. [macos-platform-floor — SHIPPED #2359;
  macos-capability-parity — SHIPPED #2411; amended by one-runtime-per-machine: an installer-registered per-user service starts the runtime, which never detaches itself]
- **DECIDED** — The runtime opens no windows: no display built-in, no node-owned window, no
  event pump, no present target or swapchain, no `winit`, and no `DISPLAY`, `WAYLAND_DISPLAY`
  or `XAUTHORITY` handling in `tatolabd`. The virtual camera keeps its compositor,
  `VulkanOffscreenCompositor`, offscreen only. A person sees a stream in a browser through the
  preview page (§Networking); the rigs and the codec proof read a port through the local relay
  as a user does, as tools outside the runtime; a stream's one output off its graph is a track
  at a relay. Owner, 2026-10-16 and 2026-10-17. [moq-relays — SHIPPED #2845]
- **DECIDED** — `TestPatternSource` is the sample stream's source: a branded looping animation
  — colour bars, the potato bouncing corner to corner, "Tatolab stream setup successfully" and
  "sample stream" bottom-right — rendered by one GPU pass into the pooled `Rgba32` frame every
  source lands in, the text from a glyph atlas baked into the binary, so it needs no device, no
  permission, no font library and nothing added to `DT_NEEDED`, and runs on the software
  device. It keeps its name and its ports; the branding is content. Owner, 2026-10-16.
  [moq-relays — SHIPPED #2866]
- **DECIDED** — `H264Encoder` has a CPU arm (unbuilt): until it lands a
  machine without a GPU encoder shares no H.264, and the sample stream is proven on a device
  with one. Where the device's platform arm refuses H.264 encode — the software device among
  them — the block encodes with openh264, loading Cisco's own prebuilt library, which the
  runtime fetches once from Cisco and verifies by hash and never builds from source; a machine
  that cannot fetch it refuses the encoder by name saying so. Same block, same config keys,
  the same Annex-B shape; the arm is chosen when the encode session opens and logged once.
  Known (2026-10-17): the process-wide first-arm-that-opens walk cannot reach it, because the
  Vulkan arm opens and refuses only at the first frame; its profile is Constrained Baseline,
  so its codec string must come from the SPS; Cisco's licence requires that the user can
  enable, disable and re-enable the binary and that its attribution line is shown where they
  do; Cisco's patent cover applies to personal and non-remunerated use alone. Owner,
  2026-10-16 and 2026-10-17. [moq-relays]
  Rejected: building openh264 from source — the patent coverage Cisco carries applies to its
  binary alone (owner, 2026-10-16).
  Rejected: a royalty-free codec in its own block for the no-GPU case — the scaffold would pick
  a codec per device (owner, 2026-10-16).
  Rejected: x264 — GPL, which a BUSL binary cannot link (owner, 2026-10-17).
- **DECIDED** — Camera → GPU transport: zero-copy import of the device's own memory when
  the driver takes it, transparent CPU upload otherwise, selected automatically — no
  configuration dial. Both arms land frames through one stage: the device's NV12 or YUYV
  bytes, read as a storage buffer by the RHI colour converter into one local scratch
  texture, copied into a pooled `Rgba32` pixel buffer and waited on host-side before the
  hand-off, so colour is the engine's on both floors. On Linux the buffer is an imported V4L2
  DMA-BUF. On Apple the capture's IOSurface-backed `CVPixelBuffer` has its memory imported as
  a storage buffer through `VK_EXT_external_memory_host` — the buffer keeps the surface alive,
  and the luma plane's 512-byte offset rides the source layout — and is read by the same NV12
  kernel, **never as a multi-planar `VkImage`**: MoltenVK refuses every CoreVideo 4:2:0
  surface as one, its import check comparing the surface's one-byte top-level element with
  the format's six-byte 2×2 block, through v1.4.2 and on `main`. A driver refusing the buffer
  import (MoltenVK before 1.4.1) falls back to CPU upload. Owner, 2026-09-21, while shipping
  #2359; a driver patch to import the surface as an NV12 image was ruled out, 2026-09-26
  (#2488, #2489). [media-io-layering; macos-platform-floor — SHIPPED #2359]
- **DECIDED** — Python-authored media processors (vendor or user) run in their own
  helper process like every other Python processor and are supported where deadlines
  allow: camera-class sources and block-level audio fit within the helper hop's
  budget; device audio callbacks stay native, always —
  a deadline the cross-process hop cannot meet, not a GIL argument.
  [importable-python-library; helper-process-placement-only — SHIPPED #1714]
- **DECIDED** — One clock on the data plane: every timestamp a processor stamps, reads,
  or compares — frames, messages, audio ticks, `ctx.time` — is the machine's monotonic clock
  (`CLOCK_MONOTONIC` on Linux, `mach_absolute_time` on Apple), the same epoch the V4L2
  and ALSA driver stamps carry on Linux and CoreAudio's `mHostTime` and AVFoundation's
  host-time-converted presentation stamps carry on Apple, comparable across every node on a host — and on that host
  alone, since the epoch is that machine's own boot: two stamps from two machines are
  readings of two unrelated clocks, so a stamp is never compared against one taken on another
  machine's clock (§Networking). No
  process-relative epoch anywhere, and each language exports exactly one name for it. The
  wheel's name reads the engine's own `MediaClock`, so on macOS a helper's
  `monotonic_now_ns`, `ctx.time`, its default write stamp and its timer deadlines share
  `mach_absolute_time`'s domain with the engine rather than `CLOCK_MONOTONIC`, which keeps
  counting through sleep; on Linux both read `CLOCK_MONOTONIC`.
  Wall clock is permitted on exactly five surfaces and nowhere else: log record `host_ts`
  and `source_ts`, log file naming, the local relay certificate's validity — its UTC
  `notBefore` / `notAfter`, issued and aged by the one file that writes the certificate
  (owner, 2026-10-17) — and a relay token's expiry — the UTC `iat` / `exp` of the moq.pro
  JWTs the moq.pro adapter and the live proof's subscribe minter sign (owner, 2026-10-20) —
  their job is correlating with the outside world, which monotonic time cannot do. A wall-clock value never enters the data plane and is never compared
  against a media timestamp; another surface is a plan change, not a judgement call. The
  list is mechanically enforced, with no per-line pragma and no opt-out attribute: the
  permitted surfaces are a closed set in the gate, so a sixth is a source change that
  surfaces in review rather than a line quietly appended, and an
  entry whose file stops reading a wall clock is a licence the gate makes you hand back. The
  allowlist is per-file, which is why a data-plane file never joins it — a machine-global
  unique name comes from the engine's unique-name primitive, never from reading a clock.
  [one-monotonic-clock — SHIPPED #1725, #1726, #1727, #1728; macos-capability-parity —
  SHIPPED #2408; moq-relays — SHIPPED #2842, #2873]
- **DECIDED** — Audio backend: one chain per platform, probed once per process and logged
  once, no configuration dial and no environment override. On Linux, PipeWire-native,
  reached by runtime dlopen — the MIT-licensed PipeWire/SPA headers are vendored, the
  header-only SPA layer compiles into the wheel as a small shim, and every `pw_*` symbol
  binds at runtime — falling back to dlopen'd `libasound`, falling back to a null backend
  under which audio processors run, produce silence, and discard; no audio library ever
  appears in the wheel's `DT_NEEDED`. On Apple, CoreAudio through the AUHAL audio unit — the
  system's own frameworks — falling through to the same null backend only when CoreAudio
  offers no AUHAL unit or no default device in either direction. **An arm is chosen by opening, not
  by loading**: a library that resolves but yields no usable connection — `libpipewire`
  present with no daemon answering, the common container case — demotes to the next arm
  exactly as a missing library does, because probing on presence alone would strand
  precisely the machines the chain exists to serve. A caller-named `device_id` is the
  one case that does not demote: it raises at `setup()`, since a wrong device id is a
  wiring error and silently landing on a different device is worse than failing. No audio
  path links an audio library.
  [audio-subsystem; dlopen-audio-backend-and-audio-blocks — SHIPPED #1989, #1990, #1991;
  macos-capability-parity — SHIPPED #2411]
- **DECIDED** — The audio device seam is an engine primitive beside the audio clock,
  not built-in-private code: `AudioDeviceBackend` opening `AudioCaptureStream` and
  `AudioPlaybackStream`, living in `core/context/` with its Linux implementations under
  `linux/` and its CoreAudio one under `apple/`, exactly where the audio clock's two halves already sit. `MicrophoneSource`
  and `SpeakerSink` are written against it and reach no engine guts — the layering wall
  above, applied to a fourth device class. There is no second audio device path: the
  built-ins, the null backend and every test open streams through this one seam. A
  stream carries a liveness report its owner reads, so a publishing or draining thread
  whose device died comes back and says why rather than only telling the log.
  [dlopen-audio-backend-and-audio-blocks — SHIPPED #1989, #2012; macos-capability-parity —
  SHIPPED #2411]
- **DECIDED** — The CoreAudio arm opens one AUHAL unit per stream, I/O enabled in its
  direction only, with an interleaved `f32` client format at the stream's rate. Devices are
  named by CoreAudio UID; one not attached is refused naming it and the UIDs that are. A
  stream with no `device_id` follows the system default device — headphones or AirPods
  arriving move it — while a named stream stays pinned and ends if its device goes away
  (owner, 2026-09-25, #2411). A stream keeps the format it opened with for life: on output
  AUHAL converts to the device's format; on input an `AudioConverter` bridges a device of
  another rate or channel count, and stamps stay continuous on the host clock through the
  converter and through a rebind. Property listeners are CoreAudio blocks on the stream's own
  serial queue. Failures land on the stream's liveness report naming the device and the
  `OSStatus` — failed renders reaching the bound, a vanished device, a refused rebind, an
  oversized cycle. A playback stream reports its device period (`BufferFrameSize`) and
  `SpeakerSink` sizes its `match_device` window and hop, ring and underrun cadence from it;
  the PipeWire, ALSA and null arms report none, and `SpeakerSink` falls back to 10 ms there.
  Audible tests are attended only (`audible-hardware-tests`); the standing `hardware-tests`
  sweep on the Mac stays silent. The deviceless audio clock on Apple is a GCD timer, the
  peer of Linux's timerfd. [macos-capability-parity — SHIPPED #2411]
- **DECIDED** — Every audio symbol binds at runtime and the wheel's `DT_NEEDED` set does
  not grow: `libpipewire-0.3.so.0` and `libasound.so.2` resolve through `libloading`,
  the pattern the DRM modifier probe already uses for `libEGL.so.1` — a library held
  beside typed function pointers, a missing library demoting to the next arm and a
  missing symbol named rather than crashing. The versioned soname is the dlopen target,
  not a stylistic echo: a machine ships `libpipewire-0.3.so.0` with no dev symlink.
  Nothing links `cpal`, `pipewire-rs`, or any pkg-config audio crate — each puts an
  audio library straight into `DT_NEEDED` and fails the portability gate by
  construction. [dlopen-audio-backend-and-audio-blocks — SHIPPED #1990, #1991]
- **DECIDED** — SPA's header-only layer compiles into the wheel as a shim that calls
  nothing. PipeWire's pod builders and parsers are inline C with no shared object, so a
  small `cc`-compiled shim owns them and every `pw_*` entry point it needs arrives as a
  function pointer Rust filled by `dlsym` — the shim itself references no external
  symbol. This is the vendored VMA build verbatim in shape: compiled with its static and
  dynamic Vulkan function lookups both off so it calls only pointers Rust hands it, and
  adding no `DT_NEEDED` entry beyond the C++ runtime.
  [dlopen-audio-backend-and-audio-blocks — SHIPPED #1990]
- **DECIDED** — The headers are vendored, not taken from the build machine.
  `manylinux_2_28` carries no PipeWire development package, so a system-header build is
  not reproducible where the wheel is actually built — the same reasoning that pins the
  GLSL compiler to build-from-source rather than linking whatever sits on the builder.
  MIT-licensed PipeWire and SPA headers land under `vendor/`, untouched and
  unreformatted, and the licence obligations are met by the machinery that already
  reproduces every vendored C/C++ project's own licence text out of the tree. `LICENSE`,
  `LICENSES/` and `docs/license/` are not edited; the shim is our code and carries the
  BUSL header. [dlopen-audio-backend-and-audio-blocks — SHIPPED #1990]
- **DECIDED** — The portability gate is the design's pass/fail: the shipped
  `_engine.abi3.so` names five host libraries, every one on the permitted-host-library list,
  and audio adds none. No name is added to the permitted-host-library list — an audio
  library appearing there is the failure this design exists to prevent, not a fix for
  it. [dlopen-audio-backend-and-audio-blocks — SHIPPED #1990, #1991]
- **DECIDED** — A backend device paces the audio path: capture and playback callbacks
  are the cadence source, and a block's timestamp derives from the backend's own
  timing (status minus reported delay) in the machine monotonic epoch — never from a
  free-running timer. The timerfd `AudioClock` remains the SDK clock primitive and
  paces deviceless graphs only (null backend, tests), so it starts when something needs
  it and a device-paced graph never starts it at all — which is what makes "exactly one
  cadence source" true in the tree rather than merely stated: device ticks and timer
  ticks cannot interleave if the timer is not running.
  [audio-subsystem; dlopen-audio-backend-and-audio-blocks — SHIPPED #1989, #1990, #1991]
- **DECIDED** — A/V sync is block-level join-by-timestamp on the one monotonic clock:
  an `AudioBlock` carries its first sample's timestamp, rate, and sample count, so any
  sample's instant is derivable and audio joins camera frames by timestamp alone. No
  sample-accurate cross-modal machinery exists.
  [audio-subsystem; dlopen-audio-backend-and-audio-blocks — SHIPPED #1988, #1990, #1991]
- **DECIDED** — The device stamps the block and the engine never re-stamps it. A
  capture block's timestamp is the backend's own timing for its first sample —
  `pw_time`-derived status minus reported delay on the PipeWire arm,
  `snd_pcm_status_get_htstamp` on the ALSA arm with the monotonic timestamp type set
  explicitly so the stamp cannot arrive on `CLOCK_REALTIME`, and the input cycle's
  `mHostTime` (the `mach_absolute_time` domain) minus the device and stream latency on the
  CoreAudio arm — and it is published
  through the timestamped write, never the implicit one, whose `MediaClock::now()`
  would stamp the moment of publication rather than the instant of capture. Both the
  message field and the frame header therefore carry the same device-derived value, in the
  same epoch a video frame's timestamp carries, which is the whole of block-level A/V
  sync: joining audio to camera frames is subtracting two integers.
  [dlopen-audio-backend-and-audio-blocks — SHIPPED #1990, #1991; macos-capability-parity —
  SHIPPED #2411]
- **DECIDED** — A device callback never blocks, and the loss is counted at the edge.
  Audio's input ports declare `ordered` — order matters for samples, and nothing on the
  link may make the device wait. So a
  bounded ring sits between the callback and the publish: the callback only ever hands
  off, a source-owned thread drains the ring into the timestamped write, and when a
  stalled consumer fills the ring the source drops the oldest block at the device edge
  and increments its own counter. The loss is explicit in both directions — the counter
  is logged the way `CameraSource` logs its own, and the gap is derivable from the
  timestamps and sample counts of the blocks either side of it. Nothing is silently
  interpolated and no sample is invented.
  [dlopen-audio-backend-and-audio-blocks — SHIPPED #1989, #1992]
- **DECIDED** — The null backend runs the graph and produces silence: under it
  `MicrophoneSource` publishes silent blocks and `SpeakerSink` discards what it
  receives, both paced by the timerfd clock — so a pipeline authored on a workstation
  runs unchanged in a headless container and a test needs no audio hardware. A device
  that was *named* and cannot be opened is the opposite case and raises at `setup()`,
  the way `CameraSource` raises on a missing `/dev/video*`: a machine with no audio is a
  supported environment, a wrong device id is a wiring error.
  [dlopen-audio-backend-and-audio-blocks — SHIPPED #1989]
- **DECIDED** — The audio data model is the `AudioBlock` message: samples ride the link
  inline as msgpack bin, CPU-resident, interleaved, with sample rate, channel count,
  dtype, and first-sample timestamp beside them. It is the wire contract and the field
  names are the contract — the same shape `VideoFrame` states for video: an optional
  cast over a self-describing msgpack named map, declared on no port, registered
  nowhere, ignoring keys it does not read. The keys are `samples`, `sample_rate`,
  `channels`, `sample_count`, `dtype`, `first_sample_timestamp_ns`. `dtype` is metadata
  with `f32` the default and `i16` legal, and `samples` is little-endian — a wire
  statement rather than an assumption, since it is the property a message decoded by a relay
  reader, a CLI, or another language depends on. The sample count counts per-channel samples —
  an interleaved block of `channels` channels carries `sample_count × channels`
  scalars — so duration and the next block's expected timestamp derive from count and
  rate alone. Audio touches no surface machinery — no surface ids, no claims, no
  lifetime contract.
  [audio-subsystem; dlopen-audio-backend-and-audio-blocks — SHIPPED #1988]
- **DECIDED** — `samples` is msgpack `bin`, so the field is a byte buffer and `dtype`
  says how to read it — never a typed vector. A `Vec<f32>` field would serialize as a
  msgpack **array** — five bytes per sample, and a shape Python's own `bytes` → `bin`
  path does not agree with. So the field carries interleaved little-endian scalars as
  bytes, and one field spelling serves `f32` and `i16` alike. The wire-key test asserts
  the binary type for both, which is the one test that can catch an array-for-`bin`
  mistake. [dlopen-audio-backend-and-audio-blocks — SHIPPED #1988]
- **DECIDED** — The Python cast is pure Python and composes nothing surface-shaped. It
  lives beside `video_frame.py`, is read with `ctx.inputs.read("audio", into=AudioBlock)`,
  and owes no `.pyi` entry — pyright checks it from source, as it does `VideoFrame`. It
  must not compose the claimed-surface access class: that class demands a surface-id
  field and takes claims in its constructor, and audio has no surface, no claim and no
  lifetime contract. Its `samples` property maps the declared `dtype` to an explicit
  little-endian numpy type, never the platform-native spelling — the wire is
  little-endian by contract, not by luck — and returns a `frombuffer` view reshaped to
  `(sample_count, channels)`, with numpy imported lazily so the wheel still declares no
  numpy dependency. A payload whose length is not `sample_count × channels × itemsize`
  is refused by name at the cast rather than reshaped into a plausible-looking wrong
  answer. [dlopen-audio-backend-and-audio-blocks — SHIPPED #1988]
- **DECIDED** — "Zero-copy" is a claim about the cast, and is stated as exactly that.
  Between shared memory and `process()` the payload is copied four times — out of the
  iceoryx2 sample, a header-strip memmove, the msgpack decode into an owned value, and
  into a Python `bytes` — and audio removes none of them; they are the helper hop every
  message pays. What the cast guarantees is that it adds no fifth: the numpy array is a view
  over that `bytes`, and `torch.from_numpy` over it is a view again. At audio's sizes
  this is the right trade and the reason audio touches no surface machinery at all — a
  512-sample stereo `f32` block is 4 096 bytes against a 16 MiB per-link ceiling for a
  helper-placed processor. No doc, test name, or log line may describe the path as
  zero-copy from the device.
  [dlopen-audio-backend-and-audio-blocks — SHIPPED #1988]
- **DECIDED** — An audio input port may declare a window contract — sample rate,
  channels, dtype, window size, hop — beside its delivery profile: the engine resamples,
  converts channels, and frames natively so `process()` receives exact-size timestamped
  blocks matching the declaration. Resampling is an always-on engine stage, never a user
  processor. Feature extraction (mel, MFCC) is not engine surface: the contract ends at
  windowed raw samples. The contract rides the three carriers `delivery_profile` already
  rides — `ProcessorPortSchema`, `PortDescriptor`, `PortInfo` — as one optional struct
  rather than five loose fields, spelled the same in both languages
  (`AudioWindowContract(...)` in Python, `audio_window(...)` in the Rust grammar).
  `sample_rate`, `channels` and `dtype` reuse the device vocabulary `AudioStreamFormat`
  and `AudioSampleFormat` already state, never a parallel spelling; `dtype` takes the two
  `AudioBlock` legalises, `f32` and `i16`. `window_size` counts per-channel samples — the
  unit `AudioBlock.sample_count` already uses — so an emitted window carries
  `window_size × channels` scalars, and `hop` defaults to `window_size`: contiguous,
  non-overlapping windows, with a hop below it a legal rolling window. A port with no
  contract is unchanged in every respect; this is opt-in, and an output port declares no
  contract at all — only a consumer states what it needs.
  Four of the five values are required; `channels` is the one optional, and absent means
  *the source's own count, whatever it is*. The stage then resamples, frames and converts
  dtype exactly as declared, skips channel conversion alone, and every emitted window
  carries the count its block arrived with — so a consumer reads `channels` off the block
  rather than assuming it. A consumer that genuinely needs a fixed count — a model trained
  on mono — declares one and is converted by the fixed rule below. The default is not a
  knob because the graph is dynamic: a microphone added later must not require touching
  every consumer downstream of it, and a fixed count belongs only where a model asserts on
  it. On the carriers it is `Option<u32>`, `AudioWindowContract(channels=None)` in Python,
  `channels =` omitted in the Rust grammar; an absent count renders as `channels: source`
  rather than `null`, so a reader learns the absence was meant. `match_device` is
  untouched — a device stream resolves a count.
  [audio-subsystem; audio-port-window-contract — SHIPPED #2032; opus-mp4-recording-rung —
  SHIPPED #2123]
- **DECIDED** — The contract is all-or-nothing and every way of getting it wrong is
  refused by name, at the earliest seam that can see it. There is no partial form — a
  half-declared contract leaves the stage guessing at exactly the values a model asserts
  on — `channels` excepted, whose absence is itself a stated value. Refused at declaration
  in both languages: any other missing field, an unknown field, an
  unknown `dtype`, a hop above `window_size` (which would silently discard samples between
  windows), any numeric field at zero or below — a *declared* `channels` included — a
  second contract on one port, and a contract on an output. A window contract requires `delivery_profile = "ordered"` and is
  refused beside a skipping profile naming both knobs — `newest` passes over messages by
  design, so an accumulator needing contiguous samples would flush on nearly every read
  and, for a window wider than one device quantum, might never emit at all. Refused at
  wire time: a second inbound link into a windowed port, naming the port and both links —
  fan-in legally interleaves N producers' blocks in one mailbox, and two sample streams
  interleaved into one accumulator is plausible-looking wrong audio. Refused at the stage:
  an N→M channel pair with neither side 1, naming both counts, because the source count
  arrives with the messages and declaration cannot see it — a refusal that applies only to a
  *declared* count, there being nothing to convert to without one. Channel conversion runs
  both directions by fixed rule — N→1 averages, 1→N duplicates — since the rung's flagship
  case is an up-conversion.
  [audio-port-window-contract — SHIPPED #2032, #2033; opus-mp4-recording-rung — SHIPPED
  #2123]
- **DECIDED** — One stage, at the one read seam every reader already shares. It sits in
  `next_bag_for_the_reader`, which an app-process Rust processor reaches through the parent's
  mailboxes and a helper-placed Python processor through its own — one implementation
  serving both, with no new IPC hop and no parent↔child contract to design, which matters
  because every Python processor is helper-placed and a Python consumer is who this
  contract exists for. The contract reaches a helper child over the same parent→child
  wiring envelope that already carries `read_mode`. The order of operations is fixed:
  decode to f32 → channel-convert → resample → frame → encode to the declared dtype, with
  internal arithmetic in f32 always and an `i16` contract encoded back saturating rather
  than wrapping. The stage owns its own decode of the six `AudioBlock` wire keys and
  re-encodes each emitted window as an ordinary `AudioBlock` message, so `read(into=AudioBlock)`
  and Rust's `read::<AudioBlock>` work unchanged. A message the stage cannot read is refused by
  name at the read — an unknown `dtype`, a payload whose length is not
  `sample_count × channels × itemsize`, a message with no `AudioBlock` keys at all — never
  reshaped into a plausible wrong answer, and the refusal names the port.
  [audio-port-window-contract — SHIPPED #2033]
- **DECIDED** — Readiness on a windowed port means a full window, not an arrived message.
  Windowing is N-in → M-out — one 1024-sample quantum satisfies two 512-sample windows, a
  one-second rolling window needs about forty-seven of them — so the stage owns a per-port
  accumulator between the mailbox and the reader, and a windowed port reports data only
  when a full window can be emitted. A reactive `process()` is never dispatched with
  nothing to read, in the helper loop and the app-process runner alike; the drain loop
  dispatches once per ready window, so one 1024-sample quantum against a 512/512 contract
  dispatches twice and a ready window never sits latent waiting for the next message. A stream
  that simply stops leaves under one window of samples parked in the accumulator, delivered
  to nothing — designed, not a defect: an exact-size contract has no partial form to hand
  over. [audio-port-window-contract — SHIPPED #2033]
- **DECIDED** — The stage derives a stamp; it never reads a clock. One device stamp anchors
  each contiguous run — taken from the first block after start or after a flush — and every
  window's `first_sample_timestamp_ns` is that anchor plus the emitted-sample offset in
  integer rational arithmetic (`anchor + emitted × 1_000_000_000 / out_rate`, widened),
  minus the resampler's reported group delay. Never an accumulated per-sample delta, which
  drifts at 44.1 kHz-family rates; never re-anchored per block, whose status-derived stamps
  jitter below sample exactness. The device stamps the block and the engine never re-stamps
  it survives intact: deriving offsets from a device stamp is not re-stamping, and
  block-level A/V sync stays subtraction.
  [audio-port-window-contract — SHIPPED #2033]
- **DECIDED** — No sample is invented to bridge a gap; the stage flushes rather than
  interpolates. A discontinuity — a block's stamp missing its expected position by more
  than half a source quantum, a tolerance because status-derived device stamps jitter
  below sample exactness — flushes the accumulator **and the resampler's own filter
  state**, then re-anchors on the next block's stamp. The filter reset is load-bearing,
  not hygiene: a polyphase resampler holds a filter's length of pre-gap samples, and
  emitting through it after the gap blends audio across the loss — exactly the
  interpolation the drop-at-the-edge clause bans. The same doctrine settles priming at
  stream start and after every flush: filter output produced before the filter has filled
  is zero-padding, not audio, so it is discarded — an emitted sample always derives from
  real input — and the group-delay subtraction aligns the first emitted stamp with the
  real input sample it derives from. The gap stays derivable from the stamps either side.
  [audio-port-window-contract — SHIPPED #2033]
- **DECIDED** — Counting is unchanged and the accumulator is not a second drop site. Bags
  stay in the counted mailbox until `read` consumes them into the stage; the accumulator
  holds only the already-consumed resampled remainder, under one window's worth, and never
  evicts. Readiness is computed jointly — queued messages' sample counts plus the remainder —
  never by draining the mailbox at `has_data`, because an eager drain would starve the
  per-link counters exactly where loss happens and grow the accumulator unboundedly under
  a stalled consumer. That forces the depth question open: the profile's depth is a floor
  no contract undercuts, and the engine sizes a windowed port's mailbox up from its
  contract (`ceil(window / quantum) + margin`) — still engine-chosen, still not authorable;
  the contract is a declaration, not a depth dial. Overflow past that depth is a counted
  mailbox eviction, same counter, same `graph` surface. A discontinuity flush discards the
  remainder — under one window of samples — and that discard is counted: the samples it
  threw away are reported on the port's link beside its dropped messages and logged with the
  port and the sample count, so a message evicted at a windowed port costs its own samples plus
  the counted flush of the remainder behind it, and no part of the loss is silent. The
  iceoryx2 ring in front of a windowed port is engine-sized to a fixed cap well above a
  profile's depth, never to the contract's own depth, and a windowed port connected live onto
  a channel created smaller than that cap is refused by name.
  Three readings the build settled. **The cap is 64 messages**, returned by the one
  creation-depth function whenever any destination of the channel being created is windowed
  and 16 otherwise; the windowed port's subscriber ring is that 64, its mailbox depth stays
  sized from its contract, and neither is fed to the other. **The live refusal reads the
  held factory** — a windowed destination wired onto a channel whose creation depth is below
  64 is refused at wire time naming the port, the link, both depths and the fix of
  connecting the windowed consumer before the channel's other links, and it reads a channel
  only the exposed-port publisher or a lagging helper still holds as readily as a busy one. **A flush counts the
  samples no reader had received**, in per-channel samples at the port's declared rate: the
  remainder past the last emitted window's already-delivered overlap, plus the staged source
  frames scaled by the rate ratio and rounded down. Subtracting the overlap is what makes
  the count mean what the entry says — under a rolling hop the front of the remainder is
  samples the consumer already holds, and counting them would report loss for audio that was
  delivered. `graph` renders `discarded_samples_by_link` beside `dropped_bags_by_link`, only
  for links into windowed ports, and a link into an unwindowed port carries no sample count
  rather than a zero, the way a port that declared nothing renders no `audio_window` key;
  `frames_dropped` stays a message total, and both flush callers — the format change and the
  gap — count.
  Three discards stay uncounted, each for a reason already in this section: resampler priming
  output is filter delay rather than input; the remainder parked when a port's last link
  disconnects is the designed stop above; and a message `accept` refuses already fails the read
  by name.
  [audio-port-window-contract — SHIPPED #2033; loss-visibility — SHIPPED #2269]
- **OPEN** — Whether a windowed port connected live onto a channel created smaller than the
  windowed cap should instead wire at that channel's depth, with its ring overwrites counted.
  Overwrite counting is shipped (§Processor model); until this is decided the connect is
  refused as the entry above states. [loss-visibility]
- **DECIDED** — The resampler is `rubato` — pure Rust, MIT, adding no `DT_NEEDED` entry —
  and the portability gate is the pass/fail: the resampler adds no host library to the five
  the shipped `_engine.abi3.so` names. Its three adapter
  obligations are the stage's to meet: fixed input-chunk sizes, planar rather than
  interleaved buffers (de-interleave after the channel convert), and the group-delay and
  reset seams the stamp and flush clauses bind. Hand-rolling a polyphase resampler was
  rejected: a maintenance burden and no capability.
  [audio-port-window-contract — SHIPPED #2033]
- **DECIDED** — A port whose target format is not knowable at declaration declares the
  sentinel `audio_window = match_device`, and the contract resolves at `setup()` — where
  the typestate is Full — from the
  format the device stream just opened. Only a processor that opens a device stream can
  satisfy the sentinel: the `setup()` setter is the engine-internal mechanism, never public
  surface, and it is deliberately not exported to Python — the parity disposition, named:
  a Python processor's window is its model's compile-time knowledge, and it holds no
  machine-varying device format to resolve, so a `match_device` port on a helper-placed
  destination is refused at wire time. An unsettled sentinel reaching the stage is refused
  naming the resolution mechanism, and a device format the stage could not honour is
  refused too. A bare public setter was rejected: it would put a dynamic-contract API on
  the declaration surface where any processor could reach it, and leave the declaration
  site silent about a resolution the reader needs to know happens.
  [audio-port-window-contract — SHIPPED #2034]
- **DECIDED** — `MicrophoneSource` and `SpeakerSink` are the audio built-ins, beside
  the camera: native built-ins in the engine tree, registered with the other
  media built-ins and reaching Python as classes generated into
  `tatolab/stream/_built_in_nodes.py` from their descriptors, beside `CameraSource`,
  configured the one way a built-in is configured
  (`stream_builder.add(MicrophoneSource, config={"device_id": "..."})`). Both are `execution =
  manual`, the mode `CameraSource` uses for a device that paces itself, with
  `scheduling = realtime` — an audio device callback is the deadline that priority
  exists for. The declaration names that deadline; it does not apply a priority here,
  because the engine skips thread-priority application for every `manual` processor by
  design — real work runs on OS-managed callback threads, which is exactly right for
  audio, where the deadline belongs to the backend's own callback thread and not to the
  source's publishing thread. Conditioning — AEC, noise suppression, AGC via the
  statically linked WebRTC Audio Processing Module — is configuration on the built-ins,
  an engine-internal chain between device and published block, bypassable for
  microphones whose hardware DSP already conditions. `SpeakerSink` playback cancels
  immediately and reports played-up-to timestamps — the barge-in door and the AEC
  reference are one mechanism. A device callback never blocks on a slow consumer:
  at capacity `MicrophoneSource` drops at the device edge and the loss is
  explicit — the timestamp gap is derivable from the blocks around it and the
  source counts what it dropped — never silent. `SpeakerSink` matches its device rather
  than refusing what it cannot play: its input declares `audio_window = match_device`
  with window = hop = one device period — it wants format conversion, not framing, and
  under all-or-nothing that is how a converter is spelled — so the stage converts and the
  sink plays. It has no refusal of a block whose rate, channels or dtype the device cannot
  take, because the mic-to-speaker mismatch the two built-ins have by construction
  (capture prefers mono, playback prefers stereo) is the plainest case the window contract
  exists to fix. Conditioning and immediate cancel are a later rung.
  [audio-subsystem; dlopen-audio-backend-and-audio-blocks — SHIPPED #1989, #1992;
  audio-port-window-contract — SHIPPED #2034; package-split-and-lend — SHIPPED #2684]
- **DECIDED** — Codec blocks are native built-ins beside the camera and the audio
  pair: `H264Encoder`, `H264Decoder`, `H265Encoder`, `H265Decoder`,
  `OpusEncoder`, `OpusDecoder` — instantiated and configured the one way a
  built-in is configured (`stream_builder.add(H264Encoder)`), per-frame paths never entering an
  interpreter, serving Python and Rust apps alike. Video blocks are built on the video
  codec backend seam — Vulkan Video on Linux, VideoToolbox on Apple. The runtime decodes no
  JPEG anywhere. AV1 and VP9 remain ported but unexposed until a consumer demands them.
  The six name the shipped set, not a rule: the next codec follows the built-in criterion in
  §Packages & extension model.
  Encoder sessions mint lazily from the first frame's dimensions; decoder sessions
  auto-size the DPB from the stream's parameter sets. Config shape, rate-control and
  GOP knobs are ticket-level, like every other built-in's config. The four video blocks
  are one encode body and one decode body specialised by a codec identity, not four
  processors — the pair differs in an enumerant, its catalog entry's codec string and a name —
  and each built-in is its own port surface, registration and identity. The layering
  wall holds at this fourth device class too: colour conversion into the codec's NV12
  input rides the engine's existing RHI colour converter — the `rgb_to_nv12` stage on
  Linux, an RGBA-image → NV12-buffer pass beside the RGBA → YUYV one on Apple — and no new
  RHI primitive was built for codecs. [codec-blocks — SHIPPED #2083, #2084, #2086;
  python-codec-block-api — SHIPPED #2105; opus-mp4-recording-rung — SHIPPED #2125, #2126,
  #2127, #2128; macos-capability-parity — SHIPPED #2412, #2413; extension-model]
  Rejected: a `JpegDecoder` built-in, or a vendor JPEG backend such as nvJPEG behind one —
  its one consumer was a drone video path, and robotics is out of scope (owner, 2026-10-04).
  Rejected: a muxer built-in writing a stream's tracks to a file — a stream's last mile is
  the user's, through any MoQ client of a relay, never the runtime's (owner, 2026-10-17).
- **DECIDED** — The video codec backend seam has the video device seam's shape: a
  `VideoCodecBackend` opening a `VideoEncodeSession` or `VideoDecodeSession`, probed once per
  process through the shared walk and logged once, no dial, a refusing backend naming
  platform, direction and stream where no arm exists. It is the only way to a session —
  `GpuContext`'s session mints are crate-private — and decode hands back pictures already in
  pooled pixel buffers, as capture does. The four video blocks are platform-free above it and
  register on both floors. The VideoToolbox arm is hardware-only — a hardware codec block
  never falls back to software — runs without reordering, sets the sync-point cadence in
  seconds and frames, and maps `bitrate_bps` to average bitrate, constant quality otherwise.
  Encode never wraps the pooled frame, since CoreVideo wraps no `'RGBA'` IOSurface: the
  converter pass writes it on the GPU into an NV12 IOSurface from the compression session's
  own pool, `'420f'`/`'420v'` by the resolved range, with no host copy. Decoded IOSurfaces
  reach the pool through the camera's IOSurface → pooled-RGBA transport, read from the clean
  aperture, which CoreMedia sets from the SPS display window. VideoToolbox speaks AVCC with
  parameter sets in the format description; the arm converts at its own edge — 4-byte start
  codes, the parameter sets in front of every sync point and nowhere else — so the published
  access unit is one wire on both floors, through the one platform-free Annex-B walk
  (`core/annex_b_access_unit.rs`). What an arm
  cannot honour refuses by name at `setup()`: on VideoToolbox `effort_level`, a
  `keyframe_interval_seconds` of 0, and a decoder with no hardware support; on a Linux device
  without Vulkan Video, session construction names the missing direction instead of
  aborting. The decoders' `max_width`/`max_height` differ by floor: Linux sizes the DPB for
  the coded extent; on Apple, where CoreMedia reports only the cropped extent, a picture past
  the cap is refused. [macos-capability-parity — SHIPPED #2412, #2413; macos-platform-floor —
  SHIPPED #2374]
- **DECIDED** — An encoded frame is the stack's media frame and nothing around it: a varint
  timestamp in microseconds, then one Annex-B access unit with its parameter sets inline, which
  the port's catalog entry signals by carrying no `description`. No pooled-buffer or
  surface-id carriage for encoded bytes unless a measured need appears. The codec string —
  parsed from the SPS the arm emits, so no arm's profile is misstated — and the coded extent,
  before crop, ride the port's catalog entry, which the encoder writes after its first frame;
  colour rides the bitstream's VUI; a sync point is the envelope's `starts_group`; the
  timestamp rides the frame header like every message's. A message the decoder cannot read is
  refused by name, never reshaped — the audio wire codec's doctrine — and a decoded frame is
  an ordinary `VideoFrame`. Loss on an ordered encoded link is the engine's per-link count the
  reader exposes as `lost_since_last_read`: a decoder enters a stream only at a
  `starts_group` and, on a message reporting a loss, discards to the next `starts_group` —
  resuming at that message when it itself starts a group. A write the channel ceiling refuses
  makes the encoder's next frame a keyframe, counted in `graph`'s refused-write count. Owner,
  2026-10-16 and 2026-10-17. [codec-blocks — SHIPPED #2083, #2085; moq-relays — SHIPPED #2850]
  Rejected: a producer sequence inside the payload — a stock player stops on it (owner,
  2026-10-16).
  Rejected: codec-level gap detection — unreliable with hardware decoders (owner, 2026-10-16).
- **DECIDED** — A decoder publishes the display window, never the coded picture. Both
  codecs pad up to a block size — H.264 to the 16-sample macroblock, H.265 to the
  64-sample CTU — so a 1920×1080 source is coded at 1920×1088 by both, and only the
  window the SPS carries brings it back. Deriving that window is the engine's decode
  session's job and not a built-in's, let alone a consumer's: a consumer handed the
  coded extent cannot tell which of the two numbers it holds, and the padding rows are
  edge-replicated garbage. One helper derives it for both codecs, the session keeps the
  coded extent (parameter sets, DPB) separate from what it publishes, and a malformed
  window off an untrusted producer's bitstream is refused rather than wrapped into a
  plausible-looking one. Worth knowing at the seam: the decoded frame is cropped on the
  RGBA path and stays coded on the raw NV12 path, which is a direct DPB readback.
  [codec-blocks — SHIPPED #2086]
- **DECIDED** — Proof precedes surface: encode → decode is proven through the engine-owned
  rig (`runtime/streamlib-engine/tests/fixtures/`) before any codec block's API lands. The rig
  is two streams on `tatolabd`, as a deployment builds it (owner, 2026-10-19): the sender
  (`TestPatternSource` → the encoder) exposes the encoder's port, and the receiver reads that
  track at the local relay through `RelayTrackSource` and decodes it. The rig reads the
  sender's encoded track with `tatolab-read-track`, as any user reads a port, decodes the dump
  with the rig's ffmpeg into PNGs, and measures them with `cargo xtask psnr`, a first-class
  calculation in the proof tooling; the receiver's decode is proven by its decoded-frame count
  in `graph`. A codec block ships only with (i) a rig round trip carrying its lock, run through
  `/verify-live`, and (ii) CI-named GPU-free tests: bitstream parsing, VUI/color translation,
  config resolution, container bytes. Running the rig stays rig-only, and no test reaches into
  a consumer for its fixtures. [codec-blocks — SHIPPED #2084, #2085, #2086; moq-relays —
  SHIPPED #2860, #2866]
- **DECIDED** — The gate scores chroma, not luma alone: `cargo xtask psnr` classifies a
  frame Y ≥ 35 dB pass / 30–35 warn / < 30 fail **and** fails it outright when either
  chroma plane falls under 30 dB — one floor for every reference, no chroma warn band.
  The chroma floor is derived, not chosen: the lowest finite clean chroma figure is
  `complex_pattern`'s, 33.52 dB (H.264) / 33.42 dB (H.265) — three cold runs per codec,
  identical to 0.00 dB run-to-run, and 0.10 dB across codecs — and one whole-set run per
  codec confirms it carries the minimum, the next finite chroma reading in the set being
  48.13 dB. A fourth injection mode
  `swap-chroma` (Cb↔Cr transposition) lands with it and is what makes the floor
  non-vacuous — the other three (`swap-channels`, `bt601-bt709`, `range-swap`) are all
  caught by luma as well, so without a chroma-only regression the new floor would gate
  nothing. `solid_red` and `solid_green` are the two references that pass luma and fail
  on chroma alone; the mode is not luma-invariant on `complex_pattern` or `solid_blue`,
  where the inverse transform leaves gamut and the clamp moves Y too. What the chroma
  columns measure is the round trip's colour path — the two converters and the 8-bit
  TV-range wire — and not codec quality: a lossless codec through the same path scores
  `complex_pattern` within 0.2 dB of a real one. Every regression class the gate exists
  for (plane order, plane offset, subsampling filter, matrix, range) still reaches it,
  because all of them reach the decoded RGB. The scoring is pure math, GPU-free and
  CI-run; ffmpeg is not on the scoring path. [codec-blocks — SHIPPED #2085, #2094]
- **DECIDED** — The test pattern's drift lock is per codec, not per rig: `cargo xtask psnr
  bar-colors` measures each SMPTE bar's median colour, averaged across the decoded frames
  (owner, 2026-10-19), and the H.265 arm locks against `psnr_test_pattern_bar_colors_baseline_h265.tsv` and H.264
  against `psnr_test_pattern_bar_colors_baseline_h264.tsv`, tolerance ±0.05 per channel. Each
  bar is a saturated primary, secondary or neutral, so a matrix mis-interpretation moves the
  bars it touches by more than a whole-frame average would show — bt601/bt709 lifts the red
  bar's green by about 0.09. The split is headroom for a codec that reconstructs a saturated
  primary differently, so that it cannot be read as a colour regression.
  [codec-blocks — SHIPPED #2085, #2086; moq-relays — SHIPPED #2866]
- **DECIDED** — On the rig the decoder enters at the stream's first keyframe with zero frames
  discarded, and the encoder writes parameter sets on every IDR, so even a late subscriber re-enters within
  one GOP. A failed header extraction refuses rather than degrading to an empty header, and
  the minted header is checked with the engine's own NAL reader. Cam Link capture ran 18
  clean runs across release and debug at 1080p60 and 4K30 with no `DEVICE_LOST`. Decode runs
  at 3.75 ms/frame, so the decoder lag that triggered the H.265 shutdown race is absent; if
  sustained decoder lag reappears, re-run that scenario on the rig rather than assuming it
  fixed.
  [codec-blocks — SHIPPED #2084, #2085, #2086]
- **DECIDED** — The four video blocks reach Python as classes generated into
  `tatolab/stream/_built_in_nodes.py` from their descriptors, beside `CameraSource` — a processor
  extension owns none of this, being an ordinary Python node class the runtime never has to
  know about (extension-model). A block's `type` is `tatolab.stream:<Class>`. Configured the
  one way a built-in is configured — `stream_builder.add(H265Encoder)`,
  `stream_builder.add(H264Encoder, config={"keyframe_interval_seconds": 2})`, its keys
  checked by pyright against the generated `TypedDict` — and resolving on both floors,
  since the codec seam made the blocks platform-free and they register everywhere.
  `tatolabd` links all four and registers them with the other built-ins. The Rust
  descriptor's doc, carried into the generated class, is where a
  block's config keys and port names are written down, as it is for every built-in, and
  it states the engine's own behavior rather than an aspiration — the encoder's
  `width`/`height` guardrails that a mismatching frame wins against with a warning, its
  lazy session mint from the first frame, the decoder's eager mint at `setup()` — which
  refuses there by name where no hardware decoder exists, and on macOS holds
  `max_width`/`max_height` against the picture extent — and
  the `max_width`/`max_height` pair that warns and auto-detects from the first SPS when
  half-specified. What a Python app may wire follows from the engine half and is stated
  so the docstrings can say it: the encoder's `video` input takes any published
  `VideoFrame`, buffer-backed or texture-backed, while the decoder's `video` output is
  an ordinary `VideoFrame` on a pooled RGBA pixel-buffer surface — so a decoded frame
  reaches a Python kernel through a DLPack landing copy and never by bare surface id,
  which is the camera's existing gap carried, not a new one.
  [python-codec-block-api — SHIPPED #2105; macos-capability-parity — SHIPPED #2413;
  stream-graph — SHIPPED #2618; package-split-and-lend — SHIPPED #2684, #2693]
- **DECIDED** — The per-block ship bar the entry above states is met for a Python surface by
  the rig itself: its two streams are Python-authored, `codec_roundtrip_stream.py`, an
  engine-owned fixture stream beside `audio_loopback_stream.py` taking its codec from an
  environment variable, so there is no second rig and no Rust arm to agree with.
  [python-codec-block-api — SHIPPED #2107; moq-relays — SHIPPED #2860]
- **DECIDED** — An encoded audio packet is the encoded-frame convention applied to audio: the
  stack's media frame — the varint timestamp in microseconds, then one bare Opus packet as
  RFC 6716 §3 frames it — every packet opening its own group, so a decoder enters at any. The
  port's catalog entry is a hang audio entry: `codec` (`"opus"`), `sampleRate` (`48000`,
  Opus's own clock), `numberOfChannels`, and the `OpusHead` bytes in `description`, whose
  PreSkip is the encoder's lookahead in 48 kHz samples, which a decoder trims. The stamp rides
  the frame header and names the packet's first sample, carried from the window block the
  encoder consumed with the timestamped write. `OpusDecoder` reads the channels and the
  pre-skip from the upstream entry's `OpusHead`, falling back to the packet's TOC for the
  channels. Owner, 2026-10-16 and 2026-10-17. [opus-mp4-recording-rung — SHIPPED #2125;
  moq-relays — SHIPPED #2850]
- **DECIDED** — `OpusEncoder` is `execution = reactive`, `scheduling = high` like the
  video blocks, input `audio` declaring `delivery_profile = "ordered"` and
  `audio_window(sample_rate = 48_000, dtype = "f32", window_size = 960, hop = 960)` — no
  channel count — so the engine resamples and frames, and `process()` receives one 20 ms
  Opus frame per dispatch in the source's own channels. Framing is the window contract's
  job, never the encoder's. The encoder mints from the first block's `channels`, the video
  encoder's first-frame pattern: one or two channels through libopus's `Encoder`, three
  to eight through `MSEncoder` with channel mapping family 1 (the standard surround order
  both MP4 and WebRTC accept), more than eight refused by name; a block whose count
  changes re-mints, as an extent change re-mints video, and the port's catalog entry then
  names the new count.
  Output `encoded_audio`; the `OpusHead`'s pre-skip is the minted encoder's reported lookahead. Config,
  both optional so `{}` is legal: `bitrate_bps` (absent → libopus's automatic rate) and
  `application` (`"audio"`, `"voip"`, `"lowdelay"`; absent → `"audio"`). FEC and DTX off.
  [opus-mp4-recording-rung — SHIPPED #2125]
- **DECIDED** — `OpusDecoder` is `reactive`/`high`, input `encoded_audio` (`ordered`,
  declaring no window contract), output `audio` as `AudioBlock` messages: `f32`, `48000`, the
  stream's channel count, stamp derived from the packet's, published through the
  timestamped write; one or two channels through `Decoder`, three to eight through
  `MSDecoder`. No config. It enters at any packet and trims `pre_skip` at entry so its
  first emitted sample is the stamped instant. A read reporting lost packets is a gap:
  reset, re-enter, log the count, invent nothing — no concealment, no FEC decode.
  That is the drop-at-the-edge and flush-not-interpolate doctrine applied to a codec: a
  decoder that concealed a lost packet would invent 20 ms of audio, so the gap stays
  derivable from the stamps instead. [opus-mp4-recording-rung — SHIPPED #2125]
- **DECIDED** — Opus links statically into the wheel: libopus is BSD-3-Clause and
  royalty-free, its attribution rides the wheel's third-party-notices surface, and no
  `DT_NEEDED` entry appears — the dlopen arm is for system audio servers, never for a
  codec the wheel can carry. It arrives through the `opus` crate over `opusic-sys`, whose
  bundled libopus builds static by default; libopus's notice joins `VENDORED_CPP_PROJECTS`
  read from the crate's own `COPYING` in the registry checkout — the `shaderc-sys` shape
  generalised to a second build-script crate rather than a parallel mechanism — and the
  portability gate is the pass/fail: Opus adds no host library to the five the shipped
  `_engine.abi3.so` names.
  [codec-blocks; opus-mp4-recording-rung — SHIPPED #2125]
- **DECIDED** — `OpusEncoder` and `OpusDecoder` reach Python as generated classes like every
  built-in, with no Linux split — nothing here is platform-bound, so they register
  unconditionally beside the audio built-ins. Their descriptors' docs, carried into the
  generated classes, state the engine's own behavior rather than an aspiration: the encoder's
  window and first-block mint, its two config keys, the decoder's entry and gap rule.
  [opus-mp4-recording-rung — SHIPPED #2126, #2128; stream-graph — SHIPPED #2618;
  package-split-and-lend — SHIPPED #2684]
- **DECIDED** — The rung's CI-run, GPU-free proof: the stage with `channels` absent emits
  the source's count and converts when one is declared, in Rust and through the Python
  declaration; the link-naming read returns the link a synthetic frame was pushed on, with
  counting untouched; the Opus bodies against the real library with no `Runtime` — a tone
  through encode → decode within a stated floor for one, two and six channels, `pre_skip`
  aligning the first sample, a reported loss resetting.
  [opus-mp4-recording-rung — SHIPPED #2123, #2124, #2125, #2126, #2127]
- **DECIDED** — `requires_gpu` and said in the module docstring:
  `test_opus_blocks.py` — a Python known-signal source → `OpusEncoder` → probes →
  `OpusDecoder` → probes: every message is a media frame opening its own group under the
  port's `OpusHead`, packets are 20 ms apart, decoded blocks are 48 kHz `f32` in the
  source's channels.
  [opus-mp4-recording-rung — SHIPPED #2126, #2128]
- **DECIDED** — Live, one arm on an engine-owned fixture beside `audio_loopback_stream.py`
  and `codec_roundtrip_stream.py`. `opus_roundtrip_stream.py`: `KnownAudioSignalSource →
  OpusEncoder → OpusDecoder → CapturedAudioWaveformRecorder`, scored by
  `known_audio_signal.py` — tone identity and the DTMF timing grid intact within its own
  floor, a lossy codec's verdict being the analysis's, never a sample-exact match — with
  no audio device in the path, so a failure here with the loopback green is the codec's. The
  decoder's port is read as a track at the local relay with `tatolab-read-track`, a tool
  outside the runtime (§Networking), by `verify_audio_channel.sh`.
  [opus-mp4-recording-rung — SHIPPED #2126, #2128; moq-relays — SHIPPED #2860]
- **DECIDED** — The held codec consumers resolve through this align, per §Consumers: a held
  codec package is mined for its logic and deletes in the change that ships its block, and a
  codec proof example deletes into the engine-owned proof rig — its job becomes the rig's
  job, and a test owns its fixtures.
  [codec-blocks — SHIPPED #2087; opus-mp4-recording-rung — SHIPPED #2129]
- **OPEN** — Audio plugins (CLAP / VST3 / LV2): intended, do not build until a
  concrete consumer demands a specific plugin. Direction: CLAP first; the plugin runs
  out-of-process in its own helper over the engine's IPC transport, never in the app
  process; a plugin an app uses is declared project-locally — shipped in or referenced
  by the app's own project, the shader precedent — never discovered from
  machine-global scan paths; the lane costs nothing when unused (no `DT_NEEDED`
  entries, no import-time work). [audio-subsystem]
- **DECIDED** — Camera, microphone and local-network permission on Apple. `tatolabd` ships
  inside `Tatolab.app` as a launch agent the app registers with `SMAppService`, so macOS
  credits the app — its prompts name Tatolab, and grants are per user and persist across
  restarts and updates while the signing identity and bundle id hold. The app's `Info.plist`
  carries the camera, microphone and local-network usage strings; the app and `tatolabd`
  carry the hardened-runtime device entitlements; only the runtime process opens devices,
  never a processor interpreter. The runtime checks authorization before opening a device and
  refuses by name, because a denied microphone delivers silence. A runtime started from a
  terminal credits that terminal — the
  responsible code — and only Apple's Terminal is exempt from the local-network check.
  Owner, 2026-10-01, on #2560's research. [one-runtime-per-machine; runtime-hosting — SHIPPED
  #2721]

## Networking — transport, moq, webrtc — SHIPPED (unbuilt: a publicly valid certificate on the every-interface listener)

- **DECIDED** — Cross-language interop happens on the wire between nodes, as messages —
  never in-graph. [importable-python-library — SHIPPED #1715]
- **DECIDED** — WebRTC ships as an extension wheel under §Packages & extension model rather
  than as a built-in — not every app needs it, and a capability with a consumer is what proves
  the extension model. Its processors are ordinary processor extensions, and a runtime
  capability the wheel needs is exposed as engine code — a split of concerns, expected to be
  rare. WebRTC is an edge source/sink processor pair ingesting and egressing external
  streams at a runtime boundary. [extension-model; networking-extension-wheels — SHIPPED
  #2153]
- **DECIDED** — The wheel sits on the encoded side of the codec blocks and touches no raw
  frame, surface or GPU: `WhipPublisher` consumes the media frames `H264Encoder` and
  `OpusEncoder` write; `WhepPlayer` writes the same frames upstream of `H264Decoder` and
  `OpusDecoder`. Audio is in scope from the first
  rung. Both are ordinary processor extensions — `@node` classes in the wheel calling the
  wheel's own Rust — each in its own processor interpreter, on the tokio runtime the wheel
  brings up there on first use. [extension-model; networking-extension-wheels — SHIPPED
  #2151; package-split-and-lend — SHIPPED #2691; moq-relays — SHIPPED #2888]
- **DECIDED** — Many tracks are many links: the publisher takes one track per
  inbound link and derives its session media description from them. The player exposes one
  output per track kind — `encoded_video` and `encoded_audio` — never one port per track:
  ports are declared statically by decorator, and a decoder downstream wants a port it can
  name at wiring time. `WhepPlayer` takes no media name, because a WHEP answer names the
  session's media and there is nothing left to choose. Endpoint and credential configuration
  is ticket-level, as for every built-in's config.
  [extension-model; networking-extension-wheels — SHIPPED #2150, #2151]
- **DECIDED** — The WebRTC processors carry the stack's media frame as bytes: the publisher
  reads each track's codec off its upstream port's catalog entry and hands the access unit or
  packet after its varint timestamp to the wheel's Rust; the player writes that frame and
  fills what a decoder needs from the stream itself — the extent from the SPS, a sync point
  from the access unit as the group flag, the audio parameters from the session description
  — into its ports' catalog entries rather than from config. What crosses a network on a
  media track is a bitstream and the description a decoder needs, never a serialised link
  payload. [extension-model; moq-data-tracks — SHIPPED #2172, #2173; moq-relays — SHIPPED
  #2888]
- **DECIDED** — A received stream reaches the graph through the manual-source shape: the wheel's Rust receives on
  its own runtime and a processor-owned thread writes, so no engine seam is added for it. Two
  budgets the wheel lives inside, ticket-level but named here: a helper stops on the shutdown
  ladder §Processor model states, whose `teardown()` budget is five seconds, so a WHIP
  `DELETE` must fit inside it; and connecting inside `setup()` spends the sixty-second
  registration budget.
  [extension-model; local-transport-hardening — SHIPPED #2264, #2266]
- **DECIDED** — The proof bar is the codec blocks' two halves. CI-run, GPU-free and
  endpoint-free: RTP packetising and depacketising round trips, SDP construction and
  parsing, and the media frames and catalog entries the player writes checked against the
  wire contract. Live,
  rig-only: WHIP publish to Cloudflare Stream and WHEP play back from it, with credentials
  outside the tree, in the fixture-script shape the codec rig set (owner, 2026-09-04).
  [extension-model]
- **DECIDED** — `packages/streamlib-webrtc/`: a standalone maturin project — own
  `Cargo.toml` (`[workspace]` root, `[lib] name = "_native"`, `crate-type = ["cdylib"]`,
  `pyo3` on `abi3-py310`, `webrtc 0.17`, `tokio`, `hyper` + `hyper-util` + `hyper-rustls`,
  `rustls`, `bytes`; no engine crate), own lockfile, `pyproject.toml` naming `tatolab-webrtc`
  and depending on `tatolab-stream` by a minimum version, `python/tatolab/webrtc/` with
  `_native.pyi` and `py.typed`. `src/` carries the WHIP and WHEP sessions (`whip_session.rs`,
  `whep_session.rs`, over `http_signalling.rs`), the RFC 6184 depacketiser
  `h264_rtp_depacketiser.rs` with its tests, and `transport_stack.rs`, which brings up the
  tokio runtime and the rustls provider once, on first use, in whichever process the
  sessions run in. [networking-extension-wheels — SHIPPED #2150; package-split-and-lend —
  SHIPPED #2673, #2691]
- **DECIDED** — `WhipPublisher`: `@node`, one fan-in input `tracks` (`ordered`) —
  each inbound link is one RTP track, video or audio by its upstream port's catalog entry,
  the session's SDP built from the links `inbound_link_names` reports at
  `setup()`; config `url` and optional `bearer_token`. `WhepPlayer`:
  `@node(execution = "manual")`, outputs `encoded_video` and `encoded_audio`, config
  `url` and optional `bearer_token`; `start()` hands `ctx.outputs` to a processor-owned
  thread that connects, drains the session and writes media frames — a group starting at
  each IDR, the stamp from the RTP clock mapped onto the monotonic clock, codec, extent and
  Opus parameters stated in its ports' catalog entries — and `stop()` closes the session inside the 5 s budget. **No
  session is minted in `setup()`, and a refused connect is retried rather than ending the
  stream** (2026-09-05, found by the live proof): a WHEP endpoint answers `409 Conflict`
  while the input it fronts has not started publishing, the ordinary state of a player
  brought up beside its publisher, so the player carries a bounded backoff — a fresh session
  per attempt, since a closed peer connection
  cannot be dialled again. A write the engine refuses is the one failure not retried: it
  names its port and ends the thread, because reconnecting would spend an endpoint's session
  forever on a write refused every time. [networking-extension-wheels — SHIPPED #2150;
  moq-relays — SHIPPED #2888]
- **DECIDED** — `ci.yml`'s `integration-test-linux` and `integration-test-macos` jobs
  carry the extension lane for `packages/streamlib-webrtc`: on the runtime unit they built, build
  the extension into a venv holding `tatolab-stream`, `cargo test` and clippy over its
  crate, `mypy.stubtest` over its `_native`, pyright over its Python, and pytest with
  `-m "not requires_gpu"`, the portability gate over its `.so` among them.
  `release-please-config.json` bumps the extension with everything else, one
  version (§Distribution & versioning); the release builds `tatolab-webrtc` for manylinux
  and Apple Silicon and uploads it to PyPI beside `tatolab-stream`, attaching nothing.
  [networking-extension-wheels — SHIPPED #2152; package-split-and-lend — SHIPPED #2691,
  #2697]
- **DECIDED** — The proof, as built. CI-run, GPU-free, endpoint-free, owned by the wheel:
  the RFC 6184 packetise/depacketise round trip (the carried tests plus STAP-A and FU-A
  cases), SDP offer construction and answer parsing, and the player's media frames and catalog
  entries checked against the wire contract on the `wired_link` fixture pattern. Live, rig-only, under
  `/verify-live` with a networking arm, two streams on one runtime as the codec rig is: a
  sender publishes `TestPatternSource` → `H264Encoder` and the known signal over WHIP to
  Cloudflare Stream, and a receiver plays the same stream back over WHEP through the decoders
  — credentials read from the environment, absent ones reported as cannot-run, never as pass.
  The lock reads the receiver's `encoded_video` track at the local relay with
  `tatolab-read-track`, a tool outside the runtime, decodes it with the rig's ffmpeg and
  scores it with `xtask psnr bar-colors` against its own baseline within ±0.05; the
  receiver's decode is proven by its decoded-frame count in `graph` — the network sits inside
  a path the codec rig already scored, so a mismatch is the wheel's.
  [networking-extension-wheels — SHIPPED #2153; moq-relays — SHIPPED #2902]
- **DECIDED** — The address, as built. A port's address is `<machine>/<stream>/<node>/<port>`,
  every part cast as one address chunk (`core/runtime/address_chunk.rs`, one shared fixture
  of cases the engine and the pure-Python builder both cast from): the `/watch` path names
  it, and an exposed port is the track `<node>/<port>` of the broadcast
  `<machine>/<stream>.hang`. It carries no processor id; whatever owns the port resolves the
  node's name when asked. No engine type parses the string form (owner, 2026-10-19). The machine is named as the machine-name entry below
  states and read by `graph`'s top-level `machine`; a stream's name is cast at load.
  `runtime_id` stays per-run — logs, iceoryx2 names, the surface-sharing service — and is
  never an address.
  [runtime-mesh — SHIPPED #2282, #2284; zenoh-and-moq-wheel-removal — SHIPPED #2643, #2645;
  package-split-and-lend — SHIPPED #2693; runtime-hosting — SHIPPED #2715]
- **DECIDED** — A link's ends are both on this machine, in one stream. As built, a link end
  is `{node, port}`, a port of this stream's graph; the graph holds one link collection, which
  every traversal walks. A snapshot whose link end names a stream or a machine is refused by
  name at load, the refusal naming the relay source node, through which a stream reads
  another stream's port on this machine or another. In Python `connect(source, destination)`
  takes a reference minted by this stream's builder as its source, and a value of any other
  kind is refused naming the spelling that would work. The engine reads no payload to carry a
  link. [zenoh-and-moq-wheel-removal — SHIPPED #2643; runtime-hosting — SHIPPED #2715]
- **DECIDED** — Reading across streams is always pulled, and the source is never asked. The
  stream that wants another stream's port subscribes to it at a relay — through the relay
  source node, the preview page, or any MoQ client — and nothing is sent for a track nobody
  subscribes to. No runtime pushes its output into another's input, and no runtime wires two
  others. Owner, 2026-10-04: a source never wires itself into a reader, the way a server never
  wires its URL into a client's browser. [exposure-levels; moq-relays — SHIPPED #2853, #2855]
- **DECIDED** — Every stamp in the tree is this machine's. The one monotonic clock of §Media I/O
  is one clock per machine, and a stamp is never compared against one from another clock. No
  link carries a stamp from another machine, so a many-track sink compares first stamps across tracks with no another-machine refusal. A
  message read from a relay reaches a graph through the relay source node, which stamps it
  from the track's own timestamp re-based onto this machine's clock — a fixed offset taken
  from the first object, re-anchored on a discontinuity, never per-message arrival time, the
  stack's own player's model — with the timestamped write; a track carrying no timestamp is
  stamped at arrival. The relay's object carries the writer's stamp and the node's doc says
  both. Owner, 2026-10-17. [runtime-mesh; zenoh-and-moq-wheel-removal — SHIPPED #2643;
  moq-relays — SHIPPED #2855]
- **OPEN** — A common clock across machines: intended, do not build until designed. Direction:
  machines negotiate a shared network time (PTP, NTP or similar) so stamps from different
  machines become comparable; until then the per-clock rule above stands, and the relay
  source node's restated stamp is a local stamp like any relay node's. [runtime-mesh;
  cross-runtime-links; moq-relays]
- **OPEN** — An adaptive jitter buffer: intended, do not build until designed. Direction: a
  playing sink holds what it plays at a steady delay sized from the jitter it measures, the
  way WebRTC's NetEQ does, separate from a relay reader's `max_age`. Owner, 2026-10-20.
  [relay-track-reading]
- **DECIDED** — The runtime decides what leaves the machine; who reads at a relay is that
  relay's own access rules. [one-runtime-per-machine; moq-relays — SHIPPED #2842, #2853]
- **DECIDED** — Tatolab solves no networking problem. Reach between machines, a peer's
  identity, encryption on the wire, NAT and names are a relay's, or the network the user
  already has — a tailnet, a LAN, the internet. The runtime reads, configures and requires
  nothing of Tailscale: no status, no `tailscale serve`, no certificate, no name; reaching a
  machine's local relay over a tailnet is a docs page, because the relay listens on the
  machine and a tailnet reaches the machine. Nothing is built for discovery, certificate
  minting, name claiming or peer authentication, on any network. Owner, 2026-10-16.
  [one-runtime-per-machine; moq-relays — SHIPPED #2842]
  Rejected: reading Tailscale's status for the machine's name and the tailnet's machines, and
  having `tatolabd` configure `tailscale serve` — a rights prompt and a manual step on every
  install, for reach the local relay already gives (owner, 2026-10-16).
- **DECIDED** — The address is `<machine>/<stream>/<node>/<port>`: the runtime is addressed by
  its machine, and the same string is the address in Python, the CLI, the URL path and the
  graph (owner, 2026-09-30). Collisions (owner,
  2026-10-02): a second user's runtime on one machine is refused, not suffixed (§Product). A **stream** name
  is unique per machine, defaults to its function's, and a second
  load of a name is refused naming where the first came from, with `--name` the way out, which
  covers two packs that each define a `main`; a **node** name is unique per stream — a
  defaulted duplicate (two unnamed `CameraSource`) is suffixed `-2`, `-3` …, while a duplicate
  the author *typed* is refused by name, because a typed name is an address; a **port** is a
  method name on its class, and two casting alike are refused at `@node` — or a Rust
  `#[processor]` — naming both. Every exposed name — machine, stream, node, port — is cast,
  never refused for its spelling: lowercased, accents dropped, every character outside RFC
  3986's unreserved set (`a-z 0-9 - . _ ~`) turned into `-`, at most
  63 characters; one that casts to empty, `.` or `..` is refused by name (the cast's detail, not
  the owner's: runs of `-` collapsed, ends trimmed, a suffix the next unused one with the name
  truncated to fit, a lookup casting its argument so `read("Video")` finds `video`). So
  `CameraSource` is `camerasource`, `name="Front Camera"` is `front-camera`,
  a hostname `Jonathans-MacBook` is `jonathans-macbook`. What a person writes — class names,
  files, imports, the string passed — is never constrained; a node's `type` stays its import
  path. Uniqueness is of the cast name: two typed names casting alike are a typed duplicate.
  Owner, 2026-10-02. [one-runtime-per-machine; stream-graph — SHIPPED #2614, #2618, #2658;
  runtime-hosting — SHIPPED #2715]
- **DECIDED** — A machine's name is its hostname, cast, and its name at a relay is the root
  that relay's credential assigns. The engine names the machine by its hostname cast and reads
  nothing of Tailscale; a relay adapter's root prefixes the broadcast at that relay.
  [runtime-hosting — SHIPPED #2715; moq-relays — SHIPPED #2842, #2873]
- **DECIDED** — Exposure: every output port of a stream is internal, private or public, and the
  runtime enforces it at the stream's edge, never inside the stream. **Internal**, the default:
  any node of the stream may link to it, and it is at no relay. **Private**: at the machine's
  local relay's loopback listener only, so a stream, a script, the preview page or any MoQ
  client on the machine reads it and nothing off the machine can. **Public**: at every relay
  the machine has joined, the local relay's every-interface listener included.
  `stream_builder.expose(output)` makes an output private and
  `stream_builder.expose(output, Exposure.PUBLIC)` public; a level is an enum member, never a
  string. The stream's function sets where its exposures start; `expose` at
  the CLI, the app or the local API changes them while the stream runs, the change applies at
  once — a reader the new level no longer allows is cut off, a relay subscriber included — and
  neither the runtime nor the stream restarts. The engine checks the live exposures wherever a
  read crosses a stream's edge, and a port is read from outside its stream only through
  exposure: no debugging door bypasses it, and a stream's own logs are how its
  insides are seen. Nothing leaves the machine until a port is public. The CLI and the app
  list every running stream with its private and public ports; an internal port may be
  listed and is never readable. Owner, 2026-10-04 and 2026-10-16. [exposure-levels;
  one-runtime-per-machine; stream-graph — SHIPPED #2708; moq-relays — SHIPPED #2853]
- **DECIDED** — Every exposed port is a track at a relay, named by its address, and that is the
  one way a port is read from outside its stream — by another stream through the relay source
  node, by the preview page, by a script, by ffmpeg-side MoQ tooling, by another runtime,
  all alike. A user with no account sees and uses their own streams through the local relay
  (owner, 2026-09-30). [one-runtime-per-machine; moq-relays — SHIPPED #2853, #2855, #2864]
- **DECIDED** — The runtime's HTTP is on loopback only and carries what is not real time:
  the listing of the machine's streams and exposed ports, the preview page, and at
  `/machine-runtime-lock-holder` the serving runtime's pid, which names the macOS machine
  lock's holder (§Product). Its paths are
  the address: `/watch/<machine>/<stream>` opens the preview on a broadcast with its tracks to
  pick from, `/watch/<machine>/<stream>/<node>/<port>` straight onto one track. The listing
  shows private and public ports; an internal port is listed and not served. The runtime's
  HTTP serves no media and listens nowhere but loopback; what the local relay's own listener
  serves over HTTP beside its tracks — its certificate fingerprint, its WebSocket path and the
  relay program's `/fetch` — is the relay's, under the same grant as the tracks. The read-only
  MCP, the bounded-sample form and the snapshot form are not built. Owner, 2026-10-16,
  2026-10-17 and 2026-10-24. [moq-relays — SHIPPED #2860, #2864;
  machine-lock-without-administrator — SHIPPED #2929]
  Rejected: live forms over HTTP served by the runtime — MPEG-TS, HLS, raw H.264, fMP4 — and a
  `whep` form (owner, 2026-10-04).
  Rejected: a snapshot form — one PNG of a port's latest frame, the runtime reading the port's
  payload to make it — a door beside the relay; a frame is seen through the preview page or
  any MoQ client (owner, 2026-10-17).
- **DECIDED** — The runtime has no door for verification, and verification gets none. Every
  end-to-end proof — the codec rigs, the audio loopback check, the evidence an agent captures —
  is a deterministic tool outside the runtime that reads a port the way a user does, as a MoQ
  client of a relay or through the preview page, and nothing of it lives in the engine. A port
  is inspected by exposing it — private keeps it on the machine — or by chaining nodes off it
  at the access level its author chooses, and an internal port is seen through its stream's
  logs alone. The engine's internals are proven by unit tests; the engine is never proven by
  building an app on it. Owner, 2026-10-17. [moq-relays — SHIPPED #2860, #2875]
  Rejected: `exchange` kept as an engine-internal verification door, an image-writing built-in
  for the rigs, an MCP image tool — side paths past exposure and the relays, which agents
  reach for over the product's own doors (owner, 2026-10-17).
- **DECIDED** — End-to-end encryption through a relay is not a launch requirement; it is a
  later change, and the design keeps it possible. A hosted relay's operator can see the
  streams passing through it until that change lands, and that is said plainly; a
  self-hosted relay is the user's own and exposes nothing. What
  stays true so the door stays open: a relay never parses the payloads it forwards; the engine
  never reads a payload; a message's group, stamp and envelope stay readable so delivery and
  drop counting work; and no key ever lives on a relay. When it lands it is the
  WebRTC insertable-streams and MoQ secure-objects shape — encrypt each payload at the source
  inside the transport encryption, decrypt in the reader, relays untouched — with a pre-shared
  key per team first (carried to a browser in a URL fragment, which never reaches a server),
  per-stream keys through the control client later, and the team's devices signing the key
  set the way Tailnet Lock does. Its cost, accepted for later: stock MoQ tooling and curl then
  work only through a TLS tunnel ending on the machine. Owner, 2026-09-30. [one-runtime-per-machine; moq-relays]
- **DECIDED** — Off a stream, data travels over MoQ on QUIC and over nothing else, to relays.
  Inside a stream, nodes share data through shared memory, as today. The runtime serves no MoQ
  endpoint of its own: it is a MoQ client of every relay it has joined, and the local relay —
  `moq-relay`, shipped in the runtime unit on both floors, started and supervised by `tatolabd`
  as child processes with their own configuration, one per listener — a loopback listener and
  an every-interface listener (the adapters entry below), each serving its tracks, its
  certificate fingerprint and its WebSocket path on one port, UDP and TCP — is the relay every
  machine has. It is a separate program the runtime joins like any other; the runtime is never
  a relay. Only the runtime publishes to either listener, and anyone may subscribe to what a
  listener serves; how the local adapter configures that — the relay's own admission knobs, an
  auth responder the adapter answers over a unix socket, a credential it mints for the runtime
  — is the adapter's, driven by its `relays.json` instance. `tatolabd` provisions the
  listeners' certificates as files the relay reloads, rotating them before they expire: on
  loopback a short-lived certificate the player pins by its fingerprint; where the machine has
  a reachable hostname, a publicly valid certificate issued programmatically, so a browser off
  the machine trusts it with nothing pinned — the issuer is a research question, not decided
  here. Owner, 2026-10-16 and 2026-10-17. [moq-relays — SHIPPED #2842, #2853]
  Rejected: embedding `moq-relay` as a library inside `tatolabd` to save the loopback hop — the
  runtime would then be a relay (owner, 2026-10-16).
  Rejected: the engine serving a MoQ endpoint itself on its tailnet address, read directly by
  other machines — a second serving path beside the relays, and a Tailscale requirement
  (owner, 2026-10-16, over the 2026-10-04 ruling).
- **DECIDED** — One publisher per runtime, and it holds no logic. It lives in `tatolabd`
  beside the relay adapters and reads the engine's exposed ports through one engine seam. It
  watches exposure across every loaded stream: a port turning private or public is announced
  at the relays its level reaches, one turning back is withdrawn, which cuts its subscribers
  at once. Per exposed port it holds one reader on the port's channel — the subscriber slot
  every channel reserves — and forwards each message's payload bytes as
  written to every joined relay that has a subscriber, stamping each object from the
  message's envelope and nothing else. Under backpressure, and on a message whose
  `lost_since_last_read` is nonzero, it drops the rest of the current group and resumes at
  the next `starts_group` — the message reporting the loss, when that message itself starts a
  group — never a message inside a group. It decodes no payload, imports no class, and knows
  no port's encoding. Which exposed ports it offers is a
  predicate that says "all": anything can be published and any port can go public, and a port
  whose bytes mean nothing off the machine — a `VideoFrame` record naming a surface — is
  forwarded all the same, so what was published is what a subscriber sees. Owner, 2026-10-16
  and 2026-10-17. [moq-relays — SHIPPED #2853]
  Rejected: the publisher refusing, warning on or filtering a port by what it carries — logic
  in the forwarding path (owner, 2026-10-17).
- **DECIDED** — A stream is one broadcast; each exposed port is one track in it. At a relay a
  stream is the broadcast `<root>/<machine>/<stream>.hang` — `<root>` the prefix the relay's
  credential assigned, the suffix the players' convention for a catalog they read — and each
  exposed port is the track `<node>/<port>` inside it, so four cameras and a microphone in one
  stream are five tracks any client subscribes to by name; the stack's player shows one video
  rendition at a time, and the preview page names the track in its URL. The relay's
  own listing is the directory: everything under `<root>/<machine>/` is that machine's streams,
  and the broadcast's catalog is what is in one. A stream that wants two unrelated bundles is
  two streams. Owner, 2026-10-16. [moq-relays — SHIPPED #2853]
  Rejected: one broadcast per exposed port — it loses multi-track switching and audio beside
  video, the things a broadcast is for (owner, 2026-10-16).
- **DECIDED** — A message's group boundary is the writer's, in the envelope. `write(port,
  payload, starts_group=True)`: the default says this message stands alone, so every data
  port is one group per message and its author never sees the flag; `starts_group=False`
  continues the group the last message opened. The built-in encoders set it themselves —
  `True` on a keyframe, `False` otherwise — and a node emitting encoded media sets it from its
  own keyframe knowledge; `OpusEncoder` opens a group per packet. The publisher maps it to the
  relay's group boundary and reads nothing else; Python and Rust readers see it alike, beside
  the stamp, and the envelope's stamp is the object's. Owner, 2026-10-16 and 2026-10-17.
  [moq-relays — SHIPPED #2821, #2850, #2853]
- **DECIDED** — The catalog is assembled from port descriptions, never from payloads. An output
  port's declaration may carry, beside its name and description, its catalog entry in the
  layout the stack's players read: the section it belongs in — `video`, `audio`, `text`,
  `json`, `binary` — and that section's own fields, `codec`, `codedWidth` and `codedHeight`,
  `sampleRate`, `numberOfChannels`, `description` for a decoder's out-of-band setup, and on a
  data track `mode` (`snapshot` or `stream`) and `mime`. The node that owns the port writes its
  entry from its config and from what it learns at its first frame — an encoder its codec
  string and coded extent — and replaces it while running; a built-in whose payload is a
  msgpack record declares `binary`, `mode: snapshot`, `mime: application/msgpack`, and no
  reader in the runtime consults a data track's `mode`. The
  publisher merges the entries of a stream's exposed ports into the broadcast's catalog track
  as text, one entry per track named `<node>/<port>` under its section, and `graph` renders
  each port's entry. Any reader may read a port's entry — a local node off its inbound link,
  a remote client off the catalog — and it is documentation a reader follows: `connect`
  compares nothing, the engine checks nothing against it, and no port is required to carry
  one. Owner, 2026-10-16 and 2026-10-17. [moq-relays — SHIPPED #2840, #2853]
  Rejected: a port declaration the engine acts on — a media opt-in the publisher reads payloads
  under — media logic in the adapter path (owner, 2026-10-16).
  Rejected: an entry in the runtime's own words — a `kind`, a free-text `encoding`, a `cast`
  import path — for the publisher to translate into the players' layout; a port describes
  itself in the layout the players read, so one malformed entry breaks its own track and
  never the broadcast (owner, 2026-10-17).
- **OPEN** — A decoded audio port playable by any MoQ player: intended, do not build until
  designed. Direction: an exposed decoded audio port publishes an `audio` entry with the `pcm`
  codec and raw interleaved little-endian f32 objects in place of its `AudioBlock` records.
  Owner, 2026-10-20. [relay-track-reading]
- **DECIDED** — Relay adapters carry what differs between relays, behind one interface. A
  relay-adapter SDK crate holds the trait — joining, publishing, the events and calls the app
  and the CLI drive, how a credential is held and refreshed — and one crate per adapter
  implements it: the local relay, Cloudflare and moq.pro; Red5 waits on its cost. What differs
  is the MoQ dialect the relay speaks (`moq-net` negotiates moq-lite and the IETF drafts;
  Cloudflare's relays are on the drafts, `moq-relay` on moq-lite), how a machine
  authenticates and refreshes its credential, the root the relay assigns the machine or
  leaves the adapter to name, and how often each broadcast's catalog is written again whole
  for a relay that hands a subscriber joining late only the objects written after it
  subscribed — Cloudflare's, every second — which the publisher does when its adapter says
  so (owner, 2026-10-20). Every adapter is compiled into `tatolabd`, so a headless machine
  has them all, and any number of instances of each is added and configured through
  `tatolabd`, by the app or at the CLI; `relays.json` holds the instances, each with its own
  settings and whatever credential its provider's SDK needs, held in the OS secret store and
  never written to a log — a token that rides a URL is redacted with the URL. Signing in to a
  provider and provisioning a relay live in the app or a separate private service, never the
  runtime. The local relay's read boundary is where it listens, never a token: it has two
  listeners — one bound to loopback alone, serving every exposed port, private and public,
  which is what the preview page and a relay source node on the machine dial; and one bound to
  every interface, serving public tracks only, which is what another machine dials. A private
  port is therefore readable only by a process on the machine, enforced by the kernel's
  loopback binding; the two listeners are two `moq-relay` children, since the program binds one
  address, and the local adapter's two instances. Owner, 2026-10-14, 2026-10-16 and
  2026-10-17. [moq-relays — SHIPPED #2842, #2871, #2873]
  Rejected: private tracks on an all-interfaces listener behind a token minted over loopback —
  a bearer token carried off the machine reads them from anywhere (owner, 2026-10-17, from the
  review of the align PR on 2026-10-16).
  Rejected: one trait inside `tatolabd` with no crate per adapter — adding a relay would mean
  editing the daemon (owner, 2026-10-17).
- **DECIDED** — Reading a track is a source node. The relay source node, a built-in, takes a
  relay, a broadcast and a track, subscribes, and writes each object's payload bytes to
  its one output as received, stamped from the track's own timestamp re-based onto this
  machine's clock (the stamps entry above); the track's catalog entry is readable off the
  node. What follows it — a decoder, a numpy node, nothing — is
  the builder's. It is how a stream on this machine reads another stream's private port, how
  a stream on another machine reads a public one, and the only way a graph reads a relay.
  Nothing in the runtime is built for runtime-to-runtime: the node is one MoQ client among
  any. Owner, 2026-10-16. Its config is MoQ's own vocabulary and nothing else: `relay_url`,
  `broadcast`, `track` and `max_age` (the entry below), both names required and each read exactly as written — a stream's
  exposed port `desk/camera-rig/encoder/encoded_video` is the broadcast `desk/camera-rig.hang`,
  the track `encoder/encoded_video`, as the preview page and any MoQ client show it. The
  runtime casts every name it gives, and a name another publisher's relay accepts is read as
  that publisher wrote it, `.hang` (moq-dev's catalog-format suffix, a convention our
  publisher follows) or not. Tatolab's `<machine>/<stream>/<node>/<port>` address is not one
  of the node's names. Owner, 2026-10-18. [moq-relays — SHIPPED #2855, #2858;
  relay-track-reading — SHIPPED #2898]
  Rejected: naming the track by a Tatolab address, `track_address`, beside `broadcast` and
  `track` — two ways to name one thing, one of them a term invented where MoQ has one (owner,
  2026-10-18).
  Rejected: casting a name the node reads — casting is for names the runtime gives (owner,
  2026-10-18).
  Rejected: the engine subscribing to a relay inside the link machinery, so a link's source
  can name a port on another machine — a MoQ reader in the engine, a second link kind, and a
  stamp-marking question with no answer (owner, 2026-10-16).
- **DECIDED** — A relay track is read through one relay-track reader, which the relay source
  node and `tatolab-read-track` share. It receives a track's groups in arrival order and
  releases them in sequence order, holding at a missing sequence. A missing group is given up
  only when its stream is reset after its header arrived, when the track ends, or when newer
  content on the track has run the reader's `max_age` past where the missing group would end,
  measured in the track's own timestamps — never on a clock, and never because a newer group
  arrived first. A group that arrives in sequence is released at once, so the hold adds no
  latency unless a group is missing. The reader joins at the live edge, replaying no cached
  history, and subscribes with its own `max_age`, so the relay sheds nothing the reader would
  still take. `max_age` is the reader's: the relay source node's defaults to 100 ms, and
  `tatolab-read-track` waits the track's declared retention. It is a transport budget, how late
  a group may arrive and still be used, and never a playout delay. The relay source node
  writes every group it gives up as a loss its readers count in `lost_since_last_read`, the
  way a link counts one, never as a silent step. Owner, 2026-10-20. [relay-track-reading —
  SHIPPED #2898, #2900]
  Rejected: moq-net's sequence cursor (`.ordered()`) — it steps over a group whose stream lands
  after its successor's, at any budget (session, 2026-10-20).
  Rejected: giving up a missing group when the budget elapses on a clock — waits and sleeps
  never fix realtime streaming (owner, 2026-10-20).
  Rejected: one budget for every reader, the track's 5 s retention — one lost group becomes
  5 s of silence and then a burst (owner, 2026-10-20).
- **DECIDED** — The engine stands on the moq-dev line of MoQ: `moq-net`, `moq-tokio`, `hang`
  and the `moq-relay` program, each pinned to an exact version — the 0.3, 0.19, 0.21 and 0.17
  lines — and every payload a stock
  player reads is in that line's media layout, every catalog in its catalog layout. Known
  (2026-10-17): Cloudflare's hosted relays are a beta speaking IETF drafts 14 and 16, which
  `moq-net` negotiates; a Cloudflare token rides the URL path; Cloudflare assigns no root, so
  its adapter names the machine's namespace; moq.pro's credential is a project signing key
  that mints read and write for every broadcast. The line's own compatibility claim is
  otherwise unverified here. Owner, 2026-10-04 and 2026-10-16. [moq-relays — SHIPPED #2842,
  #2853]
- **DECIDED** — The preview page is a lightweight built-in viewer, not a product: a small web
  project in the repo — `package.json`, `index.html` and one script on the stack's own browser
  player — built in the release pipeline the way the crates are and staged into the runtime
  unit, served by the runtime's loopback HTTP, reading the local relay alone at
  `http://<host>:<port>` so the player pins the relay's certificate by its fingerprint, and
  playing the track its URL names. Every browser plays it: Safari and iOS over the relay's
  WebSocket path, the rest over QUIC. A hosted relay has its own player at its own URL. Known
  (2026-10-17): the player's dependency tree carries an LGPL-2.1 Opus decoder polyfill, named
  in the third-party notices. Owner, 2026-10-04, 2026-10-16 and 2026-10-17. [moq-relays —
  SHIPPED #2864]
  Rejected: a `?relay=` parameter pointing the preview page at another relay — every hosted
  relay already has its own player (owner, 2026-10-16).
  Rejected: the player's published package copied in with no build — it is a library of many
  packages a browser cannot load unbundled (owner, 2026-10-17).
- **OPEN** — Left for later changes, built against by nothing: a bounded-sample form for data
  ports; end-to-end encryption (the entry above); a CMAF-emitting node for HLS players and
  stock MoQ tooling; Red5's adapter; and a zero-copy
  local fast path behind the relay source node for two streams on one machine. Known
  (2026-10-16): through the local relay a raw frame between two streams on one machine pays an
  encode, two loopback hops and a decode, about ten milliseconds and a lossy codec; data
  pays about a millisecond. [moq-relays]

## Language SDKs & parity — SHIPPED

- **DECIDED** — Python is the sole focus runtime: `tatolab-stream` is the primary authoring
  surface. TypeScript authoring is paused, not rejected — a future TypeScript SDK follows
  this same model (a stream package a TypeScript app imports, beside the one runtime; Deno
  itself optional), aimed at the hobbyist / video-creator audience when it is scheduled.
  [importable-python-library — SHIPPED #1707, #1708; importable-python-library-ripout
  — SHIPPED #1715; package-split-and-lend — SHIPPED #2686]
- **DECIDED** — The Python SDK carries a GIL-release contract: every native binding
  that can block releases the GIL around the blocking call, and pixels never cross
  into Python as Python-owned objects — frames travel as handles / surface ids, and
  pixel memory is reached only through explicitly exported views (DLPack, the CUDA
  Array Interface, a mapped CPU buffer). The contract exists so a blocking native binding
  never stalls the other threads of its processor interpreter. It is never a
  co-tenancy remedy: no two Python processors share an interpreter.
  [importable-python-library — SHIPPED #1707, #1708; helper-process-placement-only —
  SHIPPED #1714; package-split-and-lend — SHIPPED #2693]
- **DECIDED** — `tatolabd` owns SIGINT, SIGTERM and — on Linux — SIGHUP through the whole
  teardown, engine drop included, and escalates on repeat: the first interrupt stops the graph
  gracefully; the second forces it — every helper's ladder skips to terminating its process
  group, and a native processor thread still inside its callback is abandoned; the third
  kills every helper's process group and exits with status 130 at once. A native processor
  thread that ignores shutdown past its budget is abandoned rather than joined: the engine
  stays alive beneath it, never dropped before the process exits, and `tatolabd` writes a
  refusal naming each abandoned processor and exits non-zero, while a forced shutdown that
  abandoned nothing exits zero. An engine-chosen watchdog of about fifteen seconds, armed
  when the teardown starts, ends a teardown hung anywhere else: it logs what is still
  running and ends the process with status 124, distinct from the third interrupt's 130. One
  stream's own shutdown runs under that stream's watchdog (§Processor model, several streams).
  The per-user service the installer registers starts `tatolabd`, which never detaches itself.
  [shutdown-ladder; local-transport-hardening — SHIPPED #2266; package-split-and-lend —
  SHIPPED #2693; runtime-hosting — SHIPPED #2704, #2721, #2722]

## Distribution & versioning — IN-FLIGHT (unbuilt: the Rust crate's `tatolab-stream` name, and `Tatolab.app`'s first launch without an administrator prompt)

- **DECIDED** — The release publishes `tatolab-stream` and the first-party extensions to
  PyPI, where a stream's venv takes them like any other dependency, beside the `streamlib`
  crate for Rust apps, all at one version (the entry below); it publishes no engine wheel,
  and the runtime unit ships inside the installer (the entry below). Positioning is
  "realtime engine, Python authoring" — the Rust engine is named as material; never marketed
  as "a Python library". Owner, 2026-10-08.
  [importable-python-library — SHIPPED #1691, #1692, #1694, #1711; package-split-and-lend —
  SHIPPED #2697; runtime-hosting — SHIPPED #2721, #2722; amended by tatolab-names: the crate
  is `tatolab-stream`]
- **DECIDED** — One version number for everything Tatolab releases from this repository:
  `tatolab-stream` on pip, the Rust `tatolab-stream` crate, the runtime unit the installer
  ships — `tatolabd`, the `tatolab` CLI, the desktop app and `tatolab.runtime` — and the
  first-party extensions (`tatolab-webrtc` today) carry one version and are released
  together, every piece at every release, whether or not it changed. An extension still depends
  on `tatolab-stream` by a minimum version, as a third party's package does, and an extension
  that leaves this repository takes its own number. The number is for people, never a
  dependency of a stream: nothing in a stream, its `pyproject.toml` or its graph names a
  runtime version, and nothing compares versions to admit a stream. A runtime at least as new
  as a stream's `tatolab-stream` runs it; an older one refuses what it lacks by name, and the
  refusal names the runtime's own version. Owner, 2026-10-02. [package-split-and-lend —
  SHIPPED #2671, #2691, #2697]
- **DECIDED** — Wheel portability model: what the host may supply is stated per platform,
  and nothing else is linked. On Linux, system libraries (libcuda, the audio servers) are
  dlopen'd at runtime, never linked — the wgpu/opencv-python manylinux shape. A stock machine
  on either floor may have no Vulkan driver, so the lend carries one in
  `tatolab/runtime/_vulkan_driver/`: on macOS the Vulkan loader (built from source at the
  wheel's deployment target), MoltenVK and its unedited ICD manifest, linking only
  `/usr/lib/` and `/System/`; on Linux the Vulkan loader and Mesa lavapipe built with LLVM
  linked in and no window-system code, from one Mesa tag pinned by SHA-256, and its unedited
  ICD manifest — each linking only what the host may supply, proven for every native binary
  the lend carries, not only `_engine`. Engine and helper alike dlopen that loader by
  absolute path, found beside the `_engine` image through `dladdr` — on Linux before the bare
  soname, so every Linux machine runs the one loader the unit was tested with and the host's
  drivers and layers stay reachable through its own search and variables; on macOS after the
  bare names and `VULKAN_SDK` and before the Homebrew prefixes, which are developer-machine
  fallbacks.
  `tatolabd`, and `tatolab/runtime/__init__.py` in a processor interpreter, name the bundled
  manifest to the loader additively — through
  `VK_ADD_DRIVER_FILES`, before `_engine` loads, idempotently for a re-importing helper, and
  not at all when `VK_DRIVER_FILES` or `VK_ICD_FILENAMES` says the user chose their drivers —
  so a user's own driver stays discoverable. The MoltenVK carried is 1.4.1 or later: camera
  zero-copy under §Media I/O imports IOSurface memory through `VK_EXT_external_memory_host`
  in its spec-correct form, which 1.4.0 refuses and 1.4.1's source is the first to accept
  (owner, 2026-09-21, #2359). It is the Khronos release build, pinned by tag and SHA-256,
  thinned to arm64 and carried unpatched: a driver patch was ruled out (owner, 2026-09-26,
  #2488), so what MoltenVK refuses stays refused. "Baked in" means our Rust is compiled in,
  not that system deps are static. abi3 across a small range of GIL-enabled CPython builds
  only (free-threaded builds wait for the stable ABI to exist for them). "Our code" includes
  vendored C/C++ we compile and link statically, not only our Rust: the wheel carries a C++
  GLSL shader compiler so a kernel author needs no system shader toolchain. The wheel's
  adapter closure excludes skia. Processor interpreters import the lent `tatolab.runtime` —
  one native artifact, no separate helper cdylib. The portability proof parses ELF and
  Mach-O itself: every Mach-O the lend carries links only the system, carries
  `LC_CODE_SIGNATURE` — verified by `codesign --verify --strict` where the host has it, since
  a byte rewritten after signing keeps the load command — and names in `LC_BUILD_VERSION` a
  macOS no newer than the wheel's tag. A binary it cannot parse, a fat binary, or a
  `.so`/`.dylib` that is neither format fails rather than skips.
  [importable-python-library — SHIPPED #1691, #1692; python-kernel-surface — SHIPPED #1775;
  macos-platform-floor — SHIPPED #2362; package-split-and-lend — SHIPPED #2689, #2693;
  optional-accelerators — SHIPPED #2761]
  Rejected: the system loader before the lend's on Linux — a distribution loader older than
  1.3.207, which Ubuntu 22.04 and Debian 11 ship, never reads `VK_ADD_DRIVER_FILES` and so
  hides the bundled driver (owner, 2026-10-14).
  Rejected: the system loader when it reports 1.3.207 or later, else the lend's — users run
  loaders the unit was never tested with (owner, 2026-10-14).
- **DECIDED** — The macOS runtime unit is built for `aarch64-apple-darwin` at the same abi3
  floor as the Linux one, at the same version. Apple Silicon only: no
  Intel wheel, no universal2, and Rosetta is not a supported path — not as a fallback, not
  as a courtesy (owner, 2026-09-19). It is built on a pinned `macos-26` runner, the owner's own
  macOS, because the image decides the SDK. The deployment target is macOS 15.0, pinned once in
  `[tool.maturin.target.aarch64-apple-darwin]` and exported as `MACOSX_DEPLOYMENT_TARGET`,
  because maturin tags the wheel with it but does not hand it to the link; the bundled
  loader is built to it. 15.0 sits below the build SDK on purpose: it
  clears MoltenVK's floor (12) and AVFoundation's `AVCaptureDeviceTypeExternal` (14), and
  every Apple Silicon Mac can run it; a Mac on 14 or older gets no matching distribution.
  No notarisation and no signing identity: nothing pip delivers is quarantined. Every
  post-link rewrite of a shipped binary — `lipo -thin` on MoltenVK — is re-signed ad hoc
  (`codesign -f -s -`) in the step that rewrites it, and signatures are verified in the
  built zip and again after install. The same workflow runs as a dry run on every push to
  `main`, on a runner with no Vulkan SDK: it builds the runtime unit, checks the signatures of every binary in the lend
  and of `tatolabd` and `tatolab`, runs the portability, notices and driver-search tests
  against the lend, and in a venv holding only `tatolab-stream` runs `tatolab new` then
  `tatolab run` for twenty seconds, which must open the carried loader, raise nothing from the
  effect, and show the stream loaded with `TestPatternSource.video` writing at least sixty
  frames in `graph` — `H264Encoder` either encoding or, on a runner with no encoder, refusing
  by name while the stream stays loaded. On release a failed macOS proof of
  the runtime unit withholds the PyPI upload, as a failed build of any distribution does. A
  pull request builds no distribution; on `main`, `ci.yml` also builds the debug runtime
  unit on the same runner and runs the scaffolded stream on the lend's driver, the wheel's
  Python suite and the integration suite's GPU-free half on it.
  [macos-platform-floor — SHIPPED #2362; package-split-and-lend — SHIPPED #2693, #2697;
  moq-relays — SHIPPED #2845, #2866]
- **DECIDED** — The wheel is built, tested and linted on the macOS lane, not excluded from
  it. On every PR `Build (macOS)` compiles the whole workspace with the wheel in it
  (clippy on default targets, `check --all-targets`); on `main` it also runs the Rust
  suites and the wheel crate's unit tests, and `Integration test (macOS)` gates
  `_engine.pyi` with `stubtest` against the macOS binary — one stub for both floors — and
  runs the Python suite's GPU-free half. The test
  target's pipe is `std::io::pipe()`, and `check-no-inheritable-descriptor` accepts that
  portable spelling for its two pipe entries, with every Linux flag it accepts or refuses
  unchanged; on Darwin that pipe is `pipe` then `fcntl`, not atomic, and the spawn-side
  answer to the race is `posix_spawn` (#2368). `lint-logging` evaluates `cfg` for macOS
  too, so `apple/` is linted. A test absent on macOS carries
  `linux_only_capability(reason=…)`, held to the closed list §Product states. The Python
  suite's `requires_gpu` half does not run on the macOS lane — it runs on the macOS rig, as
  the Linux lane runs it on the bundled software device (§Product); the runner's paravirtual
  Metal device serves the in-process adapter tests below.
  [macos-capability-parity — SHIPPED #2400; optional-accelerators — SHIPPED #2778]
- **DECIDED** — The in-process adapters are per floor and say so. `streamlib-adapter-vulkan`,
  `streamlib-adapter-cpu-readback` and `streamlib-adapter-skia` build on MoltenVK, and the
  macOS lane runs their in-process tests on the runner's paravirtual Metal device, failing
  on any skip line but the validation-layer test's, because a fixture that cannot bring up
  its device prints a skip and passes. Each wraps an engine image over a private IOSurface
  from `GpuContext::acquire_render_target_iosurface_image`, the macOS peer of
  `acquire_render_target_dma_buf_image`, both built from one render-target usage set. The
  fd-based cross-process tests stay Linux, and cpu-readback's NV12 tests are ignored on
  macOS: the engine's IOSurface-backed image is single-plane by construction. Skia's GL arm
  stays Linux; on macOS Skia takes the prebuilt rust-skia publishes without `gl` and finds
  `vkGetInstanceProcAddr` through the consumer RHI's one loader search list.
  `streamlib-adapter-opengl` is a named absent tier on macOS — its seam is EGL and DMA-BUF,
  OpenGL is deprecated there, and a native macOS consumer takes the Vulkan adapter.
  `streamlib-adapter-cuda` is absent by nature. Each says so at its crate root, and the CUDA
  adapter's DLPack module builds on both floors as the workspace's one home for the ABI.
  [macos-capability-parity — SHIPPED #2415]
- **DECIDED** — streamlib ships under a `tatolab.*` namespace, as a pure-Python stream package
  and a native runtime package, with support for Linux and Apple Silicon macOS and no
  streamlib-owned service (sentence 5, its "host" read as the runtime — the per-user service that
  keeps the runtime alive is registered by the installer, never by a streamlib package). The stream
  package and packs go through the package index; the runtime package ships inside the installer
  with the CLI and the desktop app, never through pip (owner, 2026-09-30). Known: maturin ships a native
  portion of a PEP 420 namespace beside a pure one, in wheels and editable installs alike, so no
  custom module system is needed. [one-runtime-per-machine; package-split-and-lend — SHIPPED
  #2673, #2697; runtime-hosting — SHIPPED #2721, #2722]
- **DECIDED** — The Linux installer. Each release attaches
  `tatolab-runtime-<version>-x86_64-linux.tar.gz`, built in `manylinux_2_28` with the
  portability gate run over it and carrying its user units in `lib/systemd/user/`, and serves
  it, `install.sh` and a `latest` naming the version from `get.tatolab.com`, an R2 bucket.
  `curl -fsSL https://get.tatolab.com/install.sh | sh` checks the tarball's sha256, unpacks it
  to `~/.local/share/tatolab/<version>/`, points `current` at it, links `tatolab` and
  `tatolabd` into `~/.local/bin`, and writes `~/.config/systemd/user/tatolabd.service`
  (`Restart=on-failure`, `WantedBy=default.target`, `After=graphical-session.target`) — always
  on, and across logouts with `loginctl enable-linger`, which it offers. Re-running upgrades in
  place and restarts the service, a failed restart keeping the previous release; `--uninstall`
  removes the service and the files, never the state directory. On macOS the script refuses,
  pointing at the app. The tarball carries the Vulkan loader and the software device in
  `tatolab/runtime/_vulkan_driver/`, and `install.sh` refuses a tarball without the driver's
  manifest. Owner, 2026-10-11. [runtime-hosting — SHIPPED #2722; optional-accelerators —
  SHIPPED #2761]
- **DECIDED** — `Tatolab.app`, a menu-bar app carrying the runtime, lives in the private
  repository `tatolab/tatolab-app` — Tailscale's split: the runtime and CLI public, the app
  private — and is a Tauri app registering its agent through `objc2-service-management`. On
  macOS it ships as a notarised `.dmg` on this repository's release and as `brew install --cask
  tatolab/tap/tatolab` from `tatolab/homebrew-tap`, whose `Casks/tatolab.rb` the app
  repository bumps per release. The app repository checks this repository out at the release
  tag, builds the runtime unit, lays it into the bundle, signs, notarises, and attaches the
  `.dmg`. The bundle holds `Contents/MacOS/tatolabd`; an `SMAppService.agent` (plist in
  `Contents/Library/LaunchAgents/`, `KeepAlive` on a failed exit); the CLI as
  `Contents/MacOS/tatolab-cli`, a name that does not case-fold onto the app's own executable on a
  default APFS volume, which the `tatolab` command links to; and the lend laid out by
  Apple's code rules — every Mach-O under `Contents/Frameworks/tatolab-runtime/` at its path
  below `tatolab/runtime/`, the lend in `Contents/Resources/lend/` with each Mach-O a relative
  link into it — which `tatolabd` finds from `Contents/MacOS/`, `dladdr` finding
  `_vulkan_driver/` through the link. Registration is the login item, so one switch, "Run
  Tatolab", covers both; opening the app registers the agent unless the user turned the switch
  off, which it remembers, and on `.requiresApproval` opens Login Items. The menu shows whether
  the runtime is up and its stream count (`list_streams`); Quit quits the app, never the
  runtime. First launch asks for no administrator password. It links `tatolab` into a folder
  in the user's home directory, and offers a link in `/usr/local/bin`, behind an
  administrator prompt, as an option. `Info.plist` carries the camera, microphone,
  local-network and Documents, Desktop and Downloads usage strings; the app and `tatolabd`
  carry the hardened-runtime device entitlements; every Mach-O is signed with the Developer ID
  and the `.dmg` notarised and stapled, the seven `APPLE_*` secrets synced from Infisical into
  the app repository. Uninstalling — the app's "Uninstall Tatolab…" or `brew uninstall --cask` —
  removes the service and the files, never the state directory. On Linux the app is a `.deb`
  and an AppImage from the same repository: the same window and tray over the service the
  installer registers, its switch `systemctl --user enable --now` / `disable --now
  tatolabd.service`; it installs no runtime, so the installer stays Linux's one way to install
  the runtime, and with no service the app points at the installer. Beyond its live stream
  list, graph and logs, the app's stream actions and the rest of its GUI are the app step's.
  Owner, 2026-10-02, 2026-10-11 and 2026-10-24. [runtime-hosting — SHIPPED #2721;
  machine-lock-without-administrator — SHIPPED #2929]

## Control plane & observability — IN-FLIGHT (→ runtime-description, relay-setting-fields)

- **DECIDED** — The control plane carries no optional capability's routes, and no package
  adds one: nothing a package carries runs in the runtime process (§Packages & extension
  model). `graph` carries no `extensions` key.
  [extension-model; networking-extension-wheels — SHIPPED #2149, #2153; package-split-and-lend
  — SHIPPED #2691]

- **DECIDED** — One control plane: the api-server's HTTP + WebSocket + MCP surface,
  hosted in-process by the engine itself. The MCP tool set is the canonical
  control vocabulary; the CLI is a pure JSON-RPC client of it — agents and humans use
  the same verbs; REST/WS routes serve the same operations for programmatic clients.
  The served MCP tool set is exactly the observation verbs `graph`, `logs` and
  `describe_runtime` (the runtime's version, build id, monotonic uptime and its own log's
  folder), the four graph-mutation verbs — `add_node`, `remove_node`, `connect` and
  `disconnect` — the six stream actions — `run_stream`, `stop_stream`, `start_stream`,
  `remove_stream`, `list_streams` and `expose_port` (§Product) — and the four relay verbs —
  `add_relay`, `configure_relay`, `remove_relay` and `list_relays` (§Networking, the
  adapters); `health` is a REST route, not a tool. The MCP handshake's server version is the
  runtime's crate version. The runtime has no shutdown verb: it stays on as a service does, stopped by
  the service manager or a signal, and `stop_stream` ends a client's work. Rejected: a control plane that never mutates the graph, code being the
  source of truth and the edit loop `dev` — a dynamic graph an agent cannot add to is not
  worth having (owner, 2026-09-06). A mutation compiles inside the call, so the caller learns whether
  its change took rather than reading a `Running` node with nothing flowing. A Python
  class is named by its import path — described in the stream's interpreter at this first
  add, its descriptor and constructor registered exactly as a load registers them; the
  runtime process never imports the class, and the processor runs in its own processor
  interpreter — and a native built-in by the path `graph` reports for one;
  a link wired onto a running processor reaches it, and every channel is sized for a
  destination that connects later. MCP is
  served by the node's local API, mounted with the node and sharing its lifecycle, through
  `rmcp`, the official Rust MCP SDK — no MCP protocol logic is hand-written, in the runtime
  or the CLI's client, which is `rmcp`'s client — in two framings over one handler:
  `POST /mcp`, `rmcp`'s Streamable HTTP service with its header checks and statuses, for
  the CLI's one-shot calls; and `/mcp/stdio`, an HTTP/1.1 `Upgrade: mcp-stdio` after which
  the connection carries MCP's stdio framing both ways, for the `mcp` verb. Beside its
  tools the node serves three resources, each rendered at the moment it is read:
  `streamlib://node-catalog`, every node type it can add with its description, derived
  config schema and ports — `/api/registry`'s document; `streamlib://graph`, the `graph`
  tool's; and `streamlib://relay-adapter-catalog`, each relay adapter's kind, the name and
  icon people see it by — the icon is the provider's own logo on a square canvas, a `data:` URI the
  adapter carries and never fetches — its doc and its settings schema, its secrets marked `writeOnly` —
  `/api/relay-adapters`' document. It also serves
  three prompts, recipes rendered against the live graph and the catalog —
  `insert_node_between_linked_nodes`, `fan_output_to_another_consumer` and
  `show_channel_on_virtual_camera` — whose every step is a call to a served tool: a prompt is
  text, never a mutation path, so the tool
  set stays the whole of the control vocabulary.
  [importable-python-library; mcp-served-with-the-node — SHIPPED #1712;
  agent-readable-processor-catalog — SHIPPED #2232; local-transport-hardening —
  SHIPPED #2263, #2265; local-api — SHIPPED #2660, #2665, #2667; package-split-and-lend —
  SHIPPED #2689; runtime-hosting — SHIPPED #2704, #2711; moq-relays — SHIPPED #2871;
  runtime-description]
- **DECIDED** — The local API speaks the graph's words: a tool's argument is spelled as `graph`
  renders the same thing, and no tool, result, resource or prompt says "processor" or "display
  name". `graph` renders a node's `name`, and each link end as `{node, port}`, with no
  `processor_id` beside it. The graph-mutation tools are `add_node(type, config, name)`,
  answering the name the node received; `remove_node(name)`; `connect`, each end given as
  `<end>_node` and `<end>_port`, answering a `link_id`; and `disconnect`, taking a `link_id`.
  Every tool addresses a node by its name within its stream, never by an id; `graph` still
  renders a node's `id` as a live key. `add_node`, `remove_node`, `connect`, `disconnect` and
  `logs` take a required `stream`; `graph {stream?}` returns one stream's one-shape graph,
  loadable, or `{machine, streams: […]}`. Both ends of a `connect` are ports in the one
  stream it names (§Networking, a link's ends); the catalog
  resource is `node-catalog`, listing `nodes`, each under the `type` `add_node` takes; the
  `graph` resource, the instructions and the prompts say node and stream —
  `insert_node_between_linked_nodes` among them. The engine's Rust identifiers keep
  "processor" until the rename. Owner, 2026-10-02. [local-api; stream-graph — SHIPPED #2614;
  zenoh-and-moq-wheel-removal — SHIPPED #2643, #2645; runtime-hosting — SHIPPED #2704, #2711,
  #2715]
- **DECIDED** — `graph` carries the machine's name as a top-level key, `machine`, beside a
  stream's `stream`, `nodes`, `links` and `exposed` — in the OpenAPI schema, the MCP tool, the
  generated schema and the prompt fixture — and no mesh key. Every channel reserves one
  subscriber slot beyond its fixed cap, the exposed-port publisher's, and no other.
  [zenoh-and-moq-wheel-removal — SHIPPED #2643, #2645; package-split-and-lend — SHIPPED
  #2691, #2697; runtime-hosting — SHIPPED #2715; moq-relays — SHIPPED #2853]
- **DECIDED** — The api-server is engine-side infrastructure in the `runtime/` tree: it is
  hosted — statically linked, never dlopen'd — by `tatolabd` (and by the `streamlib` crate
  for Rust apps), and no wheel hosts it. [control-plane-one-surface; package-split-and-lend —
  SHIPPED #2693]
- **DECIDED** — The CLI is the native `tatolab`, shipped with the runtime and never in a
  pip distribution, linking no engine: `new`; the stream verbs `run`, `dev`, `stop`,
  `start`, `rm`, `streams` and `expose`, each a call into the running runtime; the
  observation verbs `graph` and `logs`; the relay verbs `relays` and `relay add`, `relay set`
  and `relay remove`, a secret given only from a file or standard input; the `mcp` verb; and
  one machine-setup verb, `enable-virtual-camera`, which installs the
  loopback permission the virtual camera's loopback door needs behind the desktop's
  password prompt and touches no node. `--version` prints the CLI's version and, when a
  runtime answers, the runtime's description. The CLI stays a pure client of the local API; the
  control plane exists to observe and drive *running* nodes, not to embed.
  [importable-python-library — SHIPPED #1683, #1711; importable-python-library-ripout
  — SHIPPED #1715; virtual-camera-sink — SHIPPED #2196; local-api — SHIPPED #2667;
  tatolab-names; package-split-and-lend — SHIPPED #2693, #2697; runtime-hosting — SHIPPED
  #2711, #2721, #2722; moq-relays — SHIPPED #2871; runtime-description]
- **DECIDED** — One engine-resolved runtime directory holds everything a live runtime puts
  on disk that means nothing once its processes are gone: the surface-sharing socket, the
  local API socket and the iceoryx2 domain. On Linux it is
  `$XDG_RUNTIME_DIR/streamlib/` when that variable is set and non-empty, and otherwise —
  empty or unset, and on macOS always — `/tmp/streamlib-<uid>/`, created owner-only and
  checked as a real directory this uid owns with no group or other bits. The check runs once
  as the runtime starts, before its first socket write, and a failure
  refuses the start by name; every user takes the resolved directory. No StreamLib variable
  overrides it — a container or CI job sets `XDG_RUNTIME_DIR` — so a runtime starts anywhere
  with nothing set, and `tatolab` resolves it through `streamlib-runtime-client-contract`, the
  code the runtime resolves it with. What a runtime *keeps* sits elsewhere: a stream's logs
  and caches under its project's `.streamlib/`, and the kept streams and the runtime's own
  log in the state directory (§Product). There is no registry to discover a runtime by: one
  runtime per machine serves one socket at a fixed path. Owner, 2026-09-14.
  [control-plane-one-surface; local-transport-hardening — SHIPPED #2261; local-api — SHIPPED
  #2660, #2663; package-split-and-lend — SHIPPED #2697; runtime-hosting — SHIPPED #2711]
- **DECIDED** — Observability: the JSONL log schema is a durable contract; graph and health
  inspection ride the same control plane. [control-plane-one-surface]
- **DECIDED** — There is no observer effect: reading a port requires neither terminating it in
  a window nor adding a node to the graph — an exposed port is read through a relay in the
  topology that ships. [control-plane-surface-pixel-exchange; moq-relays — SHIPPED #2853,
  #2860]
- **DECIDED** — Auth and remote-access posture: whoever can open the local API's socket may
  call it — the owning user, by file permission — and nothing off the machine can call it at
  all, so control carries no token and no account. What a runtime offers other machines is
  exposure, decided in §Networking. Owner, 2026-10-01.
  [control-plane-bind-posture; local-api — SHIPPED #2663]
- **DECIDED** — The local API is reachable only on its own machine. Each machine's runtime
  serves one local API on a socket in its runtime directory that only the owning user can
  open — `local-api.sock`, chmod 0600 inside the 0700 directory, a live holder refused by a
  connect probe naming the path and a stale file replaced — carrying the router and control
  vocabulary unchanged; no network address serves control, and a client dials the one socket.
  The runtime's HTTP is a separate listener that changes nothing — on loopback only, serving
  the listing, `/watch` by address and the lock holder's pid (§Networking) — because a
  browser cannot dial a socket. A runtime is never
  driven from another machine
  through its local API: changing a stream on another machine means running the CLI or an
  agent on that machine, over ssh for example, and a fleet-wide path is the external control
  client's. Owner, 2026-10-01. [one-runtime-per-machine; local-api — SHIPPED
  #2660, #2662, #2663; runtime-hosting — SHIPPED #2711; moq-relays — SHIPPED #2860, #2864]
- **DECIDED** — An MCP host reaches the local API by launching the CLI's `mcp` verb as a
  stdio server — `claude mcp add streamlib -- <cli> mcp`. The verb resolves the runtime,
  opens its socket, sends the one `/mcp/stdio` upgrade request, and then copies bytes,
  stdin → socket and socket → stdout; it parses no message, so the tools, resources, prompts
  and protocol revision stay the runtime's alone. Stdin closing half-closes the socket and
  the verb exits when the runtime closes its side; the runtime going away exits it non-zero
  with one stderr line naming the runtime; no live runtime at launch is a one-line refusal
  naming the socket and how to start the runtime. Run over ssh — `ssh <machine> <cli> mcp` — it is how an agent
  changes a stream on another machine. No network listener serves MCP. `/mcp/stdio` serves
  every MCP revision `rmcp` speaks and prefers the latest, 2026-07-28 (stateless: no
  `initialize` handshake and no session): a host at the latest revision discovers it and
  picks it, and a host that still opens with `initialize` — Claude Desktop among them — is
  answered at the revision it named, or at the newest revision with a handshake when it
  named one the runtime does not know. `POST /mcp` serves the latest revision alone, since
  it demands the per-request metadata no earlier revision carries; a request at an earlier
  revision is refused with the protocol's own unsupported-version error naming it.
  `subscriptions/listen` is acknowledged with an empty set — `listChanged` stays `false` —
  and held until cancelled or the local API stops. Agents hosted in a cloud, which can
  neither launch a command nor reach a machine's loopback, are not served by the local API.
  Owner, 2026-10-01; `rmcp`, owner, 2026-10-06; earlier revisions over `/mcp/stdio`, owner,
  2026-10-27. [local-api — SHIPPED #2665, #2667]
- **DECIDED** — `graph` returns every stream the runtime holds, and every stream action the CLI
  has — `run` attached, `run -d`, `stop`, `start`, `rm`, `streams`, `expose` — is also a tool,
  beside the graph-mutation tools, which stay, so an agent can do whatever the CLI can. One
  runtime per machine owned by one user (§Product) means one socket per machine. Owner,
  2026-10-01; confirmed 2026-10-02. [local-api; runtime-hosting — SHIPPED #2711]

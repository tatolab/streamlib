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

Reading rule since the 2026-10-04 pivot (`[moq-on-the-tailnet]`): the `tap` and `exchange` verbs
go at the sharing step. An entry describing `tap` or `exchange` is a
fact about the shipped tree until the change that removes them ships and folds it out — never
direction, and nothing new is built on it. Off a machine, the direction is §Networking's
`[moq-on-the-tailnet]` entries.

## Product (the MVP sentence) — IN-FLIGHT (→ stream-graph, package-split-and-lend, runtime-hosting, authoring-names)
<!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_launch.py -->

- **DECIDED** — A Python developer on Linux with an NVIDIA GPU, or on Apple Silicon,
  pip-installs streamlib (initially from this repo's releases; PyPI after the project
  rename) into an ordinary uv-managed venv, runs `streamlib new` then `streamlib dev`,
  sees their camera live in a window within a minute, and makes the pipeline theirs by
  editing the scaffolded processor — zero ceremony: no manifest, no `main()`, no schema
  wrangling, a fast edit loop. The zero-ceremony clauses bind both floors alike. Apple
  Silicon is a supported floor, not a developer machine: CI gates it on every PR, its
  `aarch64-apple-darwin` wheel is in the release closure, and a macOS-only regression
  blocks a release as a Linux one does. macOS security prompts are part of the
  experience and do not breach zero ceremony; needing an app bundle to obtain them
  would. Every ticket traces to this sentence or does not exist.
  [importable-python-library — SHIPPED #1683, #1684, #1711; macos-platform-floor —
  SHIPPED #2357, #2359, #2361, #2362; amended by one-runtime-per-machine: the package names, the stream vocabulary, "with an NVIDIA GPU" (accelerators are optional), and the install step — the runtime arrives from an installer that registers it as a per-user service, so the loop is install, then `new`, then `run`]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli.py::test_new_writes_a_working_app -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_launch.py::test_the_scaffolded_app_reaches_a_running_graph -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_launch.py::test_every_helper_interpreter_goes_live_inside_the_startup_budget -->
  <!-- verify: grep -n "The scaffolded app runs on the driver the wheel carries" .github/workflows/macos-wheel.yml -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py::test_every_mach_o_the_wheel_carries_is_portable -->
- **DECIDED** — Terms of the sentence: StreamLib is an importable Python library — one
  PyPI wheel carrying the Python API, the CLI, and the Rust engine (PyO3, the
  pydantic-core model); a StreamLib app is a normal Python codebase — one venv, one
  Python version, ordinary PyPI dependencies, nothing dynamically downloaded;
  `dev`/`run` load the sole `@stream` in `stream.py` by convention — `run <file>.py:<fn>`
  or `run <module>:<fn>` loads one, `-f <file>` overrides the file, `--name` the stream's
  name — and a directory holding an `app.py` but no `stream.py` is refused naming
  `stream.py` and `-f`; nodes are
  Python classes written in the project or imported from pip-installed packages, and
  `stream.add` takes the class; the builder's API is `add`/`connect`/`expose`.
  [importable-python-library — SHIPPED #1683, #1707, #1708; stream-graph — SHIPPED #2567, #2569; amended by authoring-names: the builder is `StreamBuilder`, held as `stream_builder`; amended by one-runtime-per-machine: a stream package and a runtime package; `@stream` functions over a `Stream` builder, `setup` retired; `@node` — stream-graph builds the authoring clauses]
  <!-- verify: pytest sdk/tatolab-stream/tests/test_stream_graph_builder.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli.py::test_a_directory_holding_only_an_app_py_is_refused_naming_stream_py_and_the_file_flag -->
- **DECIDED** — The zero-ceremony bar (the sentence is untrue until all hold): no
  manifest authoring; no boilerplate entry; bags/schemas fixed (no engine schema
  matching, cast-at-read, no versions at the code layer); scaffolding for app and
  processor; the scaffold pins `.python-version` (3.12) and the wheel supports a small
  Python version range. [importable-python-library — SHIPPED #1684, #1711; schema-free-ports
  — SHIPPED #1814]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli.py::test_each_scaffolded_processor_lives_outside_the_entry_file -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_launch.py::test_the_edit_loop_survives_a_bad_save_and_shows_a_good_one -->
- **DECIDED** — Rust authoring stays a supported capability: a Rust app is a plain
  cargo project depending on the `streamlib` crate — no wrapper generation, no special
  format; third-party Rust processors for Rust apps are ordinary cargo dependencies,
  source-compiled. [importable-python-library — SHIPPED #1715]
- **DECIDED** — The two floors are one product surface. A Python processor written
  against the wheel's public surface runs on both; where it cannot is a short closed
  list that refuses by name before a frame flows — at load, or in `setup()`,
  naming the platform — never mid-frame: ray-tracing kernels (MoltenVK has no
  `VK_KHR_ray_tracing_pipeline`; each constructor refuses at `setup()` naming the absent
  tier), `VirtualCameraSink` (refused at load), the CUDA Array Interface, and the
  fd-shaped raw handles (`export_dma_buf`, `export_opaque_fd`, `import_dma_buf` exist on
  macOS and refuse pointing at `export_iosurface`). The scaffold and the examples use
  only the portable surface. The guarantee is mechanical, never prose: one
  `_engine.pyi`, gated by `stubtest` against both binaries in CI, so no class or method
  exists on one floor and not the other; one Python suite runs on both floors — its
  GPU-free half on both CI lanes, its `requires_gpu` half on each floor's rig; and a
  test skipped off Linux carries `linux_only_capability(reason=…)`, whose reason
  `test_platform_markers.py` holds to the closed list plus one named group — a test
  whose body is itself a Linux mechanism (`XDG_RUNTIME_DIR`, v4l2loopback and udev, the
  boot-session file, X11/Wayland, SIGHUP and SIGINT hand-back). A test red on macOS for
  any other reason is a parity bug, never a skip.
  [macos-capability-parity — SHIPPED #2400, #2403, #2405]
  <!-- verify: pytest sdk/tatolab-stream/tests/test_platform_markers.py -->
  <!-- verify: grep -n "mypy.stubtest tatolab.runtime._engine" .github/workflows/test.yml .github/workflows/python-wheel.yml -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_ray_tracing_tier_refusal.py::test_every_ray_tracing_constructor_refuses_at_setup_naming_the_absent_tier -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_helper_process.py::test_an_fd_shaped_raw_handle_refuses_by_name_off_linux -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_each_raw_handle_flavour_refuses_by_name_off_its_platform -->
- **DECIDED** — The scaffold models the pathway: pixels on the GPU, logic on the CPU, the
  pixel view explicit. `streamlib new` writes two processors, each in its own module
  under `nodes/` — an `InvertingEffect` over `GlslPixelEffect` (one GLSL `effect`
  function) in the camera-to-window path, and a numpy `BrightnessMeter` on a fan-out of
  the effect's output that reads the frame through `frame.cpu()` and logs its mean once a
  second, paced on `ctx.time` — with dependencies `streamlib` and `numpy>=2.1`, nothing
  more, the same on both floors. The files render from template files the wheel ships
  (`tatolab/runtime/_scaffold_template/`), each placeholder its template's own default value so
  the templates stay importable and checkable; ruff runs over every render, pyright over
  the template tree, and the cross-floor check gates the output.
  [engine-steps-for-effects-and-model-input; engine-steps — SHIPPED #2434, #2438; amended by stream-graph: the entry file and the nodes' directory re-spelled]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli.py::test_the_scaffold_models_pixels_on_the_gpu_and_logic_on_the_cpu -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli.py::test_the_scaffold_depends_on_streamlib_and_numpy_only -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli.py::test_new_writes_exactly_the_rendered_templates -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli.py::test_every_scaffolded_python_file_passes_ruff -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cross_floor_check.py::test_the_scaffold_binds_to_no_floor -->
- **DECIDED** — A stream is the unit a person writes and runs: today's app — a directory whose
  `stream.py` defines one or more streams, each a function decorated `@stream` that builds that
  stream's graph of nodes and links from the classes it imports (`@node`), plus `pyproject.toml`
  and one venv; a stream may expose ports. Each machine runs one runtime, which runs many streams — written by different people
  or teams — owns the accelerator when one is
  present, serves the machine's local API, and schedules across streams. Every Python node
  keeps its own process, and agents keep changing live graphs. Owner, 2026-09-30.
  [one-runtime-per-machine; stream-graph; amended by moq-on-the-tailnet: the runtime serves
  the machine's MoQ endpoint, built at the sharing step]
- **DECIDED** — What Tatolab is for: a simple SDK for defining live streams as pipes and
  sharing them with people and agents. Tailscale users are the first customers, and Tatolab
  builds nothing Tailscale already does. Robotics and physical-machine systems are out of
  scope, and Tatolab never replaces a team's own ROS or Zenoh stack. Owner, 2026-10-04.
  [moq-on-the-tailnet]
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
  `run` against a running runtime connects as usual; until the app ships, `tatolabd` runs in a
  terminal there. On Linux the installer — `curl | sh` or a distro package — registers
  `tatolabd` as a systemd user service, the service being the end state on Linux, not a stand-in
  for the app. Owner, 2026-10-02. [runtime-hosting]
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
  (runtime-hosting decision 5). `tatolab streams` lists all four — attached, kept,
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
  work, outside this pivot. Owner, 2026-10-01. [runtime-hosting; one-runtime-per-machine]
- **DECIDED** — Several streams in one project or package: a stream is a decorated function,
  `@stream def camera_rig(stream: Stream)`, and a file or a package may define as many as it
  likes; the bare `setup(stream)` retires rather than living beside it, and a package may ship
  both runnable streams and composable nodes (owner, 2026-09-30). Decided as stream-graph
  decision 1 (owner, 2026-10-01): the name
  defaults to the function's and its docstring is the description an agent reads; `run` with no argument runs the sole `@stream` in `stream.py` and refuses by name when
  there are several; `run stream.py:camera_rig` or `run acme_rover:camera_rig` runs one. A
  package declaring its streams under an entry-point group is the packs OPEN's.
  [one-runtime-per-machine; stream-graph — SHIPPED #2567; amended by authoring-names:
  `@stream def camera_rig(stream_builder: StreamBuilder)`]
- **DECIDED** — Composition inside a stream is plain Python: a function that takes the builder,
  adds nodes, connects them and returns port references is a reusable fragment. The graph
  stays flat and addresses stay `<machine>/<stream>/<node>/<port>`; no group label and no nested
  subgraph is built until an editor or an agent needs to see a fragment as one thing. Across
  streams, linking to another stream's port is the composition. Owner, 2026-10-01.
  [runtime-hosting; one-runtime-per-machine]

## Packages & extension model — IN-FLIGHT (→ package-split-and-lend, authoring-names)

- **DECIDED** — PyPI and cargo are the package systems. The custom module system is
  deleted in full: `streamlib_modules/`, the `.slpkg` format, `streamlib.lock`, the
  package source, the `add`/`install`/`link`/`pkg` verbs, `BuildOrchestrator` and all
  runtime downloading or compiling. Compilation happens at publish time, by the
  author, with standard tools (maturin/CI for wheels, cargo for crates) — StreamLib
  never compiles user code.
  [importable-python-library; importable-python-library-ripout — SHIPPED #1715;
  schema-free-ports — SHIPPED #1813; processor-class-identity — SHIPPED #1837, #1841]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-10-importable-python-library-ripout.md -->
- **DECIDED** — The plugin ABI is deleted: no dlopen'd processor cdylibs, no `repr(C)`
  vtable surface, and none of that ABI's load-time machinery — no dlopen load handshake, no
  cdylib build fingerprints. The scope is the deleted ABI and nothing wider: a helper process
  imports the one wheel and checks that the engine it imported is its parent's build, which
  is the handshake §Processor model states, not an ABI surface returning. The extension paths are
  Python packages and Rust source crates only — and an extension wheel is a Python
  package: Rust inside, loaded across the CPython ABI, never dlopen'd by the engine
  (extension-model, 2026-09-04).
  [importable-python-library; importable-python-library-ripout — SHIPPED #1715;
  local-transport-hardening — SHIPPED #2262; amended by package-split-and-lend: a processor
  interpreter imports the lent `tatolab.runtime` and checks it is its parent's build, true by
  construction and kept as the backstop]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-10-importable-python-library-ripout.md -->
- **DECIDED** — Third-party native code (closed-source included) ships as an ordinary
  Python package whose native internals expose capabilities to Python as handles —
  frames, FDs, exportable device allocations, buffers — wrapped by a Python
  processor. It never links the engine and never speaks streamlib internals; the
  CPython ABI is the only
  binary boundary, and no process ever holds two streamlib engines — the app process
  runs the one engine, and a helper process imports the same wheel as a processor
  host, never as a second engine. Handles it exposes must be genuinely transferable
  across a process boundary (an fd, an exportable allocation) — an
  address-space-local pointer is not a handle.
  [importable-python-library — SHIPPED #1710, #1756, #1757; amended by one-runtime-per-machine: the runtime process runs the one engine, one per machine]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py -->
- **DECIDED** — First-party optional capabilities ship the same way third-party native
  code does: as separate PyPI extension wheels — Rust inside for speed, a Python
  processor as the binding for any processor the wheel supplies — depending on the
  `streamlib` wheel as a binary and never building it from source. Optional means an app can be complete without it. The engine
  is not the home of every capability; it is the home of what belongs in core. Owner,
  2026-09-04. [extension-model]
- **DECIDED** — Two extension mechanisms, recorded as the current best understanding of
  the shape and expected to flex during the align and implementation. A *processor
  extension* is a Python processor class in a pip-installed package whose per-frame work
  runs in native code the same wheel carries: `stream.add(TheClass)` adds it, as
  for any Python processor; it runs in its own helper process under the one placement
  rule; and it calls its own package's Rust directly — the engine does not call extension
  code on the data path, and there is no processor-to-engine-to-wheel round trip. A
  *capability extension* is support code: declared by a standard entry point in the
  wheel's `pyproject.toml` that pip records at install and the engine reads through
  `importlib.metadata` at startup — pip's registry, not a file scan — and run once, the
  way a driver is loaded, so that the processors in the same wheel find what they need
  already in place. It may bring up a device library or a network stack, and it may
  introduce an engine-grade capability the engine does not itself provide — specialised
  graphics processing, a transport, a device class — the Unreal-module shape. It registers
  through a sandboxed door the engine offers, so two packages cannot unsafely alter engine
  features, and it extends rather than rewrites engine pieces. Pure Python stays a
  complete way to write a processor; this is an additional pathway. Both compile at
  publish time with maturin, neither is dlopen'd by the engine, and the CPython ABI stays
  the only binary boundary. [extension-model]
- **DECIDED** — The criterion for a built-in, stated so that the next one is
  contestable: a first-party capability ships inside the wheel only if (a) its per-frame
  path has a deadline the helper hop cannot meet — a vsync-paced present loop, a device
  audio callback — or (b) it needs an engine-only primitive the handle-shaped surface does
  not export, or (c) it presents an OS-facing device to the other applications on the
  machine — a virtual camera; a virtual microphone would be the same case — which
  `pip install streamlib` alone must make available, with no further package to install;
  and in every case (d) a named consumer exists. Everything else is an
  extension. What an extension needs and the engine does not yet expose is engine work,
  done as engine code inside the extension's own change, rather than by the extension
  reaching past the surface. Codec sessions are not exported to Python; an extension that
  needs one brings that export as engine work. [extension-model; virtual-camera-sink —
  SHIPPED #2196, #2197, #2198]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_virtual_camera_sink.py -->
- **DECIDED** — The `streamlib` wheel exports its bag codec as two module-level functions
  with stub entries: `encode_bag_to_msgpack_bytes(bag: Mapping[str, Any]) -> bytes` and
  `decode_msgpack_bytes_to_python_object(msgpack_bytes: bytes) -> Any`. They are the
  existing `encode_bag_to_msgpack` and `decode_msgpack_to_python_object` made reachable,
  with exactly the codec's rules — a dict with string keys at every level, the eight value
  types, `bytes` as `bin` at 1×, refusal by name of anything else — and no new behavior.
  The codec's rules include one nesting bound: a value nesting more than 128 containers is
  refused by name, on encode and decode alike.
  This is the first firing of the clause above that engine work an extension needs is done
  as engine code inside the extension's own change: an extension carrying a bag across its
  own transport needs the one codec, and a second one in the wheel would be the parallel
  abstraction the doctrine forbids. It is not a raw byte port — no link reads or writes
  bytes; the pair converts between a bag and bytes in the caller's own hands.
  `docs/decisions/extension-model.md` records why. [moq-data-tracks — SHIPPED #2171]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_bag_codec_export.py -->
- **DECIDED** — The capability-extension mechanism, decided on the first real extension
  and expected to move where implementation teaches otherwise. The entry-point group is
  `streamlib.extensions`; an entry names one callable the wheel exports, `load(host)`.
  The engine runs every installed hook once per process that takes an engine role: in
  the app process when `Runtime()` is constructed, and in each helper after the wheel is
  imported and the log channel is up but before the processor's module is imported — so
  a failing hook is reportable through the normal channel, and a stack the hook brings
  up exists in the process where `process()` runs. `host` is a small bounded object: it
  says which role the process has, and it takes `register_capability(name, version)` — a
  registry the wheel owns, because native processor registration is reachable only from
  Rust that links the engine, which an extension by construction does not. Doors on
  `host` grow only when an extension needs one, as engine code inside that extension's
  change. A hook that raises fails the runtime's construction in the app process and
  fails that processor's start by name in a helper — the posture the engine's own init
  hooks already take — rather than skipping and logging, since an extension that half
  loaded is worse than one that refused. Two wheels registering one capability name
  refuse by name at startup. `graph` carries what loaded, as a third top-level key beside
  `nodes` and `links`: one entry per capability with its name, version and distribution.
  There is no per-app opt-out yet; the first app that needs one gets it as a one-line
  addition. [extension-model; reopened by one-runtime-per-machine: how an external control client loads]
- **DECIDED** — The support hook's contract, as built. A wheel declares
  `[project.entry-points."streamlib.extensions"] <name> = "<module>:load"`; the engine
  reads `importlib.metadata.entry_points(group="streamlib.extensions")` and calls each
  `load(host)` once per process taking an engine role — from `Runtime.__init__` in the app
  process, and from `_helper.py` between the log sink's installation and the processor
  class's import. `host` is `tatolab.runtime.CapabilityExtensionHost`, a `#[pyclass]` with a stub
  entry: `role` (`"app"` or `"helper"`) and `register_capability(name, version)`. In the
  app process a registration lands on the runtime and renders in `graph`; in a helper it is
  recorded for the extension's own reads. A hook that raises fails `Runtime()` with the
  distribution named; in a helper it fails that processor's start through the log channel
  and the parent refuses the processor by name, inside the existing 60 s budget. A second
  registration of one capability name refuses at the second hook, naming both
  distributions. `GraphResponse` gains `extensions: [{name, version, distribution}]`, a
  third top-level key, in the OpenAPI schema and the MCP `graph` tool alike. No opt-out.
  Discovery and the loop are Python; the runtime-side registry and the `graph` key are the
  one engine change. [networking-extension-wheels — SHIPPED #2149; reopened by one-runtime-per-machine: how an external control client loads]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_capability_extensions.py -->
- **DECIDED** — The mechanism's own proof is GPU-free and CI-run: a test-only distribution
  under the wheel's tests, installed into the venv, whose entry point registers a capability
  and whose second variant raises — proving discovery, the app-process and helper call
  sites, hard-fail by name, duplicate refusal, and the `graph` key, with no network and no
  device. [networking-extension-wheels — SHIPPED #2149]
- **DECIDED** — An extension wheel is built the way a third party would build one, which
  is the dogfooding the pivot exists for: a standalone maturin project under `packages/`
  with its own workspace root and lockfile — not a member of the engine workspace —
  depending on the published `streamlib` wheel by version and on `pyo3`, and on no engine
  crate; independently versioned and released; published through the same simple index
  the wheel uses, which becomes multi-project to carry it. Distribution names take the
  `streamlib-<capability>` form and imports `streamlib_<capability>`. Its gates are its
  own CI lane — stubtest over its own `.pyi`, pyright, the portability gate — since the
  engine workspace's gates do not walk a non-member. A Rust-side extension SDK is not
  owed by the first two extensions, whose Rust handles bytes and no engine object; it
  lands with the first extension that needs one. [extension-model; amended by tatolab-names:
  `tatolab-<name>` importing as `tatolab.<name>`; amended by package-split-and-lend: an
  extension depends on `tatolab-stream`, and its nodes run in processor interpreters where
  `tatolab.runtime` is lent; while released from this repository an extension carries
  Tatolab's one version number (§Distribution & versioning)]
- **OPEN** — How an engine-grade capability an extension introduces — a specialised
  graphics pass, a device class — is reached by processors and by the engine. Undecided
  until an extension brings one: the first two register a name and bring up a network
  stack, which is all the mechanism has to carry so far. [extension-model]
- **OPEN** — Whether an extension's native code may ever be called in the app process
  rather than in its helper — a Rust-implemented class reached through the CPython API
  with the GIL released on entry. The placement rule stands unchanged: every Python
  processor, extension or not, runs in its own helper process. This is the owner's ruling
  to make and never a session's inference; until it is made there is no carve-out.
  [extension-model]
- **DECIDED** — The engine's handle-shaped primitive surface is the public contract
  for native interop: DMA-BUF / OPAQUE_FD import and export on Linux and IOSurface export
  on macOS, the present target, texture rings, codec byte pumps, the audio clock, color
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
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_pixel_exchange.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_a_graph_frame_reaches_torch_as_a_device_tensor -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_mlx_reads_a_graph_frame_over_its_own_bytes -->
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
  counts), dedicated-allocation status, the exporter's memory type index, and the
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
  <!-- verify: cargo test -p streamlib-adapter-cuda --test opaque_fd_wheel_export_foreign_consumer a_wheel_exported_opaque_fd_read_by_a_foreign_process_shows_the_kernels_pixels -->
  <!-- verify: cargo test -p streamlib-adapter-cuda --test opaque_fd_image_consumer_rhi_round_trip an_exported_opaque_fd_pins_the_payload_past_source_texture_teardown -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_a_texture_handle_round_trips_across_the_process_boundary -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_an_iosurface_port_is_looked_up_and_read_by_native_code_in_the_helper -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_each_iosurface_export_is_a_fresh_send_right_the_caller_owns_and_gives_back -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_each_raw_handle_flavour_refuses_by_name_off_its_platform -->
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
  that reads the bag as a dict. Publish-to-claim transit rides pool depth, and so
  does an untyped read: the strictness dial is also the safety dial — depth bounds
  the window, and outwaiting it is an error, never somebody else's pixels. The
  engine inspects no bag content anywhere. The producer never waits on a consumer:
  the pool skips leased slots and grows to its cap; at cap the producer drops its
  own frame — a slow consumer costs memory, then its own frames, never another
  processor's cadence. A producer-internal transient (a frames-in-flight ring
  texture) never backs a cross-process export: the export blit sources the
  surface's pooled backing whenever one exists, read-only; texture-backed export
  remains for surfaces with no pooled backing (kernel outputs).
  [surface-id-lifetime-contract — SHIPPED #1868, #1869, #1871, #1877]
  <!-- verify: cargo test -p streamlib-engine a_checkout_of_a_retired_frame_id_is_refused_naming_the_recycling -->
  <!-- verify: cargo test -p streamlib-python-wheel claiming_a_recycled_frame_is_refused_naming_the_recycling -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-16-surface-id-lifetime-contract.md -->
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
  `surface_id` and never guessed: the wheel inspects bag content no more than the engine
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
  <!-- verify: pytest sdk/tatolab-stream/tests/test_claimed_surface_pixel_access.py::test_the_bare_object_hands_back_the_surfaces_own_capsule -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_claimed_surface_pixel_access.py::test_a_two_surface_type_is_refused_every_bare_door_naming_the_surfaces -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_claimed_surface_pixel_access.py::test_a_frame_that_cannot_take_a_write_back_arrives_read_only -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_video_frame_claim.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_video_frame_claim.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_compute_kernel.py::test_a_raise_inside_the_texture_cpu_door_propagates_and_follows_its_floors_publication_rule -->
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
  <!-- verify: git grep -n -e "copy_surface_to_surface" -e "torch.accelerator" -- examples -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli.py::test_the_scaffold_depends_on_streamlib_and_numpy_only -->
- **DECIDED** — A frame's DLPack export honours a request for the host side on every
  floor: `dl_device=(kDLCPU, 0)` hands back the surface's host mapping, so
  `numpy.from_dlpack(frame, device="cpu")` is one line on both floors — on macOS the same
  IOSurface pages the Metal capsule aliases, not a copy. `copy=True` stays refused by name
  at every door — the surface handle's `__dlpack__` and the device-tensor scope — and
  `as_numpy()` rides the same host request, one mapping, not two copies.
  [portable-gpu-interop — SHIPPED #2404]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_the_host_side_stays_reachable_on_explicit_request -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_a_copy_request_is_refused_at_both_doors -->
- **DECIDED** — The cross-floor check reads a Python processor's source and its
  `pyproject.toml` for what binds it to one floor and names the file, line and portable
  spelling of each. It runs inside `streamlib dev` and `streamlib run` as a warning that
  never blocks a start, gates in CI the wheel's own Python and the scaffold's output, and
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
  dependency with no `sys_platform`/`platform_system` marker. Nothing under a `sys.platform` guard is flagged, in either branch.
  `launch_app_node` prints the block on stdout between resolving the entry file and
  executing it — nothing when clean, a failure of the check itself reported, a start
  never blocked; on Python 3.10 the dependency rule is skipped and the block says so. CI
  gates the wheel's own Python and both scaffold variants on both lanes.
  [portable-gpu-interop — SHIPPED #2421]
  <!-- verify: pytest sdk/tatolab-stream/tests/test_cross_floor_check.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cross_floor_check.py -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_cross_floor_check.py::test_the_stream_packages_own_python_binds_to_no_floor -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cross_floor_check.py::test_the_runtimes_own_python_binds_to_no_floor -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_launch.py::test_a_scaffolded_app_with_a_cross_floor_finding_warns_and_starts_anyway -->
- **DECIDED** — streamlib offers an extension point for an external control client, and runs
  complete without one. [one-runtime-per-machine]
- **DECIDED** — The package split and the lend. `tatolab-stream`, importing as
  `tatolab.stream`, is pure Python and is everything a stream module imports: `@stream`,
  `@node`, `@input`, `@output`, the `Stream` builder and its references, the built-in node
  classes with their config shapes, and the data types; a stream is written, type-checked and
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
  runtime process. Owner, 2026-10-02. [package-split-and-lend; one-runtime-per-machine; amended by
  authoring-names: `@node.input`, `@node.output`, the `StreamBuilder`]
- **OPEN** — How an external control client plugs in: an entry point with a role of its own
  beside today's two, or another seam. What it hands the runtime beyond a relay address and a
  credential is the sharing step's to decide (§Networking). Known (2026-10-04): no stream map,
  router or peer identity exists to push. [one-runtime-per-machine; moq-on-the-tailnet]
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
  is §Packages' packs OPEN. The extensions' entry-point group is `tatolab.extensions`, and the
  Rust crate for writing streams is `tatolab-stream`. Owner, 2026-10-01. [tatolab-names;
  one-runtime-per-machine]
- **DECIDED** — No public Python name Tatolab publishes says "processor". The move from
  `streamlib` to `tatolab.*` re-spells every one still standing, at once:
  `ProcessorOwnedWindow` → `NodeOwnedWindow`, `ProcessorOwnedWindowEvents` →
  `NodeOwnedWindowEvents`, `ProcessorOutputTextureRing` → `NodeOutputTextureRing` (its module
  `node_output_texture_ring`), `ProcessorLinkDataAccess` → `NodeLinkDataAccess`, the GPU
  capabilities' `acquire_texture_from_processor_output_pool` and
  `acquire_storage_buffer_from_processor_output_pool` → `…_from_node_output_pool`, and the
  contexts' `processor_id` → `node_id`, the id `graph` renders on the node. A name an earlier
  change deletes is deleted, never renamed. The engine's Rust identifiers and the wire keep
  "processor" until the rename step. Owner, 2026-10-02. [tatolab-names; package-split-and-lend]
- **DECIDED** — The authoring names, with no alias. The object a `@stream` function is handed is
  a `StreamBuilder`, and the scaffold, the docs and every refusal model the parameter as
  `stream_builder`: `@stream def main(stream_builder: StreamBuilder)`; the decorator keeps
  `@stream` and a stream stays the decorated function. A node declares its ports with
  `@node.input` and `@node.output` — attributes of the `@node` decorator, so a node module
  imports `node` alone for them and no name it imports shadows Python's builtin `input()`. A
  Rust node's attribute macro keeps `input(…)` / `output(…)`, already the same word, and a node
  reference keeps `input(name)` / `output(name)`. They land in this milestone, ahead of the
  stand-alone stream package. Rejected: `@incoming` / `@outgoing` for ports — vocabulary no
  Python dataflow or media framework uses; `@input_port` / `@output_port` — a word the
  declaring side alone would carry (owner, 2026-10-06). Owner, 2026-10-06. [authoring-names]
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
  that venv, and the one door into the runtime process — a capability extension's hook — opens
  only to what the installer put beside the runtime. Undecided: the registry's owner and
  standards, a manager UI, and what the app's own catalog does. [one-runtime-per-machine]

## Consumers — examples & packages — IN-FLIGHT (→ jpeg-after-the-robotics-cut)
<!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-31-consumer-tree-disposition.md -->

- **DECIDED** — `examples/` is the in-repo showcase and living documentation of the
  current authoring idiom, converted gradually and never a contract source: engine
  contracts are stated in the engine and proven by engine tests, and no example is read
  to infer what the engine guarantees. An external examples repository (the
  framework-repo/examples-repo model) remains a possible later move and is not decided
  now. [consumer-tree-disposition — SHIPPED #2052, #2053, #2054, #2055, #2056, #2057,
  #2058, #2059]
  <!-- verify: examples/*/pyproject.toml -->
- **DECIDED** — `packages/` holds first-party extension wheels — the optional
  capabilities §Packages & extension model decides ship outside the wheel, with its
  built-in criterion deciding which side of the line a capability lands on. Each is an
  ordinary pip-installable Python package depending on the streamlib wheel through its
  public surface, never linking the engine.
  In-repo consumers (examples included) link a package locally as a Python path
  dependency — no publish loop stands between an example and the package it uses.
  Externally, packages publish through the same GitHub-hosted PEP 503 index the wheel
  uses (PyPI after the rename). `test-fixtures` remains as the tree's one
  engine-adjacent Rust crate. [consumer-tree-disposition — SHIPPED #2052; extension-model]
  <!-- verify: grep -n "packages/" Cargo.toml -->
- **DECIDED** — Conversion is a from-scratch rewrite in the current idiom, never an
  in-place upgrade: start from the `streamlib new` scaffold, mine the old directory for
  its logic only, author against today's full surface (delivery profiles, window
  contracts, cast objects, kernels-as-objects), and delete the old directory in the same
  PR. Every pre-pivot consumer neither deleted nor held below is conversion backlog under
  this doctrine, and that backlog is executed in full: `audio-mixer-demo`,
  `microphone-reverb-speaker`, `raytracing-showcase` and `fisheye-object-detection` are
  Python, `camera-compute-kernel` and `camera-halftone` are kernel examples, and
  `tokio-integration` is a plain cargo project. `examples/` stands at thirteen converted
  beside two held. A showcase authored in the current idiom is an ordinary addition under
  the convention below, not conversion backlog: `camera-codec-roundtrip` is the codec
  blocks' showcase; `camera-audio-recorder` is the recording showcase — `CameraSource →
  H264Encoder → Mp4Sink` beside `MicrophoneSource → OpusEncoder → Mp4Sink`, the camera
  also fanned to a `DisplayWindow`, Ctrl-C stopping and closing the file;
  `camera-webrtc-publish` sends camera and microphone through the codec blocks to
  `WhipPublisher`, credentials from the environment; and `camera-virtual-camera` is the
  virtual camera's showcase — one `CameraSource` fanned to a `VirtualCameraSink` and to a
  Python effect feeding a second one, so a graph appears as two named cameras in any other
  application on the machine and both are gone at Ctrl-C.
  [consumer-tree-disposition — SHIPPED #2053, #2054, #2055, #2056, #2057, #2058, #2059;
  codec-roundtrip-reproof — SHIPPED #2087; python-codec-block-api — SHIPPED #2108;
  opus-mp4-recording-rung — SHIPPED #2129; networking-extension-wheels — SHIPPED #2153;
  virtual-camera-sink — SHIPPED #2198]
  <!-- verify: git ls-files examples/camera-halftone examples/camera-compute-kernel examples/fisheye-object-detection examples/camera-codec-roundtrip examples/camera-virtual-camera -->
  <!-- verify: git ls-files examples/camera-audio-recorder/app.py examples/camera-audio-recorder/pyproject.toml -->
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
  <!-- verify: git grep -n -e "ModelInputTensorKernel" -e "copy_surface_to_surface" -- examples -->
- **DECIDED** — Retired in one sweep:
  `examples/pipelines`, `examples/camera-deno-subprocess` (its halftone effect rebuilt as
  `examples/camera-halftone`), `examples/camera-python-subprocess`,
  `examples/polyglot-manual-source`, `examples/camera-rust-plugin`,
  `examples/vulkan-video-roundtrip-cdylib-camera`, `examples/dynamic-reconfigure`,
  `examples/api-server`, `examples/api-server-demo`, `examples/runtime-graph-json-demo`,
  `examples/hello-streamlib` (the `streamlib new` scaffold is the hello; `camera-display`
  is the canonical minimal example), and `packages/audio`, `packages/camera`,
  `packages/display`, `packages/frame-tap`, plus the `packages/core` stub. A test owns its
  fixtures: CI reaching into `examples/` would make a consumer a contract source.
  [consumer-tree-disposition — SHIPPED #2052]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-31-consumer-tree-disposition.md -->
- **DECIDED** — A consumer blocked on an undecided domain is held in-tree until the
  align covering that domain mines it for logic; its deletion rides that change's own
  ship. Held on codec blocks: `packages/jpeg` and `examples/jpeg-psnr`, which delete outright
  with the retired `JpegDecoder` rather than waiting on a rung. Held on audio plugins:
  `packages/clap`. Held on screen capture: `packages/screen-capture`,
  `examples/screen-recorder`. [consumer-tree-disposition — SHIPPED #2052; amended by
  jpeg-after-the-robotics-cut: the JPEG pair deletes outright]
  <!-- verify: git ls-files packages/clap packages/screen-capture examples/screen-recorder -->
- **DECIDED** — No additional native-processor *distribution* mechanism is owed
  pre-1.0: an extension wheel is an ordinary Python package on the ordinary index, and
  closed-source Rust processors for Rust apps are deliberately not a path — a
  closed-source vendor ships the Python package whose native internals expose handles.
  What the extension-model pivot adds is not distribution but *registration*: the
  capability extension's support hook, declared by a standard entry point pip records
  and the engine runs once per process, which §Packages & extension model owns.
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

## Processor model & scheduling — IN-FLIGHT (→ stream-graph, package-split-and-lend, runtime-hosting)

- **DECIDED** — A link is pure plumbing: output port → input port, carrying a bag
  (self-describing msgpack named map). The engine has no type layer: ports carry no
  type declaration, connect never inspects or compares types and never warns, no read
  path examines a tag, and the frame header carries no schema ident. Consuming is a
  cast at read time; a mismatch surfaces as a decode failure at the consuming
  processor. One carve-out, and only one: declaring an audio window contract **is** that
  port's opt-in to the engine reading its bags as `AudioBlock`, so the engine inspects a
  payload on exactly the ports that asked it to. A link into a port with
  no contract is unchanged in every respect — still pure plumbing, `connect` still
  compares nothing, and the frame header still carries no schema ident.
  [schema-free-ports — SHIPPED #1814; audio-port-window-contract — SHIPPED #2033]
  <!-- verify: cargo test -p streamlib-ipc-types frame_header_size_matches_constant -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_block_bag_wire_codec::tests::a_bag_carrying_extra_keys_is_read_rather_than_refused -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-11-schema-free-ports.md -->
- **DECIDED** — A port declares three things, plus an optional window contract on an
  audio input, and nothing else: name, description, and — on an input — delivery profile,
  beside which an audio input may declare the window contract §Media I/O states. Type
  information belongs to the authoring language and never reaches the engine: in Python
  the port method's return annotation is the declaration, read by humans and type
  checkers only, with `ctx.inputs.read(port)`
  yielding the bag as a mapping and `read(port, into=T)` the opt-in strictness dial
  (a TypedDict casts for free, a dataclass or pydantic model constructs and validates,
  raising at read); in Rust the read target's `Deserialize` impl is the validation,
  always on, with no free-cast mode. [schema-free-ports — SHIPPED #1816, #1812]
  <!-- verify: sdk/streamlib-python-wheel/tests/test_read_into_target.py -->
- **DECIDED** — A read can name the inbound link it drained. Beside `read_raw`, a reader
  offers a read that returns the bag, its stamp and the *inbound link* it arrived on,
  named by the source channel name the link subscribed to — `<lowercased producer
  processor id>/<output port>`, the name `graph` and `tap` already show. The mailbox
  already queued each frame holding its link's identity for drop attribution; this
  exposes the identity the per-link counters are keyed by, so no frame carries anything
  it did not carry before and counting is unchanged. In Python `LinkInputDataReader`
  gains the same read, in two spellings — `read_from_inbound_link(port, into=T)`, handing
  back the cast and the link name, and `read_from_inbound_link_with_timestamp(port,
  into=T)`, handing back the producer's stamp beside them — so a Python-authored
  many-input sink is possible rather than deferred, and one that restates a producer's
  timing downstream has the stamp to restate. A destination can also enumerate its
  inbound links at `setup()`
  (`inbound_link_names(port)`), which is how a sink learns how many tracks it owes. A bag
  the port never enumerated a link for is refused by name rather than borrowing one.
  A helper-placed destination is *told* its link's name rather than deriving it: the wiring
  envelope's input entry carries `inbound_link_name` — the channel's own name — beside
  `channel_service_name` for every link, and `wire_input_link` takes it as a required
  positional — not defaulted,
  because a default meaning "the channel name" is the back-compat shim the doctrine bans and
  would make a silently wrong name reachable. A helper-protocol change, made safe by the
  build id above.
  [opus-mp4-recording-rung — SHIPPED #2124; networking-extension-wheels — SHIPPED #2150;
  cross-runtime-links — SHIPPED #2287; reopened by one-runtime-per-machine: whether addresses
  gain a stream level; amended by moq-on-the-tailnet: naming a link pulled from another
  machine is the sharing step's]
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::input::tests::two_inbound_links_hand_a_reader_the_link_each_bag_arrived_on -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::input::tests::naming_the_inbound_link_a_bag_arrived_on_leaves_the_per_link_drop_counts_alone -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::input::tests::a_port_lists_the_inbound_links_wired_into_it_and_a_port_with_none_lists_none -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::input::tests::an_injected_bag_with_no_inbound_link_is_refused_by_name_rather_than_borrowing_one -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_inbound_link_read_with_timestamp.py -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_native_destination_knows_each_link_by_its_channel -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::the_envelope_hands_a_helper_its_channel_as_the_name_it_knows_the_link_by -->
- **DECIDED** — The delivery profile is the whole of channel policy: one word, declared
  port-locally at the consuming input port. Every input port declares its delivery profile explicitly — there is no default
  and nothing left to infer one from, so an input port without one is a wiring error.
  Ring depth and overflow policy are engine-chosen and are not authorable: no port
  declares a depth, a leak policy, or a queue element, and there is no second surface
  that tunes one. [schema-free-ports — SHIPPED #1811; delivery-profile-vocabulary —
  SHIPPED #2024, #2025]
  <!-- verify: cargo test -p streamlib-engine missing_declaration_is_a_wiring_error_naming_the_port -->
  <!-- verify: sdk/tatolab-stream/tests/test_node_declaration.py::test_an_input_port_without_a_delivery_profile_is_refused -->
- **DECIDED** — The delivery profile names a read policy and nothing else. There are
  exactly two: `newest` — the consumer drains to the most recent bag, older ones are
  passed over — and `ordered` — the consumer receives bags in publication order. Both
  drop under sustained pressure. Neither promises delivery, because on a link whose head
  is a device that will not wait, no port-local declaration can: backpressure only
  relocates the loss to the device edge. `lossless` is retired — the word promised what
  the runtime does not do. [delivery-profile-vocabulary — SHIPPED #2024]
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::delivery_profile::tests::newest_resolves_to_skip_drop_shallow -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::delivery_profile::tests::ordered_resolves_to_fifo_drop_deep -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::delivery_profile::tests::port_declaration_resolution::unknown_declared_value_is_rejected_with_the_legal_values -->
- **DECIDED** — No loss is silent. A bag dropped at a port is counted by the port that
  dropped it and is readable over the control plane in `graph`, alongside the processor's
  other metrics. Drops are counted per link, never as one blended total, so a future
  reflection of a link's count to its producer stays possible without recounting. A count
  is cumulative for the life of one wiring, not of the link id: disconnect takes it with
  the link and reconnecting the same id starts from zero, because a count outliving its
  link would name something `graph` no longer has. A drop is a normal, reportable event
  on a realtime link, never an error and never invisible — a run that lost most of its
  bags must not read as a healthy one. A `newest` port
  passing over bags to reach the most recent is the profile working, not loss at the
  port, and is deliberately uncounted. In the tree, the mailbox eviction, the subscriber
  ring's overwrite, the two receive-seam
  discards and a write refused at the channel ceiling are each counted where they happen and
  rendered under `metrics`, for a helper-placed processor as for an app-process one. A
  `graph` that reports no drops is proof that none happened, with the two residuals the
  entries below state.
  [delivery-profile-vocabulary — SHIPPED #2023; loss-visibility — SHIPPED #2268, #2269,
  #2270]
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::mailbox::tests::an_eviction_is_counted_against_the_link_whose_bag_was_lost -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::input::tests::each_inbound_link_reports_its_own_losses_at_a_stalled_ordered_port -->
  <!-- verify: cargo test -p streamlib-engine --lib core::graph::components::processor_metrics::tests::a_processors_metrics_render_every_inbound_links_losses_by_name -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::mailbox::tests::passing_over_bags_to_reach_the_newest_is_not_a_drop_at_the_port -->
- **DECIDED** — A bag the iceoryx2 subscriber ring overwrites is counted. Every channel's
  data service carries a per-channel 64-bit sequence number in an iceoryx2 user header —
  never in the frame header, which stays exactly as it is. A number is consumed once a
  sample reaches the send, unless the send fails before delivering to anyone; a bag refused
  at the ceiling or never loaned consumes none. Each subscriber keeps the last number it
  received per producing publisher, the first sample after wiring its baseline, so a
  restarted producer is never read as a gap; the numbers a jump skips — the new number
  minus the last, minus one — are added to that inbound link's dropped-bag count, on
  `ordered` ports only, since a `newest` port passing over bags is the
  profile working. The number is engine-internal: no processor reads it and no bag carries
  it. Six readings the build settled, none of them a mechanism the entry did not already
  state. **Per-channel is per producing publisher**: each publisher numbers its own sends
  from zero and each subscriber keeps its last number keyed by the publisher's id, so a
  publisher recreated when a source port's last link goes starts a new baseline rather than
  a gap. **"Fails before delivering to anyone" names exactly two send errors** — the two
  that fail before any delivery consume no number; the other three consume one, because
  iceoryx2 does not report a partial delivery, and a subscriber the failed send never
  reached therefore reads a real loss. **The header is one engine type**,
  `DataChannelBagSequenceNumberUserHeader`, with its iceoryx2 type name pinned so a move
  never changes its identity, and a source-walking gate refuses a data `publish_subscribe`
  builder outside the node wrapper so no site can open a service without it. **A gap and an
  eviction never double-count**: a ring gap is a bag never received and an eviction is a bag
  received and then displaced, and each lands once on the link's one counter. **A `newest`
  port counts neither** — the eviction at a skip-to-latest mailbox is the profile working.
  And **a bag
  dropped at the receive seam is counted on its link**: a frame too short for a header and a
  frame bound to a port with no mailbox each consume a number and reach no reader, so no gap
  could show them.
  Two stated residuals: a bag lost after a link's last receive and before its disconnect is
  not counted; and bags the ring overwrites before a link's first receive are not counted
  either, because that first sample is the baseline the gap is measured from. Counts land on
  the next receive for a native and a Python consumer alike — a consumer inside a long
  `process()` receives nothing, so its overwrites reach `graph` when it next reads, and no
  timer is added to shorten that.
  [loss-visibility — SHIPPED #2268]
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::an_ordered_consumer_that_stops_reading_renders_exactly_the_bags_its_ring_overwrote -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_newest_consumer_renders_no_loss_for_bags_passed_over_in_its_ring_or_its_mailbox -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::input::tests::a_replacement_publisher_on_a_live_subscriber_reads_as_a_new_baseline_not_a_gap -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::input::tests::a_frame_too_short_for_a_header_is_counted_on_the_link_it_arrived_on -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::input::tests::a_frame_bound_to_a_port_with_no_mailbox_is_counted_on_its_link -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::node::tests::a_channel_data_service_and_an_opener_disagreeing_on_the_user_header_are_refused_by_name -->
  <!-- verify: cargo run -p xtask -- check-iceoryx2-construction -->
- **DECIDED** — A write refused at the channel ceiling is counted on the producer, per
  output port. The ceiling is per channel and the refusal happens before the bag reaches any
  link, so it consumes no sequence number and no destination can ever see it; one counter at
  the single send seam serves a native and a Python producer alike, so a Rust author's `Err`
  is counted too, and `graph` renders `refused_bags_by_output_port: {port: n}` on the
  producing node. The cost is stated rather than hidden: a reader looking only at a
  destination's link does not see this loss. Rejected: adding the refusals into each
  outbound link's `dropped_bags_by_link` at its destination — it would need per-link
  baselines at the producer and a merge of two processes' counters at render (owner,
  2026-09-14). [loss-visibility — SHIPPED #2268]
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::output::tests::a_write_refused_at_the_ceiling_consumes_no_sequence_number -->
  <!-- verify: cargo test -p streamlib-engine --lib core::graph::components::processor_metrics -->
- **DECIDED** — A helper-placed destination's per-link counts reach `graph`. The parent
  creates one blackboard per helper spawn; the helper is its only writer, one entry per
  inbound link holding that link's dropped-bag count, and the parent reads it whenever
  `graph` renders, without waiting on the child. The last counts a crashed helper wrote stay
  readable; a dead writer can no longer update its entries, so a respawned helper gets a
  fresh board. A helper's write refused at the ceiling is
  counted the same way. The node's `metrics` key then renders for a helper-placed processor
  as it does for an app-process one.
  Five readings the build settled. **An entry is a slot carrying its wiring**: a
  blackboard's keys are fixed at creation and links arrive live, so the board declares one
  slot per inbound link the cap allows, and the parent assigns each link a slot and a wiring
  generation carried in its setup envelope entry or its `wire_link`. Each value holds that
  generation beside the link's dropped bags and discarded samples, and the parent renders a
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
  `requires_gpu` and therefore rig-only.
  [loss-visibility — SHIPPED #2270]
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_helper_placed_destinations_node_renders_the_counts_its_helper_wrote_on_its_slot -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::helper_process_loss_count_board::tests::the_parent_reads_each_entry_only_for_the_wiring_it_assigned -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::helper_process_loss_count_board::tests::a_write_from_a_wiring_the_slot_has_moved_past_lands_nothing -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::input::tests::every_loss_counted_on_a_mirrored_link_reaches_its_board_slot_as_it_is_counted -->
  <!-- verify: pytest -m requires_gpu sdk/streamlib-python-wheel/tests/test_helper_loss_counts.py -->
- **DECIDED** — No link ever blocks a producer: no profile resolves to producer-blocking and
  no overflow policy parks a producer. A processor publishing to a slow consumer loses bags at
  that consumer's port, counted as the entry above states; it is never parked. A parked
  producer cannot observe shutdown.
  [delivery-profile-vocabulary — SHIPPED #2025]
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::node::tests::overflow_enabled_publisher_does_not_block_on_full_buffer -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::channel_sizing_tests::every_channel_service_opens_under_safe_overflow -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-29-delivery-profile-vocabulary.md -->
- **DECIDED** — Loss-handling knowledge lives at the link's endpoints, never in the
  engine. The engine's whole role is to count a drop at the port that dropped it and
  surface it; it never inspects a payload — save on a port whose window contract is
  exactly that opt-in — never knows a bag holds a reference frame, and never acquires a
  drop rule that depends on content. The carve-out buys no drop rule either: the windowing
  stage reads samples, never a frame's role in a stream, and a windowed port drops on the
  same counted-mailbox terms as every other port. A producer that can make loss
  cheaper reacts at the source — an encoder under downstream pressure declines to encode
  raw frames and resumes at its next sync point, which is where loss belongs and costs
  least. A consumer on an encoded stream must bound loss — this is a requirement, not an
  option: a consumer that sees a gap discards until the producer's next sync point, and
  never commits or forwards a stream it knows is broken. No consumer drops or passes on
  encoded frames blindly. The information that makes both possible travels as
  ordinary bag fields the producer writes and the consumer casts, never as a tag in the
  frame header and never as engine-visible type. [delivery-profile-vocabulary]
- **OPEN** — Reflecting a link's drop count back to its producing port, so a producer
  can react to pressure it cannot otherwise see: intended, do not build until the first
  encoded-domain link exists — nothing in the tree reads it before an encoder does.
  Direction — only drops at `ordered` inputs (a `newest` input passing over bags is the
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
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-11-schema-free-ports.md -->
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
  <!-- verify: cargo test -p streamlib-engine --test attribute_macro_test the_descriptor_carries_the_config_types_schema_rather_than_its_name -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_node_config_class.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_config_construction.py -->
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
  in-tree config type derives the schema — the built-ins' configs, their enums and
  `ApiServerConfig` — and a field's `///` doc is its `description`, a serde default its
  `default`, a field with neither `required`. Every catalog document, from either language,
  is JSON Schema draft 2020-12
  with no `$schema` key: schemars 0.8 emits draft-07, so the Rust seam writes its
  definitions under `$defs` with references pointing there and a tuple's positional
  schemas as `prefixItems`. The two languages keep two spellings that are both valid
  2020-12 — Rust writes a nullable as `"type": [T, "null"]` and stamps a root `title`,
  Python writes `anyOf` with null and no title — and neither is converted.
  [agent-readable-processor-catalog — SHIPPED #2224]
  <!-- verify: cargo test -p streamlib-engine --test attribute_macro_test a_processor_declaring_no_config_takes_none_and_says_which_key_had_nowhere_to_go -->
  <!-- verify: cargo test -p streamlib-engine --test compile_fail_config_without_json_schema -->
  <!-- verify: cargo test -p streamlib-processor-schema the_document_is_2020_12_with_no_schema_key_and_no_definitions_keyword -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-09-13-agent-readable-processor-catalog.md -->
- **DECIDED** — The Python half, as built. `@processor` reads `__init__` through
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
  On the wire, `stream.add(cls, config={…})` carries a dict, the graph node stores its JSON, and
  `ctx.config` is that mapping. [agent-readable-processor-catalog — SHIPPED #2226]
  <!-- verify: pytest sdk/tatolab-stream/tests/test_node_config_class.py::test_a_keyword_parameter_is_refused_with_the_fix_named -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_config_construction.py::test_the_helper_constructs_the_processor_by_the_config_keyword -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_node_config_class.py::test_the_document_is_2020_12_with_no_meta_schema_key -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_config_catalog.py -->
  <!-- verify: pytest packages/streamlib-webrtc/tests/test_processors.py -->
- **DECIDED** — Port rendering in the control plane is name, description, delivery
  profile, direction, and — on an audio input that declared one — its window contract; no
  port carries a type in `graph`, `tap`, or any snapshot. A port that declared nothing
  renders no `audio_window` key at all rather than a null. A `match_device` port renders
  the five values its device settled — machine-dependent because the device format is,
  which is truer than a static lie — and renders the sentinel itself while nothing has
  settled it. [schema-free-ports — SHIPPED #1816; audio-port-window-contract — SHIPPED
  #2032, #2034]
  <!-- verify: sdk/tatolab-stream/tests/test_node_declaration.py::test_a_declared_port_carries_no_type_key_under_any_spelling -->
  <!-- verify: sdk/tatolab-stream/tests/test_node_declaration.py::test_a_port_declaring_no_contract_carries_no_audio_window_key -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_settled_contract_reaches_graph_on_the_port_that_settled_it -->
- **OPEN** — What a port reports about the bags it produces or accepts, and how a bag
  shape is described and served at all — by any author, the built-ins included — so an
  agent can wire a custom processor it did not write: an input that only receives and
  may be fed several shapes, an output that may write a duck-typed bag, nothing
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
  shutdown wake (an `eventfd` on Linux, a close-on-exec pipe written once on macOS), so a bag
  or a shutdown wakes it on arrival. The 100 ms channel-poll loop survives only as the
  fallback for a platform with neither queue or a waiter whose setup failed, and a wake that
  cannot be created leaves channel-only shutdown rather than a panic. `MonotonicTimer` runs on
  both floors behind the unchanged Python surface: `timerfd` on Linux; on macOS a one-shot
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
  <!-- verify: cargo test -p streamlib-engine --lib core::execution::thread_runner::tests::a_shutdown_seen_only_on_the_wake_fd_ends_a_loop_blocked_in_its_wait -->
  <!-- verify: cargo test -p streamlib-engine --lib core::execution::thread_runner::tests::a_delivered_notify_makes_the_wait_report_notified -->
  <!-- verify: cargo test -p streamlib-python-wheel --lib python_monotonic_timer::tests::late_wakes_never_move_a_later_deadline_off_the_grid -->
  <!-- verify: cargo test -p streamlib-python-wheel --lib python_monotonic_timer::tests::a_kqueue_deadline_is_armed_absolute_in_the_mach_absolute_time_epoch -->
  <!-- verify: cargo test -p streamlib-engine --lib apple::media_clock::tests::a_tick_rounded_up_from_nanos_never_converts_back_to_an_earlier_nano -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_helper_process.py::test_a_continuous_processor_runs_at_the_start_rather_than_one_interval_in -->
- **DECIDED** — Helper-process placement is the only execution placement. Every Python
  processor runs in its own child process — its own interpreter, its own GIL — spawned
  by the Rust engine as an exec of `sys.executable` from the app's venv: never fork
  (GPU contexts are fork-unsafe), never `multiprocessing` or a worker pool (the engine
  owns the child's lifecycle from its compiler ops and needs no GIL to manage it).
  In-process hosting of a Python processor does not exist — not as a default, a
  fallback, an optimisation, or an engine choice. Isolation, not latency, is the
  optimised axis: no processor may ever block, stall, or degrade another. Same user
  code, one venv, no per-processor environments, no placement surface of any kind.
  Helper children import the wheel itself — one native artifact. Every processor class
  must be import-addressable from a module whose import is side-effect-safe; there is
  nothing to equalize and nothing to move between, because there is no second
  placement. [helper-process-placement-only — SHIPPED #1714; amended by package-split-and-lend: the exec
  is the stream's own venv interpreter with the runtime's lend directory, which holds
  `tatolab/runtime/`, prepended to its `PYTHONPATH`, so a child imports the lent portion, not a wheel in its venv; per-stream
  environments in one runtime process are runtime-hosting's]
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
  <!-- verify: cargo test -p streamlib-engine --test surface_share_over_raw_mach -->
  <!-- verify: cargo test -p streamlib-engine --lib apple::surface_share::mach_surface_share_service::tests::an_admitted_pid_is_pinned_to_the_pid_version_it_first_connects_with -->
  <!-- verify: cargo test -p streamlib-surface-client destroying_a_receive_right_notifies_the_dead_name_watcher -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --lib core::context::surface_store::mach_surface_share_pool_tests::a_slot_whose_iosurface_is_in_use_is_not_rehanded_to_its_producer -->
  <!-- verify: cargo test -p streamlib-python-wheel --lib python_helper_process_pixel_exchange::macos::iosurface_pool_slot_import_tests::a_killed_helper_releases_the_frame_it_held -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-09-29-macos-platform-floor.md -->
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
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --test timeline_crosses_to_a_helper_as_a_metal_shared_event -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --lib apple::surface_share::cross_process_timeline_pair::tests::a_stalled_consumer_is_forced_past_within_the_bound_and_a_late_report_is_harmless -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --lib apple::surface_share::cross_process_timeline_pair::tests::a_release_reported_past_what_was_produced_is_refused_and_moves_nothing -->
  <!-- verify: cargo test -p streamlib-engine --lib apple::surface_share::mach_surface_share_service::tests::a_registration_announcing_one_timeline_port_is_refused -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --test texture_crosses_to_a_helper_on_an_iosurface a_texture_whose_timeline_pair_will_not_export_is_refused_and_registers_nothing -->
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
  <!-- verify: cargo test -p streamlib-consumer-rhi --lib iosurface_import_tests -->
  <!-- verify: cargo test -p streamlib-consumer-rhi --lib consumer_vulkan_sync::tests::a_shared_event_imports_at_the_producers_value_and_both_sides_observe_each_other -->
  <!-- verify: cargo test -p streamlib-engine --lib apple::surface_share::mach_surface_share_service::tests::a_texture_registration_round_trips_its_recipe_layout_and_timeline_ports -->
  <!-- verify: cargo test -p streamlib-python-wheel --lib python_helper_process_pixel_exchange::macos::texture::texture_check_out_tests::a_checked_out_texture_reads_its_iosurface_rows_and_releases_on_its_shared_event -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --test texture_crosses_to_a_helper_on_an_iosurface -->
- **DECIDED** — Shutdown always ends, and a cooperative processor's `teardown()` always
  runs. The engine stops every helper at once, never one after another, each on the same
  ladder: `stop` and `teardown` are sent together; a Python callback still running after one
  second is interrupted with `KeyboardInterrupt`, after which `stop()` and `teardown()` still
  run and the bag in flight is lost; `teardown()` then has five seconds; the helper's whole
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
  it terminates the helper's process group, and a recording keeps what its closed fragments
  hold, which is the case the fragmented layout was chosen for. A live `remove_processor`
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
  signal, so a macOS helper arms its own watch at boot, before capability extensions or its
  processor's module load — kqueue `EVFILT_PROC` with `NOTE_EXIT` on the parent's pid,
  belt-and-braces with a dead-name notification on a boot-time connection to the
  surface-share service. Whichever fires first shuts the escalate socket, so the helper reads
  the end of its channel and runs the `stop` and `teardown()` the engine can no longer send,
  while a thread that needs no GIL walks the ladder's own budgets: it interrupts a callback
  still running after one second and kills the process group of whatever is alive at 6.5 s,
  skipping the terminate rung because the teardown budget is already spent. A `SIGKILL`ed app
  therefore leaves no helper on either floor, and on macOS the helper's `teardown()` still
  runs. **A third interrupt exits 130 on both floors**: the run loop parks for good while the
  process is being ended at once, so killing the helper groups can no longer let `run()`
  return inside the log-flush grace and exit 0.
  [shutdown-ladder; local-transport-hardening — SHIPPED #2264, #2266; macos-platform-floor —
  SHIPPED #2357; macos-capability-parity — SHIPPED #2410; amended by one-runtime-per-machine:
  an installer-registered per-user service starts the runtime, which never detaches itself]
  <!-- verify: sdk/streamlib-python-wheel/tests/test_helper_placement.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_helper_placement.py::test_a_processor_interrupted_while_still_setting_up_still_tears_down -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_helper_placement.py::test_a_worker_a_processor_forked_goes_down_with_the_apps_helper -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_three_helpers_slow_to_stop_cost_about_one_ladder -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_a_process_the_app_started_never_holds_the_apps_output_past_its_exit -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_no_helper_outlives_an_app_killed_outright -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_a_helper_whose_app_was_killed_still_runs_its_teardown_on_macos -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_a_third_ctrl_c_kills_every_helper_process_group_and_exits_130 -->
  <!-- verify: cargo test -p streamlib-engine --lib core::signals::tests::sighup_is_not_owned_on_macos -->
  <!-- verify: cargo test -p streamlib-python-wheel --lib python_helper_process_parent_death_watch::tests::the_watch_fires_when_the_watched_process_exits -->
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
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_helper_that_cannot_open_its_port_leaves_the_link_in_error_with_its_reason -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_link_no_helper_has_to_answer_for_is_wired_as_soon_as_it_is_opened -->
  <!-- verify: cargo test -p streamlib-api-server the_instructions_and_every_wiring_prompt_say_what_pending_and_error_mean -->
- **DECIDED** — A helper refuses to start unless the engine it imported is its parent's
  build. The build id is the crate version, the git sha — `unknown` where the build has no
  `.git` — and a nonce minted per build by the engine's build script, compiled into
  `_engine.abi3.so`. The parent passes its own in the helper's environment; the helper
  compares before it opens any channel or socket, and on a mismatch writes a refusal naming
  both ids to raw stderr and exits, so the parent reports that processor's start as refused
  and names the helper's stderr. An absent id is a refusal too, never a silent pass.
  Rejected: a hand-bumped subprocess protocol version beside the build id — it never caught a
  helper built against a different iceoryx2 patch or a stale wheel on the helper's `sys.path`
  (local-transport-hardening, 2026-09-14).
  [local-transport-hardening — SHIPPED #2262; amended by package-split-and-lend: the lend
  gives both sides one build by construction, and this check stays as the backstop]
  <!-- verify: cargo test -p streamlib-engine --lib core::engine_build_id_composition -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_helper_placement.py::test_a_helper_that_imported_another_engine_build_is_refused_naming_both_builds -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_helper_process.py::test_a_helper_handed_no_engine_build_id_refuses_rather_than_passing -->
- **DECIDED** — The MVP edit loop is re-running `dev` (warm restart is sub-second by
  construction). Reload-on-save is a nicety, not MVP-gating, and when built it is
  processor-granular — stop the processor, respawn its helper (a fresh interpreter
  re-imports the class), rewire its ports — never module-loading machinery.
  [importable-python-library — SHIPPED #1711]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_launch.py::test_a_bad_config_is_reported_without_a_launcher_traceback -->
- **DECIDED** — A processor's identity is its class, named by its fully-qualified
  import path (`my_app.filters:BlurProcessor` in Python, the type path in Rust) —
  derived mechanically, never authored, and the same string in the registry, in the
  control plane's type field, and for spawning the processor's helper process — which
  is how every Python processor runs. A processor defined in the entry file run as
  `python <script>.py` identifies as `__main__:<Type>` and is a wiring error at `stream.add`,
  with an error naming the fix (move the class to an importable module and import it
  from the entry file — one import line). The entry file itself may still run as
  `__main__`; only processor classes may not live there.
  `@processor` declares execution, interval, scheduling
  priority, and description only. Mechanically means at the authoring seam, never from a
  runtime reflection API: Rust captures the type path where the macro expands, because
  `std::any::type_name`'s output format is unspecified across compiler versions and must
  never key a registry; Python joins `__module__` and `__qualname__` with a colon. Nothing
  derives a per-processor isolation tier: every processor runs at the one tier the engine
  assigns. The `FullAccessGrant` moat is a compile-time guarantee
  about who may mint an in-process `RuntimeContextFullAccess`, never a placement question.
  [processor-class-identity — SHIPPED #1837, #1839, #1840, #1841;
  helper-process-placement-only — SHIPPED #1714; amended by built-in-node-type: a built-in is
  named by its class's import path in `tatolab.stream`, never by its Rust type path]
  <!-- verify: cargo test -p streamlib-engine --test processor_class_import_path_test -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_identity.py::test_the_launch_arrangement_never_changes_the_identity -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_identity.py::test_a_processor_declared_in_the_entry_file_is_refused -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-12-processor-class-identity.md -->
- **DECIDED** — A built-in node's type is the import path of its class in `tatolab.stream` —
  `tatolab.stream:CameraSource` — the same string in the runtime's registry, on the stream
  library's class, in every graph and over the local API, from Python and from Rust alike. The
  runtime registers each built-in under that string; no table translates it to another name,
  and a built-in's Rust module path never reaches a graph. Every node's type is thus the
  import path of the class a stream names: a type under `tatolab.stream` is one of the
  runtime's built-ins, and any other is imported in the stream's own interpreter. A type
  changes only when its class's public name does — renaming or moving the runtime's crates
  and modules never changes one — and the first golden graph a runtime is held to already
  carries it. Owner, 2026-10-02. [built-in-node-type; package-split-and-lend]
- **DECIDED** — A Python processor class registers its descriptor — identity,
  description, ports, config schema — when `@processor` runs, so it is in the processor
  catalog before its first add; the constructor arrives at first add exactly as today.
  Decoration inside a helper process registers nothing, because a helper hosts no
  graph. A class decorated twice under one import path meets the existing
  duplicate-path refusal. As built: no longer — `package-split-and-lend` deleted
  `register_declared_processor_class` and
  `ProcessorInstanceFactory::install_constructor_for_registered_descriptor`, `@node` registers
  nothing, and this entry folds into that change's "`@node` registers nothing" at ship.
  [agent-readable-processor-catalog — SHIPPED #2228; reopened by one-runtime-per-machine: declarations readable without an engine]
- **DECIDED** — An instance's display name is the human-facing label — passed at `add`,
  readable off the returned handle, and the prefix on its log records; it defaults to
  the class's short name and the engine disambiguates duplicates within one graph. It is
  also the processor's part of its port address `<runtime name>/<display name>/<port>`
  (§Networking), so renaming a processor re-addresses its ports. Being part of that address
  is what bounds it: `add`
  refuses a requested display name that is empty, contains `/`, `*`, `$`, `#` or `?`, or
  begins with `@`, naming the character and the fix, in Rust and in MCP
  `add_processor` alike. Spaces and unicode stay legal, and a class's short name and the
  engine's ` 2` suffix always pass, so no default display name is ever refused.
  Identity is never derived from it — and neither is the default: a descriptor carries
  the class's short name as its own validated field rather than the engine splitting one
  out of the import path, because splitting would re-invent an identity grammar.
  [processor-class-identity — SHIPPED #1838, #1841; runtime-mesh — SHIPPED #2282; reopened
  by one-runtime-per-machine: whether addresses gain a stream level; amended by
  one-runtime-per-machine: a duplicate the author typed is refused by name, and only a
  defaulted duplicate is disambiguated — stream-graph builds it; amended by runtime-hosting
  decision 3: a name is cast to the address grammar (§Networking, the address) and a
  defaulted duplicate takes `-2`, replacing spaces, unicode, ` 2` and the character
  refusals]
  <!-- verify: cargo test -p streamlib-engine --test node_name_test -->
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::address_chunk -->
  <!-- verify: cargo test -p streamlib-engine --test graph_snapshot_round_trip_test a_loaded_name_already_in_the_graph_is_refused_rather_than_suffixed -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_stream_graph_builder.py::test_a_typed_duplicate_is_refused_at_the_add_that_typed_it_naming_both -->
- **OPEN** — Additional execution flavors to scale processor count (lightweight /
  green-thread style): intended, do not build until designed; hard constraint — no new
  configuration dials. [execution-model]
- **DECIDED** — A graph is data: a stream's `@stream` function compiles to the stream's graph, a
  serializable description a runtime runs — nodes by class import path with their config and
  name, the links, and what the stream exposes. It is one shape with what `graph` renders: the
  engine's existing round-trippable snapshot extended, rendered live with state and counters
  beside the spec, never a second format (sentence 1; the one-shape reading follows engine
  doctrine). [one-runtime-per-machine; stream-graph]
- **DECIDED** — The graph is emitted, never authored. A stream's function produces it, the
  runtime exports the live one in the same shape, and nobody writes one by hand as the source
  of a stream — a stream's source is its Python, which keeps the retired manifest retired. On
  the next start the function wins: an agent that edits a running stream over the local API
  changes the live graph, and keeps the change only by changing the code (owner, 2026-09-30).
  "The function" is the graph it compiled to at its last load: a kept stream re-loads that
  recorded graph, and picking up a changed source is another `run -d` (§Product, how a
  stream is loaded and kept). [one-runtime-per-machine; stream-graph]
- **DECIDED** — A stream's environment is its project directory and that directory's venv
  interpreter. It is recorded beside the graph when the stream is loaded — never inside it, so
  the same graph loads from another checkout — and every processor interpreter of the stream
  starts from it, its working directory the project. The runtime adds nothing per stream and
  nothing from the caller — no `.env`, no `-e`, no caller's shell; a stream's compile and its
  processor interpreters inherit the runtime's own environment — and a stream reads its own
  settings from its project as any program does
  (runtime-hosting decision 4). Provisioning an environment is
  the packs OPEN in §Packages. Owner, 2026-10-02. [package-split-and-lend; runtime-hosting]
- **OPEN** — What the graph holds beyond nodes, links and exposures: a stream's needs.
  Direction (review, not decided): a camera, a microphone, a display, the accelerator, network
  exposure — derived from its nodes' declarations (built-ins carry theirs; a user node that
  opens a device directly says so on `@node`) and carried in the graph, never authored; the
  runtime requests exactly those at load and refuses by name what the machine cannot grant, so
  nothing enumerates every possible device up front. Decided with the resources entry below.
  [one-runtime-per-machine]
- **DECIDED** — Several streams in one runtime process. The runtime keeps, once for the
  machine: the one `GpuContext` every stream shares, signal ownership and the local API.
  Everything else the tree keeps once per process today
  becomes per stream: the event topic, the processor registry, the interpreter a stream's
  nodes start from, the log file, shutdown and its escalation, the teardown watchdog, and the
  process-group table of its processor interpreters — so one stream's shutdown, crash budget
  or graph change never touches another's. A stream links to any private or public port of
  another stream on the same machine (§Networking, exposure), over the
  local transport, and surfaces are shared
  across every stream's processor interpreters on both floors, all being the runtime's
  children, so a link between two streams on one machine copies no pixels. Streams needing
  conflicting Python packages each start from their own venv (the package split and the lend,
  §Packages). Owner, 2026-10-01; the surface clause confirmed 2026-10-02. [runtime-hosting;
  one-runtime-per-machine; amended by moq-on-the-tailnet: the machine's MoQ endpoint is kept
  once for the machine, built at the sharing step]
- **OPEN** — Resources across streams: requests and limits, realtime priority across streams,
  admission control, and how a stream states what it needs. Direction (review, not decided):
  what a stream needs is read from its graph (the derived needs above) and granted per stream
  at load by the runtime — on Apple behind the OS prompt, on Linux by the runtime's own grant —
  and remembered; a control client reads the same list and never has to predict it.
  [one-runtime-per-machine]
- **DECIDED** — Failure isolation: one engine for every stream on the machine. A native crash
  in a built-in — camera, codec, display — ends every stream on the
  machine, and the runtime restarts and re-loads every kept stream neither stopped nor failed
  from its persisted graph, while an attached stream ends with its `run` (§Product); a hang is
  bounded per node by the existing abandon budget and ends only that stream; Python crashes and hangs
  stay in their own process. "Restart the runtime and the streams come back" means re-loading
  from persisted graphs, never re-attaching — processor interpreters die with their parent by
  construction and surfaces cannot outlive the device's owner — so the runtime keeps a state
  directory holding each loaded graph (the runtime directory is
  deliberately ephemeral today). Owner, 2026-09-30: a driver-level fault taking every stream
  down for a few seconds is the equivalent of a fundamental Docker issue crashing everything;
  one engine is easier to run and to get bug reports for, and an engine per stream multiplies
  the ways things can fail. An engine per stream is not a fallback. [one-runtime-per-machine]

## Graphics (RHI / GPU) — DECIDED (unbuilt: accelerators optional, pivot step 5)

- **DECIDED** — All Vulkan lives in the RHI (`vulkan/rhi/` + `streamlib-consumer-rhi`); one
  kernel abstraction per pipeline kind; consumers go through `GpuContext` only. Vulkan is the
  one RHI on every supported platform: MoltenVK is the macOS driver, reached through the same
  `HostVulkanDevice` and, in a helper, the same `ConsumerVulkanDevice`. Metal appears only as
  handles exported from Vulkan objects (`vkExportMetalObjectsEXT`) — the `MTLBuffer` behind an
  imported IOSurface, the shared event behind a timeline. There is no second backend, no
  per-platform RHI, and no backend selector at build or run time.
  [macos-platform-floor — SHIPPED #2355, #2356; macos-capability-parity — SHIPPED #2402, #2404]
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests a_device_comes_up_on_this_hosts_driver_and_names_itself -->
  <!-- verify: cargo test -p streamlib-consumer-rhi an_iosurface_import_exports_a_metal_buffer_over_the_surfaces_own_pages -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-09-29-macos-platform-floor.md -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-09-26-macos-capability-parity.md -->
- **DECIDED** — Instance and device creation are one probed path on every platform. Host and
  helper instances request `VK_KHR_portability_enumeration` with `ENUMERATE_PORTABILITY_KHR`,
  and both devices request `VK_KHR_portability_subset`, wherever the driver advertises them.
  Both request one instance API version, `REQUESTED_VULKAN_INSTANCE_API_VERSION` (1.4), shared
  from `streamlib-consumer-rhi` so host and helper cannot resolve different entry points
  across the IPC seam. It is the floor that makes the promoted 1.3 entry points resolve and is
  never inferred from a device query — MoltenVK clamps a device's reported `apiVersion` to
  whatever the instance asked for — and a source gate bans branching on the reported version,
  tests included. [macos-platform-floor — SHIPPED #2356]
  <!-- verify: cargo test -p streamlib-engine the_requested_instance_api_version_clears_the_promoted_entry_point_floor -->
  <!-- verify: cargo run -p xtask -- check-no-device-api-version-branch -->
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
  A vsync-off present request on a surface advertising no `MAILBOX` takes FIFO — never
  IMMEDIATE, which tears — and says so once per process.
  [macos-platform-floor — SHIPPED #2356, #2357, #2374; macos-capability-parity — SHIPPED #2403]
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests ray_tracing_on_a_device_without_it_refuses_rather_than_panicking -->
  <!-- verify: cargo test -p streamlib-engine the_ray_tracing_refusal_names_the_tier_and_the_extension -->
  <!-- verify: cargo test -p streamlib-engine device_without_video_refuses_every_codec_operation_naming_its_direction -->
  <!-- verify: cargo test -p streamlib-engine a_subgroup_operation_in_a_stage_the_driver_does_not_serve_is_refused -->
  <!-- verify: cargo test -p streamlib-engine a_vsync_off_request_takes_fifo_where_the_driver_advertises_no_mailbox -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_ray_tracing_tier_refusal.py::test_every_ray_tracing_constructor_refuses_at_setup_naming_the_absent_tier -->
- **DECIDED** — The engine's kernel primitives are exposable to Python as configured
  blocks: shader/compute source and binding config passed from Python, compiled and
  executed by the engine on its device — no user-side Vulkan, ever.
  [importable-python-library — SHIPPED #1717; python-kernel-surface — SHIPPED #1773, #1775]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_compute_kernel.py -->
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
  <!-- verify: cargo test -p streamlib-engine subprocess_escalate::compute::tests -->
  <!-- verify: cargo test -p streamlib-engine subprocess_escalate::graphics::tests -->
  <!-- verify: cargo test -p streamlib-engine subprocess_escalate::ray_tracing::tests -->
  <!-- verify: cargo test -p streamlib-engine subprocess_escalate::export_staging::tests -->
  <!-- verify: cargo test -p streamlib-engine every_staging_op_and_acquire_image_refuse_on_macos_naming_the_reason -->
  <!-- verify: cargo test -p streamlib-engine a_uniform_buffer_binding_is_refused_naming_its_kind -->
  <!-- verify: cargo test -p streamlib-engine a_trace_refuses_a_storage_buffer_binding_by_name -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_graphics_kernel.py::test_a_uniform_buffer_binding_is_refused_naming_its_kind -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_graphics_kernel_surface.py::test_a_draw_takes_no_vertex_buffer_no_index_buffer_and_no_depth_target -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_graphics_kernel_surface.py::test_a_graphics_kernel_carries_no_depth_or_vertex_input_state -->
- **DECIDED** — A kernel's output is an engine-owned texture that Python names by
  surface id and passes downstream in a bag, and that a third-party GPU library in its
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
  dial. Linux: single-plane render-attachment usage takes explicit-modifier DMA-BUF where the
  render-target modifier probes available; a CUDA-mappable format whose usage sits inside the
  OPAQUE_FD set takes OPAQUE_FD where that image pool exists. macOS: any single-plane format
  on a device with `VK_EXT_metal_objects` takes an `OPTIMAL` image over a private IOSurface
  the engine creates — never `kIOSurfaceIsGlobal`, which MoltenVK's own export path sets —
  bound to a device-local memory type that is not host-visible, chosen by query; a
  host-visible type would eagerly allocate a private `MTLBuffer` per image. Everything else
  keeps a non-importable allocation. A planar format is refused an IOSurface-backed image by
  name: NV12 render targets are absent on macOS, since MoltenVK refuses a biplanar 4:2:0
  IOSurface as a multi-planar image and no driver patch is carried (owner, 2026-09-26). A
  flavour the device or format cannot take falls back at derivation rather than failing the
  acquire, and the later cross-process import refuses by naming the flavour.
  [python-kernel-api; python-kernel-surface — SHIPPED #1778, #1779; macos-capability-parity
  — SHIPPED #2402, #2404]
  <!-- verify: cargo test -p streamlib-engine the_seam_refuses_to_publish_a_staging_no_frame_was_read_into -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_a_raise_inside_the_device_tensor_scope_follows_its_floors_publication_rule -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_a_texture_handle_round_trips_across_the_process_boundary -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_a_device_write_is_ordered_ahead_of_the_engines_next_gpu_read -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_the_device_tensor_strides_follow_the_surfaces_row_pitch -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests every_single_plane_format_takes_an_iosurface_backed_image -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests a_planar_format_is_refused_an_iosurface_backed_image_by_name -->
  <!-- verify: cargo test -p streamlib-consumer-rhi the_binding_takes_device_local_memory_that_is_not_host_visible -->
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
  <!-- verify: cargo test -p streamlib-engine a_device_whose_probed_type_is_not_host_cached_gets_no_host_cached_pool -->
  <!-- verify: cargo test -p streamlib-engine a_staging_registration_states_the_exporters_memory_type_index -->
  <!-- verify: cargo test -p streamlib-engine parse_texture_usages_combines_tokens_and_implies_both_copy_bits -->
  <!-- verify: cargo test -p streamlib-engine the_seam_publishes_a_staged_edit_back_into_the_pooled_backing -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_compute_kernel.py::test_a_texture_backed_surfaces_pixels_reach_the_cpu_with_numpy_alone -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_compute_kernel.py::test_a_raise_inside_the_texture_cpu_door_propagates_and_follows_its_floors_publication_rule -->
  <!-- verify: cargo test -p streamlib-python-wheel a_checked_out_texture_reads_its_iosurface_rows_and_releases_on_its_shared_event -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-24-texture-backed-cpu-reach.md -->
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
  <!-- verify: cargo test -p streamlib-engine a_host_mapping_takes_the_imported_tier_when_the_device_allows_it -->
  <!-- verify: cargo test -p streamlib-engine a_refused_import_falls_back_to_host_cached_staging_and_says_why -->
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
  <!-- verify: cargo test -p streamlib-engine the_yuyv_pass_writes_every_pixel_of_the_target_range -->
  <!-- verify: cargo test -p streamlib-engine an_owned_color_converter_shares_no_kernel_with_the_cached_one -->
- **DECIDED** — A consumer of a *published* frame barriers it out of whatever layout it
  was observed in into the layout its own descriptor declares, and republishes that layout
  on the registration; only `UNDEFINED` is refused, because only `UNDEFINED` is free to
  discard the picture. Hard-coding the arriving layout is wrong: a frame
  published from the app process arrives in `SHADER_READ_ONLY_OPTIMAL` and one published
  from a helper arrives in `GENERAL`, so a sink that admits only the first silently
  consumes nothing from any graph with a Python processor in it — and sampling in place
  instead is undefined, since the sampled descriptor states one layout whatever the image
  is in. [virtual-camera-sink — SHIPPED #2198]
  <!-- verify: cargo test -p streamlib-media-builtins every_published_layout_is_admitted_and_only_an_unpublished_one_is_refused -->
  <!-- verify: cargo test -p streamlib-media-builtins both_doors_barrier_into_the_layout_a_sampled_descriptor_declares -->
- **OPEN** — Serialising a `TextureRegistration`'s layout cell. The engine's contract is
  read-then-barrier-then-update with no lock held across the submit, which the encoder, the
  present compositor and the escalate path all follow; two app-process consumers fanned
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
  <!-- verify: cargo test -p streamlib-engine a_dispatch_reads_one_surface_and_writes_another -->
  <!-- verify: cargo test -p streamlib-engine a_name_supplied_twice_is_refused -->
- **DECIDED** — Compute, graphics, ray tracing, and CPU readback are always-present
  capabilities of `GpuContext`, reached the same way by every caller. No kernel capability
  can be absent at runtime, and no application glue supplies one.
  [python-kernel-api; python-kernel-surface — SHIPPED #1773, #1774, #1777; amended by one-runtime-per-machine: accelerators are optional]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-22-python-kernel-surface.md -->
- **DECIDED** — GLSL is the shader source contract: Python passes GLSL text and the
  engine compiles it at kernel construction, and re-creating an identical kernel is free —
  compilation is cached under a key covering everything that changes the output (source,
  stage, entry point, target environment, compiler version), never source alone.
  Pre-compiled SPIR-V stays accepted as an escape hatch. Authoring a kernel requires no
  toolchain beyond the installed wheel, for every kernel kind. The wheel carries a C++
  GLSL compiler (shaderc / glslang). [python-kernel-api; python-kernel-surface —
  SHIPPED #1775]
  <!-- verify: cargo test -p streamlib-engine glsl_shader_source_compiler -->
  <!-- verify: cargo test -p streamlib-engine re_registering_an_identical_kernel_is_a_cache_hit -->
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
  <!-- verify: cargo test -p streamlib-engine a_batch_costs_one_submission_and_one_stall_where_separate_dispatches_cost_n -->
  <!-- verify: cargo test -p streamlib-engine a_batch_naming_one_kernel_twice_is_refused_saying_why -->
  <!-- verify: cargo test -p streamlib-engine one_surface_bound_as_two_kinds_in_one_dispatch_is_refused -->
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
  <!-- verify: cargo test -p streamlib-engine a_texture_copies_into_a_texture_and_the_source_keeps_its_layout_and_pixels -->
  <!-- verify: cargo test -p streamlib-engine an_rgba_pool_frame_lands_in_an_rgba8_unorm_texture -->
  <!-- verify: cargo test -p streamlib-engine a_retired_frame_is_refused_by_name -->
  <!-- verify: cargo test -p streamlib-engine a_pool_frame_its_producer_still_owns_is_refused_as_taking_no_write_back -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_surface_copy.py::test_a_frame_lands_in_a_kernel_input_texture_with_no_array_library -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_surface_copy.py::test_the_landing_check_fails_when_the_copy_is_skipped -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-09-26-portable-gpu-interop.md -->
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
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_tensor_storage_buffer.py::test_a_tensor_written_through_torch_is_read_by_another_process -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_tensor_storage_buffer.py::test_an_odd_shaped_tensor_round_trips -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_tensor_storage_buffer.py::test_a_tensor_acquired_after_a_window_opens_round_trips -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_tensor_storage_buffer.py::test_a_kernel_writes_a_tensor_bound_by_surface_id_and_a_draw_reads_one -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_tensor_storage_buffer.py::test_mlx_reads_the_tensor_torch_mps_wrote_in_another_process -->
  <!-- verify: cargo test -p streamlib-engine a_storage_buffer_registration_without_its_layout_is_refused_by_name -->
  <!-- verify: cargo test -p streamlib-engine a_later_pass_in_a_batch_reads_the_tensor_an_earlier_pass_wrote -->
  <!-- verify: cargo test -p streamlib-engine a_retired_tensor_frame_id_is_refused_and_writes_nothing -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-09-29-engine-steps.md -->
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
  and at the cap refuses by name so the producer drops its own frame; a descriptor change
  replaces the pool, bridge teardown releases every slot, and a retired id is refused as
  recycled at resolve. `ProcessorOutputTextureRing` keeps its spelling and asks this pool for
  every frame; `ModelInputTensorKernel` asks its tensor side directly. Handing out a reused
  slot is bookkeeping only — a lease scan and a generation mint, entering neither the
  escalate gate nor a device-idle wait; only growth allocates under the gate. Nothing
  therefore drains a slot's earlier readers before its next write, so every write into a slot
  orders itself: a draw's colour-target barrier sources `ALL_COMMANDS` / `MEMORY_WRITE`, as
  the compute path's entry scope does, so it waits on the one queue for a display's in-flight
  compose of that slot. Owner, 2026-09-22, over a wheel ring asking a "still held?" op per
  rotation, which would be a second system deciding slot reuse.
  [engine-steps — SHIPPED #2427, #2429, #2503, #2546; amended by
  tatolab-names: on the Python surface the two acquire methods become
  `…_from_node_output_pool` and the ring `NodeOutputTextureRing` (§Packages, no public Python
  name says "processor")]
  <!-- verify: cargo test -p streamlib-engine every_slot_held_grows_the_pool_until_its_cap_then_refuses_by_name -->
  <!-- verify: cargo test -p streamlib-engine a_slot_a_consumer_has_checked_out_is_skipped_until_released -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests a_tensor_storage_buffer_pool_never_rewrites_a_tensor_a_consumer_holds -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests a_reused_processor_output_slot_skips_the_escalate_gate_and_growth_enters_it -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests offscreen_draw_into_a_slot_the_display_is_still_composing_is_ordered_after_the_compose -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_texture_ring_producer.py::test_a_frame_a_consumer_holds_keeps_its_pixels_while_the_producer_produces -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_texture_ring_producer.py::test_the_same_schedule_with_no_claim_recycles_the_first_frame -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_tensor_storage_buffer.py::test_a_tensor_a_consumer_holds_is_never_rewritten -->
- **DECIDED** — A pixel effect is written as a shader body: `GlslPixelEffect`, pure wheel
  Python over `create_compute_kernel`, the processor output texture ring,
  `copy_surface_to_surface` and dispatch, with no engine, wire or stub change. The user writes
  one GLSL function, `vec4 effect(vec4 source, ivec2 at)`;
  `GlslPixelEffect.compile(gpu_full_access, effect_glsl=, dials=)` in `setup()` builds an
  ordinary compute kernel around it, and `apply_to_frame(gpu_limited_access, frame, dials=)`
  in `process()` lands the frame with the engine copy, dispatches, and returns the output bag
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
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_glsl_pixel_effect.py::test_an_invert_effect_with_a_strength_dial_outputs_255_minus_the_source -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_glsl_pixel_effect.py::test_the_invert_check_fails_for_an_identity_effect -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_glsl_pixel_effect.py::test_every_dial_type_reaches_the_shader_at_its_std430_offset -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_glsl_pixel_effect.py::test_a_compiler_diagnostic_names_the_line_of_the_users_body -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_glsl_pixel_effect_refusals.py -->
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
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_model_input_tensor_kernel.py::test_every_layout_and_dtype_of_a_fit_matches_the_torch_reference -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_model_input_tensor_kernel.py::test_the_comparison_fails_for_a_kernel_compiled_with_the_wrong_mean -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_model_input_tensor_kernel.py::test_a_pad_bottom_right_tensor_follows_its_frame_across_an_extent_change -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_model_input_tensor_kernel_refusals.py -->
- **OPEN** — Everything else, including the two graphics capabilities no language can
  render: depth attachments — Rust constructs a depth-testing pipeline that Python cannot
  name, and no pass in either language renders against one — and MSAA, refused for every
  caller in every language with the pipeline hardcoded to a single sample. Both are
  unbuilt engine capabilities rather than Python-reach gaps; equalising the construction
  surface with no pass to render against would buy nothing.
- **DECIDED** — Accelerators are optional: a stream that needs no GPU runs on a machine without
  one. [one-runtime-per-machine]
- **OPEN** — What optional accelerators mean for the engine: how the runtime starts with no
  device, and what a stream that needs one gets on a machine without one. Direction (review,
  not decided): a runtime property, never a package — an extra cannot change the runtime's
  native module, and a second engine-linking distribution would reverse "no process ever holds
  two streamlib engines" and the deleted bridge traits. The smallest shape is the owner's
  2026-09-26 preference: one runtime artifact; device selection takes the first device meeting
  the floor, with a bundled software Vulkan last; absent tiers refused by name; `start()` no
  longer initialising the GPU unconditionally. Exactly one `GpuContext` per runtime, shared by
  every stream. Undecided: whether the adapter crates move anywhere, and what the
  software-Vulkan bundle costs the Linux wheel. [one-runtime-per-machine]

## Media I/O — camera, display, audio, codecs — IN-FLIGHT (→ runtime-hosting: Apple permissions through Tatolab.app; jpeg-after-the-robotics-cut)

- **DECIDED** — First-party camera, display, and audio are native built-in processors
  in the engine tree, statically linked into the wheel — pre-built named blocks
  instantiated and configured from Python (`stream.add(CameraSource)`), whose per-frame
  paths never enter the interpreter. Lag-by-design ends: built-ins ship inside the
  wheel, current by construction. This names the shipped set, not a rule: a further
  first-party capability is a built-in only under the criterion in §Packages & extension
  model, and is otherwise an extension wheel.
  [importable-python-library — SHIPPED #1709; extension-model]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_native_builtin_blocks.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_launch.py::test_a_native_block_added_without_config_reaches_a_running_graph -->
- **DECIDED** — A virtual camera is a fifth device class and a built-in under criterion
  (c): `VirtualCameraSink`, in the media built-ins crate, Linux only — with no Apple port,
  stated rather than deferred: a macOS virtual camera is a CoreMediaIO Camera Extension
  inside a bundled, entitled, notarised app, which the floor rules out under any
  justification, and the DAL plug-in stopped loading in macOS 14.1; on macOS the runtime
  refuses a graph naming it at load, naming the platform, and `streamlib enable-virtual-camera`
  refuses by name. As
  many instances as the graph adds — the display's rule. Each instance is one camera that exists only
  while its processor runs: created at `setup()`, removed at `teardown()`, a camera plugged
  in and pulled out from every other application's point of view, whose frames are
  whatever the graph writes. It takes video on an input `video` declared `newest`, the
  display's shape. Config: `name`, the camera's name in every picker, defaulting to
  `StreamLib Camera` followed by a four-character id derived from the app's entry
  directory and the instance's display name — distinct across instances and apps, the
  same on every run of the same app, so unnamed cameras never collide and a device left
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
  macos-capability-parity]
  <!-- verify: cargo test -p streamlib-media-builtins virtual_camera_sink -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_virtual_camera_sink.py -->
  <!-- verify: cargo test -p streamlib-engine a_pipewire_camera_node_offers_a_modifier_and_a_shared_memory_sibling -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli.py::test_enable_virtual_camera_refuses_by_name_off_linux -->
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
  primitives third parties get — DMA-BUF / OPAQUE_FD / IOSurface import-export, present
  target, audio clock, color resolution, and the audio device, video device and video codec
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
  `VideoFrame` bag on port `video` whose `surface_id` names a pooled buffer, colour as the
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
  <!-- verify: cargo test -p streamlib-engine --lib the_video_chain_is_probed_once_and_hands_back_the_same_backend_every_time -->
  <!-- verify: cargo test -p streamlib-engine --lib the_linux_video_chain_offers_v4l2_before_falling_through_to_the_refusing_backend -->
  <!-- verify: cargo test -p streamlib-engine --lib the_macos_video_chain_offers_avfoundation_before_falling_through_to_the_refusing_backend -->
  <!-- verify: cargo test -p streamlib-engine --lib a_named_camera_that_is_not_attached_is_refused_listing_the_ones_that_are -->
  <!-- verify: cargo test -p streamlib-engine --lib a_named_device_is_refused_naming_it_and_the_way_to_run_without_one -->
  <!-- verify: cargo test -p streamlib-engine --lib the_largest_format_within_the_cap_is_chosen -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib a_captured_frame_is_published_as_the_bag_a_camera_has_always_published -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --test avfoundation_camera_captures_through_the_video_device_seam -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_camera_source.py::test_a_device_that_was_named_and_cannot_be_opened_refuses_at_setup -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-09-29-macos-platform-floor.md -->
- **DECIDED** — A camera frame carries one capture instant on both of its stamps. The
  seam's hand-off carries `capture_timestamp_ns`, resolved under one rule every arm shares
  (`VideoCaptureInstantResolver`), and `CameraSource` assigns it to the frame's own
  `timestamp_ns` **and** passes the same value to `write_with_timestamp` — never the implicit
  write, whose `MediaClock::now()` stamps publication, and never the call swap alone, which
  sets only the envelope. The encoder reads the payload's stamp, `Mp4Sink` the
  envelope's; both name the instant of capture on both floors. A device stamp is trusted only
  when it is on the machine's monotonic clock and non-zero — V4L2's dequeued-buffer stamp with
  its timestamp flags, AVFoundation's sample presentation stamp converted from the session's
  synchronisation clock to host time; anything else falls back to the dequeue instant and is
  reported once per device, naming it. **A stamp ahead of the dequeue instant is clamped to it
  and counted**, the first reported: no real capture happens in the future, and trusting one
  silently ships a frame-period of audio-video skew. Owner, 2026-09-19, on evidence that
  `vivid` sets the monotonic flag honestly and still stamps roughly nine tenths of a frame
  period ahead; what a real UVC device reports is unmeasured, and the clamp is what makes that
  gap safe to carry. [macos-platform-floor — SHIPPED #2359]
  <!-- verify: cargo test -p streamlib-engine --lib a_monotonic_stamp_from_the_future_is_clamped_to_dequeue_and_counted -->
  <!-- verify: cargo test -p streamlib-engine --lib a_stamp_off_the_monotonic_clock_falls_back_to_the_dequeue_instant -->
  <!-- verify: cargo test -p streamlib-engine --lib a_zero_stamp_falls_back_to_the_dequeue_instant_and_is_not_counted_as_clamped -->
  <!-- verify: cargo test -p streamlib-engine --lib a_device_that_never_stamps_usably_is_reported_once_however_many_frames_it_sends -->
  <!-- verify: cargo test -p streamlib-engine --lib a_buffer_flagged_monotonic_carries_its_stamp_in_nanoseconds -->
  <!-- verify: cargo test -p streamlib-media-builtins --features hardware-tests --test camera_to_display_window_on_the_macos_window_server -->
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
  <!-- verify: cargo test -p streamlib-engine --lib a_pending_request_is_made_once_and_never_waited_on -->
  <!-- verify: cargo test -p streamlib-engine --lib a_denial_names_the_responsible_application_and_the_setting_but_not_python -->
  <!-- verify: cargo test -p streamlib-engine --lib a_microphone_denial_names_the_microphone_setting_and_not_the_cameras -->
  <!-- verify: cargo test -p streamlib-engine --lib a_reminder_due_before_any_answer_warns_once_naming_the_application_and_the_setting -->
  <!-- verify: cargo test -p streamlib-engine --lib a_terminal_is_named_by_its_bundle -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --lib a_stream_waiting_on_the_user_binds_no_unit_and_delivers_once_allowed -->
- **DECIDED** — Windowing: the engine owns the process's one event pump and mints
  windows on request; a window-owning processor registers with it and keeps every
  window policy decision — title, extent, what a resize means, when to redraw, what
  closing does. winit permits one event loop per process, so the loop is owned once,
  above every processor that wants a window, and N window-owning processors coexist
  in one process: the built-ins crate does not depend on winit, so the engine holds
  the only construction site there is. Each window's owner renders on its own thread,
  never the pump's, so windows are not serialised behind one render loop — a claim
  about the render loop, not the device, since two windows still share one `VkDevice`
  and its queues. The raw-window-handle seam remains the internal boundary — the
  engine mints the present target from the raw handle and owns every swapchain and
  acquire detail,
  plus the platform main-thread event loop where the OS demands it. On Linux the pump runs
  on its own thread; on Apple it is built on the process's first thread when the runtime
  starts and driven there while `rt.run()` blocks with the GIL released (in the importable
  arrangement that thread belongs to the user's script). A runtime started off the first
  thread, or a process whose first thread another `NSApplication` loop already drives, is
  refused a pump by name, and every caller gets the same answer. On Apple the present target
  is minted from a `CAMetalLayer` the pump adds as a sublayer of the window's content view at
  creation, since winit hands out a raw window handle and AppKit a view only on the first
  thread; a present target opens at the size the pump read at creation, so opening one under
  the escalate gate never waits on that thread; and a registration hands its window back to
  the pump to close, AppKit closing a window only there. Cmd+Q from the engine's application
  menu requests the same shutdown Ctrl-C does. A window's requested size is in the desktop's
  logical pixels, so `DisplayWindow`'s `width`/`height` and a processor-owned window's
  request mean the same apparent size on a 1x and a 2x display while the swapchain renders at
  full density — owner, 2026-09-21, while shipping #2357, on a 640×360 window showing at
  320×180 on a Retina Mac. A processor that cannot get a window drains and discards, so
  upstream still sees a live consumer.
  [importable-python-library — SHIPPED #1707; shared-window-event-pump — SHIPPED #1734;
  macos-platform-floor — SHIPPED #2357]
  <!-- verify: cargo test -p streamlib-engine --test window_event_pump_serves_many_windows -->
  <!-- verify: cargo test -p streamlib-engine --lib a_request_carries_its_title_and_size_to_the_window_attributes -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --test processor_owned_window_on_the_first_thread -->
  <!-- verify: cargo test -p streamlib-media-builtins --features hardware-tests --test two_display_windows_on_the_macos_window_server -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-23-shared-window-event-pump.md -->
- **DECIDED** — Window ownership is a processor capability, not a built-in privilege:
  a processor requests a window from the engine and owns its policy; the engine mints
  it (pump registration + present target) and, for an owner whose code cannot sit in
  the app process, runs that window's native present loop itself — the owner feeds
  the loop by naming published surface ids, latest-wins: naming no frame leaves the
  last one up, and an owner's slowness never stutters its own window, which paces on
  vsync in native code always. The request seam is the same for every processor, and
  the only cross-language delta is where the loop runs: a native owner may instead
  drive its own render thread against its present target — the deadline constraint
  does not bind app-process code — while a Python processor reaches the request
  across the escalate path and feeds the engine-run loop; the per-frame naming is a
  camera-class-cadence message that fits the helper hop, and no vsync deadline ever
  crosses it. Colour is no delta either: the per-frame naming carries the frame's
  primaries, transfer and HDR sidecar in the engine's own vocabulary, so a Python
  owner renegotiates the swapchain exactly as a native one does. A window is
  requested in `setup()`, where the typestate is Full, and released at teardown or
  with its processor — never minted mid-`process()`. The
  per-frame verb accepts anything that names a published surface: the cast object
  (whose claim guarantees the id un-recycled), a kernel-output handle, or a bare
  surface id — the last with one qualifier: naming no extent is how a caller says it
  knows nothing else about the surface, so a bare id, or a cast type declaring none,
  names a texture-backed surface only. A buffer-backed frame named that way does not
  draw — the window keeps what it last had and the engine says so once per pool slot
  rather than raising — so a camera or test pattern, which publishes buffer-backed
  frames, is named with a cast object carrying its extent. The pump's two events
  reach the owner as coalesced state polled off the window object, never a callback
  across the hop; an owner that reads neither
  gets the defaults — resize just works (the engine owns every swapchain detail),
  and an unread close-request closes the window, after which the per-frame verb is
  a no-op and the window reports closed: a user gesture never takes down a pipeline.
  A refused request — no display server, a dead pump — raises at `setup()`, never
  degrading silently: the built-in's drain-and-discard exists to keep upstream
  seeing a live consumer, and a processor-owned window has no port of its own to
  protect. The window is a processor resource, invisible to `graph` and `tap`
  topology. The present compositor stays engine-internal — no cross-process spelling
  and no Python name; at this capability surface, present-class means windows. One
  present-loop machinery serves the built-in display and every processor-owned
  window. The capability is Python's on both floors: on macOS a Python processor requests,
  shows, drains and closes its own window and carries the HDR sidecar through the same
  present loop, the helper's window exchange client being fd-free and shared.
  [processor-owned-windows — SHIPPED #1928, #1929, #1930; macos-capability-parity — SHIPPED
  #2407]
  <!-- verify: cargo test -p streamlib-engine --test processor_owned_window_over_the_escalate_wire -->
  <!-- verify: cargo test -p streamlib-engine --test processor_owned_window_shows_named_surfaces -->
  <!-- verify: cargo test -p streamlib-engine --test processor_owned_window_refused_without_a_display_server -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_owned_window.py::test_all_three_ways_of_naming_a_published_surface_reach_the_window -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_owned_window.py::test_a_users_close_leaves_the_pipeline_running_and_the_owner_informed -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_owned_window.py::test_a_frame_that_names_its_colour_reaches_the_window_with_its_hdr_sidecar -->
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
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --lib a_corevideo_420v_surface_imported_as_a_storage_buffer_converts_like_a_copy_and_follows_cpu_writes -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --lib a_storage_buffer_clone_keeps_the_imported_surface_alive_until_it_drops -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --lib nv12_starting_past_byte_0_converts_identically_to_the_same_bytes_at_byte_0 -->
- **DECIDED** — MJPEG capture on Linux (unbuilt). `CameraSource`'s V4L2 arm reads a camera's
  MJPEG modes beside NV12 and YUYV. The frame is decoded inside the capture path by the
  in-tree JPEG decoder — `sdk/vulkan-jpeg`, which moves into the engine with the change that
  builds this and stays a workspace member until then — and lands in the pooled `Rgba32`
  pixel buffer every capture lands in, so no JPEG reaches a port and no block is involved.
  The decoder reads the 4:2:2 frames cameras send, not 4:2:0 alone. Which mode the arm
  picks when a camera offers several is ticket-level. Acceptance is the rig's capture card, a
  real UVC device: its MJPEG mode, opened directly, live at 1920×1080 and 30 fps through the
  engine's MJPEG path; the mode rule is proven by unit tests over a USB 2.0 camera's mode list,
  and no USB 2.0 webcam is sought (owner, 2026-10-05). The Apple arm is unchanged. Owner,
  2026-10-04. [jpeg-after-the-robotics-cut]
- **DECIDED** — Python-authored media processors (vendor or user) run in their own
  helper process like every other Python processor and are supported where deadlines
  allow: camera-class sources and block-level audio fit within the helper hop's
  budget; vsync-paced present loops and device audio callbacks stay native, always —
  a deadline the cross-process hop cannot meet, not a GIL argument.
  [importable-python-library; helper-process-placement-only — SHIPPED #1714]
- **DECIDED** — One clock on the data plane: every timestamp a processor stamps, reads,
  or compares — frames, bags, audio ticks, `ctx.time` — is the machine's monotonic clock
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
  Wall clock is permitted on exactly three observability surfaces and nowhere else: log
  record `host_ts` and `source_ts`, and log file naming — their job is correlating with
  the outside world, which monotonic time cannot do. A wall-clock value never enters the
  data plane and is never compared against a media timestamp; a fourth surface is a plan
  change, not a judgement call. The list is mechanically enforced, with no per-line pragma
  and no opt-out attribute: the permitted surfaces are a closed set in the gate, so a fourth
  is a source change that surfaces in review rather than a line quietly appended, and an
  entry whose file stops reading a wall clock is a licence the gate makes you hand back. The
  allowlist is per-file, which is why a data-plane file never joins it — a machine-global
  unique name comes from the engine's unique-name primitive, never from reading a clock.
  [one-monotonic-clock — SHIPPED #1725, #1726, #1727, #1728; macos-capability-parity —
  SHIPPED #2408]
  <!-- verify: cargo test -p streamlib-engine --lib now_lands_in_the_kernel_monotonic_domain -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_clock_and_log.py::test_monotonic_now_ns_reads_the_engine_media_clock -->
  <!-- verify: cargo test -p streamlib-python-wheel --lib python_logging::tests::a_wheel_stamp_and_an_engine_stamp_taken_back_to_back_differ_by_microseconds -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_capability_contexts.py::test_ctx_time_is_the_engine_media_clock_in_nanoseconds -->
  <!-- verify: cargo run -p xtask -- check-clock-usage -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-13-one-monotonic-clock.md -->
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
  <!-- verify: cargo test -p streamlib-engine --lib the_chain_is_probed_once_and_hands_back_the_same_backend_every_time -->
  <!-- verify: cargo test -p streamlib-engine --lib the_macos_chain_offers_coreaudio_before_falling_through_to_null -->
  <!-- verify: cargo test -p streamlib-engine --lib the_walk_demotes_past_every_arm_that_declines_in_the_order_it_was_given -->
  <!-- verify: cargo test -p streamlib-engine --lib the_linux_chain_offers_pipewire_then_alsa_before_falling_through_to_null -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_microphone_source.py::test_a_device_that_was_named_and_cannot_be_opened_refuses_at_setup -->
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
  <!-- verify: cargo test -p streamlib-engine --test silent_null_arm_captures_without_ever_dying -->
  <!-- verify: cargo test -p streamlib-engine --test silent_null_arm_plays_what_it_is_given -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib a_publishing_thread_whose_device_died_comes_back_and_says_why -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib a_drain_thread_whose_device_died_comes_back_and_says_why -->
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
  <!-- verify: cargo test -p streamlib-engine --lib a_named_device_that_is_not_attached_is_refused_naming_it_and_the_ones_that_are -->
  <!-- verify: cargo test -p streamlib-engine --lib an_unnamed_stream_moves_to_a_default_that_moved -->
  <!-- verify: cargo test -p streamlib-engine --lib a_named_stream_stays_on_its_device_when_the_default_moves -->
  <!-- verify: cargo test -p streamlib-engine --lib failed_renders_reaching_the_bound_in_a_row_end_the_stream_once -->
  <!-- verify: cargo test -p streamlib-engine --lib a_month_long_converted_stream_still_stamps_exactly -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib a_stream_reporting_its_device_period_sizes_everything_from_that_period -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib a_stream_reporting_no_device_period_is_sized_from_ten_milliseconds -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --test coreaudio_arm_stamps_blocks_with_the_devices_own_timing -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --test coreaudio_arm_plays_what_it_is_given -->
- **DECIDED** — Every audio symbol binds at runtime and the wheel's `DT_NEEDED` set does
  not grow: `libpipewire-0.3.so.0` and `libasound.so.2` resolve through `libloading`,
  the pattern the DRM modifier probe already uses for `libEGL.so.1` — a library held
  beside typed function pointers, a missing library demoting to the next arm and a
  missing symbol named rather than crashing. The versioned soname is the dlopen target,
  not a stylistic echo: a machine ships `libpipewire-0.3.so.0` with no dev symlink.
  Nothing links `cpal`, `pipewire-rs`, or any pkg-config audio crate — each puts an
  audio library straight into `DT_NEEDED` and fails the portability gate by
  construction. [dlopen-audio-backend-and-audio-blocks — SHIPPED #1990, #1991]
  <!-- verify: cargo test -p streamlib-engine --lib a_missing_library_demotes_and_names_the_library_it_looked_for -->
  <!-- verify: cargo test -p streamlib-engine --lib the_loader_names_the_symbol_a_wrong_library_does_not_export -->
- **DECIDED** — SPA's header-only layer compiles into the wheel as a shim that calls
  nothing. PipeWire's pod builders and parsers are inline C with no shared object, so a
  small `cc`-compiled shim owns them and every `pw_*` entry point it needs arrives as a
  function pointer Rust filled by `dlsym` — the shim itself references no external
  symbol. This is the vendored VMA build verbatim in shape: compiled with its static and
  dynamic Vulkan function lookups both off so it calls only pointers Rust hands it, and
  adding no `DT_NEEDED` entry beyond the C++ runtime.
  [dlopen-audio-backend-and-audio-blocks — SHIPPED #1990]
  <!-- verify: cargo test -p streamlib-engine --lib the_shim_names_every_entry_point_it_expects_rust_to_resolve -->
- **DECIDED** — The headers are vendored, not taken from the build machine.
  `manylinux_2_28` carries no PipeWire development package, so a system-header build is
  not reproducible where the wheel is actually built — the same reasoning that pins the
  GLSL compiler to build-from-source rather than linking whatever sits on the builder.
  MIT-licensed PipeWire and SPA headers land under `vendor/`, untouched and
  unreformatted, and the licence obligations are met by the machinery that already
  reproduces every vendored C/C++ project's own licence text out of the tree. `LICENSE`,
  `LICENSES/` and `docs/license/` are not edited; the shim is our code and carries the
  BUSL header. [dlopen-audio-backend-and-audio-blocks — SHIPPED #1990]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_third_party_notices.py -->
- **DECIDED** — The portability gate is the design's pass/fail: the shipped
  `_engine.abi3.so` names five host libraries, every one on the permitted-host-library list,
  and audio adds none. No name is added to the permitted-host-library list — an audio
  library appearing there is the failure this design exists to prevent, not a fix for
  it. [dlopen-audio-backend-and-audio-blocks — SHIPPED #1990, #1991]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py::test_the_native_extension_links_nothing_the_host_may_not_supply -->
- **DECIDED** — A backend device paces the audio path: capture and playback callbacks
  are the cadence source, and a block's timestamp derives from the backend's own
  timing (status minus reported delay) in the machine monotonic epoch — never from a
  free-running timer. The timerfd `AudioClock` remains the SDK clock primitive and
  paces deviceless graphs only (null backend, tests), so it starts when something needs
  it and a device-paced graph never starts it at all — which is what makes "exactly one
  cadence source" true in the tree rather than merely stated: device ticks and timer
  ticks cannot interleave if the timer is not running.
  [audio-subsystem; dlopen-audio-backend-and-audio-blocks — SHIPPED #1989, #1990, #1991]
  <!-- verify: cargo test -p streamlib-engine --test audio_clock_paces_only_what_needs_it -->
  <!-- verify: cargo test -p streamlib-engine --test pipewire_arm_stamps_blocks_with_the_devices_own_timing -->
  <!-- verify: cargo test -p streamlib-engine --test alsa_arm_stamps_blocks_with_the_devices_own_timing -->
- **DECIDED** — A/V sync is block-level join-by-timestamp on the one monotonic clock:
  an `AudioBlock` carries its first sample's timestamp, rate, and sample count, so any
  sample's instant is derivable and audio joins camera frames by timestamp alone. No
  sample-accurate cross-modal machinery exists.
  [audio-subsystem; dlopen-audio-backend-and-audio-blocks — SHIPPED #1988, #1990, #1991]
  <!-- verify: cargo test -p streamlib-media-builtins --lib a_published_block_carries_the_streams_format_and_the_devices_timestamp -->
- **DECIDED** — The device stamps the block and the engine never re-stamps it. A
  capture block's timestamp is the backend's own timing for its first sample —
  `pw_time`-derived status minus reported delay on the PipeWire arm,
  `snd_pcm_status_get_htstamp` on the ALSA arm with the monotonic timestamp type set
  explicitly so the stamp cannot arrive on `CLOCK_REALTIME`, and the input cycle's
  `mHostTime` (the `mach_absolute_time` domain) minus the device and stream latency on the
  CoreAudio arm — and it is published
  through the timestamped write, never the implicit one, whose `MediaClock::now()`
  would stamp the moment of publication rather than the instant of capture. Both the
  bag field and the frame header therefore carry the same device-derived value, in the
  same epoch a video frame's timestamp carries, which is the whole of block-level A/V
  sync: joining audio to camera frames is subtracting two integers.
  [dlopen-audio-backend-and-audio-blocks — SHIPPED #1990, #1991; macos-capability-parity —
  SHIPPED #2411]
  <!-- verify: cargo test -p streamlib-engine --lib the_devices_latency_moves_the_stamp_back_by_that_many_frames -->
  <!-- verify: cargo test -p streamlib-engine --lib a_blocks_stamp_sits_one_period_before_a_status_reporting_one_unread_period -->
  <!-- verify: cargo test -p streamlib-engine --lib a_stamp_from_the_wrong_clock_is_refused_and_a_monotonic_one_is_not -->
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
  <!-- verify: cargo test -p streamlib-media-builtins --lib the_device_callback_hands_off_into_the_ring_and_the_loss_lands_there -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib the_device_callback_takes_what_is_queued_and_never_waits_for_the_graph -->
- **DECIDED** — The null backend runs the graph and produces silence: under it
  `MicrophoneSource` publishes silent blocks and `SpeakerSink` discards what it
  receives, both paced by the timerfd clock — so a pipeline authored on a workstation
  runs unchanged in a headless container and a test needs no audio hardware. A device
  that was *named* and cannot be opened is the opposite case and raises at `setup()`,
  the way `CameraSource` raises on a missing `/dev/video*`: a machine with no audio is a
  supported environment, a wrong device id is a wiring error.
  [dlopen-audio-backend-and-audio-blocks — SHIPPED #1989]
  <!-- verify: cargo test -p streamlib-engine --lib every_block_carries_a_full_quantum_of_silence -->
  <!-- verify: cargo test -p streamlib-engine --lib a_named_device_is_refused_by_name_rather_than_opened_as_something_else -->
- **DECIDED** — The audio data model is the `AudioBlock` bag: samples ride the link
  inline as msgpack bin, CPU-resident, interleaved, with sample rate, channel count,
  dtype, and first-sample timestamp beside them. It is the wire contract and the field
  names are the contract — the same shape `VideoFrame` states for video: an optional
  cast over a self-describing msgpack named map, declared on no port, registered
  nowhere, ignoring keys it does not read. The keys are `samples`, `sample_rate`,
  `channels`, `sample_count`, `dtype`, `first_sample_timestamp_ns`. `dtype` is metadata
  with `f32` the default and `i16` legal, and `samples` is little-endian — a wire
  statement rather than an assumption, since it is the property a bag decoded by a tap,
  a CLI, or another language depends on. The sample count counts per-channel samples —
  an interleaved block of `channels` channels carries `sample_count × channels`
  scalars — so duration and the next block's expected timestamp derive from count and
  rate alone. Audio touches no surface machinery — no surface ids, no claims, no
  lifetime contract, no `exchange`.
  [audio-subsystem; dlopen-audio-backend-and-audio-blocks — SHIPPED #1988]
  <!-- verify: cargo test -p streamlib-media-builtins --lib audio_block_msgpack_wire_carries_the_samples_as_a_binary_payload -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib audio_block_cast_ignores_unknown_keys -->
- **DECIDED** — `samples` is msgpack `bin`, so the field is a byte buffer and `dtype`
  says how to read it — never a typed vector. A `Vec<f32>` field would serialize as a
  msgpack **array** — five bytes per sample, and a shape Python's own `bytes` → `bin`
  path does not agree with. So the field carries interleaved little-endian scalars as
  bytes, and one field spelling serves `f32` and `i16` alike. The wire-key test asserts
  the binary type for both, which is the one test that can catch an array-for-`bin`
  mistake. [dlopen-audio-backend-and-audio-blocks — SHIPPED #1988]
  <!-- verify: cargo test -p streamlib-media-builtins --lib an_i16_block_carries_its_samples_as_a_binary_payload_too -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_audio_block_cast.py::test_the_payload_crosses_the_wire_as_bytes -->
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
  <!-- verify: pytest sdk/tatolab-stream/tests/test_audio_block_cast.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_audio_block_cast.py -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_audio_block_cast.py::test_an_audio_block_takes_no_surface_and_holds_no_claim -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_audio_block_cast.py::test_the_numpy_types_are_spelled_little_endian_at_the_source -->
- **DECIDED** — "Zero-copy" is a claim about the cast, and is stated as exactly that.
  Between shared memory and `process()` the payload is copied four times — out of the
  iceoryx2 sample, a header-strip memmove, the msgpack decode into an owned value, and
  into a Python `bytes` — and audio removes none of them; they are the helper hop every
  bag pays. What the cast guarantees is that it adds no fifth: the numpy array is a view
  over that `bytes`, and `torch.from_numpy` over it is a view again. At audio's sizes
  this is the right trade and the reason audio touches no surface machinery at all — a
  512-sample stereo `f32` block is 4 096 bytes against a 16 MiB per-link ceiling for a
  helper-placed processor. No doc, test name, or log line may describe the path as
  zero-copy from the device.
  [dlopen-audio-backend-and-audio-blocks — SHIPPED #1988]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_audio_block_cast.py::test_the_samples_are_a_numpy_view_over_the_bag_bytes -->
- **DECIDED** — The wheel's own test harness decodes a bag carrying bytes: the collector
  `await_bag` decodes through `rmpv`, the way the tap path does, so a `bin` payload in any
  bag — audio's or one a Python processor writes — reaches a test intact.
  [dlopen-audio-backend-and-audio-blocks — SHIPPED #1988]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_audio_block_cast.py::test_a_block_read_off_the_wire_casts_to_an_audio_block -->
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
  <!-- verify: pytest sdk/tatolab-stream/tests/test_node_declaration.py::test_an_audio_input_declares_its_window_contract -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_node_declaration.py::test_an_omitted_hop_defaults_to_the_window_size -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_node_declaration.py::test_an_output_port_takes_no_window_contract -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_node_declaration.py::test_an_omitted_channel_count_follows_the_source -->
  <!-- verify: cargo test -p streamlib-engine --test attribute_macro_test the_descriptor_carries_the_window_contract_its_port_declared -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::a_contract_declaring_no_channels_emits_the_sources_own_count -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::a_channel_free_contract_still_resamples_to_the_rate_it_declared -->
- **DECIDED** — The contract is all-or-nothing and every way of getting it wrong is
  refused by name, at the earliest seam that can see it. There is no partial form — a
  half-declared contract leaves the stage guessing at exactly the values a model asserts
  on — `channels` excepted, whose absence is itself a stated value. Refused at declaration
  in both languages: any other missing field, an unknown field, an
  unknown `dtype`, a hop above `window_size` (which would silently discard samples between
  windows), any numeric field at zero or below — a *declared* `channels` included — a
  second contract on one port, and a contract on an output. A window contract requires `delivery_profile = "ordered"` and is
  refused beside a skipping profile naming both knobs — `newest` passes over bags by
  design, so an accumulator needing contiguous samples would flush on nearly every read
  and, for a window wider than one device quantum, might never emit at all. Refused at
  wire time: a second inbound link into a windowed port, naming the port and both links —
  fan-in legally interleaves N producers' blocks in one mailbox, and two sample streams
  interleaved into one accumulator is plausible-looking wrong audio. Refused at the stage:
  an N→M channel pair with neither side 1, naming both counts, because the source count
  arrives with the bags and declaration cannot see it — a refusal that applies only to a
  *declared* count, there being nothing to convert to without one. Channel conversion runs
  both directions by fixed rule — N→1 averages, 1→N duplicates — since the rung's flagship
  case is an up-conversion.
  [audio-port-window-contract — SHIPPED #2032, #2033; opus-mp4-recording-rung — SHIPPED
  #2123]
  <!-- verify: pytest sdk/tatolab-stream/tests/test_node_declaration.py::test_a_hop_above_the_window_size_is_refused_naming_both_numbers -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_node_declaration.py::test_a_contract_beside_a_skipping_delivery_profile_is_refused_naming_both_knobs -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_node_declaration.py::test_a_partial_contract_is_refused_naming_the_missing_fields -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_node_declaration.py::test_every_value_but_the_channel_count_is_still_required -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_node_declaration.py::test_a_declared_channel_count_of_zero_is_still_refused -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_second_inbound_link_into_a_windowed_port_is_refused_naming_the_port_and_both_links -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::a_channel_pair_with_neither_side_at_one_is_refused_naming_both_counts -->
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
  re-encodes each emitted window as an ordinary `AudioBlock` bag, so `read(into=AudioBlock)`
  and Rust's `read::<AudioBlock>` work unchanged. A bag the stage cannot read is refused by
  name at the read — an unknown `dtype`, a payload whose length is not
  `sample_count × channels × itemsize`, a bag with no `AudioBlock` keys at all — never
  reshaped into a plausible wrong answer, and the refusal names the port.
  [audio-port-window-contract — SHIPPED #2033]
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::a_48k_stereo_source_reaches_a_16k_mono_512_port_as_exact_windows_32ms_apart -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::a_bag_the_stage_cannot_read_is_refused_by_name_rather_than_reshaped -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_block_bag_wire_codec::tests::an_i16_contract_saturates_at_both_endpoints_rather_than_wrapping -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_audio_window_stage.py::test_a_helper_placed_consumer_reads_exact_windows_at_the_rate_it_declared -->
- **DECIDED** — Readiness on a windowed port means a full window, not an arrived bag.
  Windowing is N-in → M-out — one 1024-sample quantum satisfies two 512-sample windows, a
  one-second rolling window needs about forty-seven of them — so the stage owns a per-port
  accumulator between the mailbox and the reader, and a windowed port reports data only
  when a full window can be emitted. A reactive `process()` is never dispatched with
  nothing to read, in the helper loop and the app-process runner alike; the drain loop
  dispatches once per ready window, so one 1024-sample quantum against a 512/512 contract
  dispatches twice and a ready window never sits latent waiting for the next bag. A stream
  that simply stops leaves under one window of samples parked in the accumulator, delivered
  to nothing — designed, not a defect: an exact-size contract has no partial form to hand
  over. [audio-port-window-contract — SHIPPED #2033]
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::one_1024_sample_quantum_against_a_512_512_contract_yields_exactly_two_windows -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::the_readiness_floor_never_claims_a_window_the_read_cannot_then_produce -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::a_stream_that_stops_mid_window_hands_over_nothing_rather_than_a_short_block -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_audio_window_stage.py::test_a_hop_below_the_window_rolls_at_the_hops_cadence_not_the_windows -->
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
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_accumulator::stamp_arithmetic_tests::a_frame_index_past_a_u64_multiplys_reach_is_still_stamped_exactly -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::the_first_window_carries_the_anchor_stamp_rather_than_one_a_group_delay_later -->
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
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::the_first_window_after_a_gap_carries_no_energy_from_before_it -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::no_window_spans_a_gap_in_the_source_stream -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::a_stamp_jittering_inside_half_a_quantum_does_not_flush_the_run -->
- **DECIDED** — Counting is unchanged and the accumulator is not a second drop site. Bags
  stay in the counted mailbox until `read` consumes them into the stage; the accumulator
  holds only the already-consumed resampled remainder, under one window's worth, and never
  evicts. Readiness is computed jointly — queued bags' sample counts plus the remainder —
  never by draining the mailbox at `has_data`, because an eager drain would starve the
  per-link counters exactly where loss happens and grow the accumulator unboundedly under
  a stalled consumer. That forces the depth question open: the profile's depth is a floor
  no contract undercuts, and the engine sizes a windowed port's mailbox up from its
  contract (`ceil(window / quantum) + margin`) — still engine-chosen, still not authorable;
  the contract is a declaration, not a depth dial. Overflow past that depth is a counted
  mailbox eviction, same counter, same `graph` surface. A discontinuity flush discards the
  remainder — under one window of samples — and that discard is counted: the samples it
  threw away are reported on the port's link beside its dropped bags and logged with the
  port and the sample count, so a bag evicted at a windowed port costs its own samples plus
  the counted flush of the remainder behind it, and no part of the loss is silent. The
  iceoryx2 ring in front of a windowed port is engine-sized to a fixed cap well above a
  profile's depth, never to the contract's own depth, and a windowed port connected live onto
  a channel created smaller than that cap is refused by name.
  Three readings the build settled. **The cap is 64 bags**, returned by the one
  creation-depth function whenever any destination of the channel being created is windowed
  and 16 otherwise; the windowed port's subscriber ring is that 64, its mailbox depth stays
  sized from its contract, and neither is fed to the other. **The live refusal reads the
  held factory** — a windowed destination wired onto a channel whose creation depth is below
  64 is refused at wire time naming the port, the link, both depths and the fix of
  connecting the windowed consumer before the channel's other links, and it reads a channel
  only a tap or a lagging helper still holds as readily as a busy one. **A flush counts the
  samples no reader had received**, in per-channel samples at the port's declared rate: the
  remainder past the last emitted window's already-delivered overlap, plus the staged source
  frames scaled by the rate ratio and rounded down. Subtracting the overlap is what makes
  the count mean what the entry says — under a rolling hop the front of the remainder is
  samples the consumer already holds, and counting them would report loss for audio that was
  delivered. `graph` renders `discarded_samples_by_link` beside `dropped_bags_by_link`, only
  for links into windowed ports, and a link into an unwindowed port carries no sample count
  rather than a zero, the way a port that declared nothing renders no `audio_window` key;
  `frames_dropped` stays a bag total, and both flush callers — the format change and the
  gap — count.
  Three discards stay uncounted, each for a reason already in this section: resampler priming
  output is filter delay rather than input; the remainder parked when a port's last link
  disconnects is the designed stop above; and a bag `accept` refuses already fails the read
  by name.
  [audio-port-window-contract — SHIPPED #2033; loss-visibility — SHIPPED #2269]
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_channel_created_for_a_windowed_destination_holds_the_windowed_ring_depth -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_windowed_consumer_connected_onto_a_running_shallower_channel_is_refused_naming_both_depths -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_gap_flush_at_a_windowed_destination_renders_its_discarded_samples_under_its_link -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_stalled_windowed_consumer_counts_what_its_sixty_four_bag_ring_overwrote -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::resolved_audio_window_contract::tests::the_profiles_depth_is_a_floor_no_contract_undercuts -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::resolved_audio_window_contract::tests::a_one_second_window_is_sized_past_the_profiles_depth_by_its_own_quanta -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::a_single_evicted_block_displaces_the_stamps_enough_to_flush -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::a_full_mailbox_that_still_cannot_make_a_window_says_so_once -->
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
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py::test_the_native_extension_links_nothing_the_host_may_not_supply -->
- **DECIDED** — A port whose target format is not knowable at declaration declares the
  sentinel `audio_window = match_device`, and the contract resolves at `setup()` — where
  the typestate is Full, the same phase in which a processor requests a window — from the
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
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::resolved_audio_window_contract::tests::a_device_stream_format_resolves_to_the_contract_that_plays_on_it -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::resolved_audio_window_contract::tests::an_unsettled_sentinel_is_refused_naming_the_resolution_mechanism -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_match_device_port_on_a_helper_placed_destination_is_refused_at_wire_time -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_match_device_contract_wires_awaiting_its_device_rather_than_refusing -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_node_declaration.py::test_the_device_matching_sentinel_is_on_no_public_surface -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_node_declaration.py::test_the_device_matching_sentinel_is_refused_at_decoration -->
- **DECIDED** — `MicrophoneSource` and `SpeakerSink` are the audio built-ins, beside
  camera and display: native built-ins in the engine tree, registered with the other
  media built-ins and surfaced to Python as marker classes beside `CameraSource`,
  configured the one way a built-in is configured
  (`stream.add(MicrophoneSource, config={"device_id": "..."})`). Both are `execution =
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
  audio-port-window-contract — SHIPPED #2034]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_microphone_source.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_speaker_sink.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_speaker_sink.py::test_a_microphone_wired_to_a_speaker_runs_and_plays_what_it_captured -->
  <!-- verify: cargo test -p streamlib-media-builtins --test speaker_sink_matches_its_device a_sixteen_kilohertz_source_reaches_whatever_this_machines_speaker_opened_at -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-29-audio-port-window-contract.md -->
- **DECIDED** — Codec blocks are native built-ins beside camera, display and the audio
  pair: `H264Encoder`, `H264Decoder`, `H265Encoder`, `H265Decoder`,
  `OpusEncoder`, `OpusDecoder`, `Mp4Sink` — instantiated and configured the one way a
  built-in is configured (`stream.add(H264Encoder)`), per-frame paths never entering an
  interpreter, serving Python and Rust apps alike. Video blocks are built on the video
  codec backend seam — Vulkan Video on Linux, VideoToolbox on Apple. There is no JPEG
  block: `JpegDecoder` is retired unbuilt, the parked nvJPEG backend and its `libnvjpeg`
  probe are deleted, and JPEG decode exists only inside camera capture. AV1 and VP9 remain ported but unexposed until a consumer demands them.
  `Mp4Sink` is a sink rather than a codec and holds no session. The seven name the shipped
  set, not a rule: the next codec follows the built-in criterion in §Packages & extension
  model.
  Encoder sessions mint lazily from the first frame's dimensions; decoder sessions
  auto-size the DPB from the stream's parameter sets. Config shape, rate-control and
  GOP knobs are ticket-level, like every other built-in's config. The four video blocks
  are one encode body and one decode body specialised by a codec identity, not four
  processors — the pair differs in an enumerant, the bag's `codec` string and a name —
  and each built-in is its own port surface, registration and identity. The layering
  wall holds at this fourth device class too: colour conversion into the codec's NV12
  input rides the engine's existing RHI colour converter — the `rgb_to_nv12` stage on
  Linux, an RGBA-image → NV12-buffer pass beside the RGBA → YUYV one on Apple — and no new
  RHI primitive was built for codecs. [codec-blocks — SHIPPED #2083, #2084, #2086;
  python-codec-block-api — SHIPPED #2105; opus-mp4-recording-rung — SHIPPED #2125, #2126,
  #2127, #2128; macos-capability-parity — SHIPPED #2412, #2413; extension-model; amended by
  jpeg-after-the-robotics-cut: `JpegDecoder` is retired with robotics out of scope, and the
  nvJPEG tree and probe go with it (owner, 2026-10-04)]
  <!-- verify: cargo test -p streamlib-media-builtins --test h264_decoder_completes_the_round_trip -->
  <!-- verify: cargo test -p streamlib-media-builtins --test h265_decoder_completes_the_round_trip -->
  <!-- verify: cargo test -p streamlib-media-builtins --test h264_encoder_publishes_the_bag_convention -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib encoded_packet_to_audio_block_decoder::tests::a_tone_survives_the_round_trip_at_one_two_and_six_channels -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::the_file_opens_with_the_brands_and_one_trak_per_link_named_after_its_producer -->
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
  `EncodedVideoFrame` is one wire on both floors, through the one platform-free Annex-B walk
  (`core/annex_b_access_unit.rs`) the arm, `Mp4Sink` and the proof rig share. What an arm
  cannot honour refuses by name at `setup()`: on VideoToolbox `effort_level`, a
  `keyframe_interval_seconds` of 0, and a decoder with no hardware support; on a Linux device
  without Vulkan Video, session construction names the missing direction instead of
  aborting. The decoders' `max_width`/`max_height` differ by floor: Linux sizes the DPB for
  the coded extent; on Apple, where CoreMedia reports only the cropped extent, a picture past
  the cap is refused. [macos-capability-parity — SHIPPED #2412, #2413; macos-platform-floor —
  SHIPPED #2374]
  <!-- verify: cargo test -p streamlib-engine --lib the_video_codec_chain_is_probed_once_and_hands_back_the_same_backend_every_time -->
  <!-- verify: cargo test -p streamlib-engine --lib the_linux_video_codec_chain_offers_vulkan_video_before_falling_through_to_the_refusing_backend -->
  <!-- verify: cargo test -p streamlib-engine --lib the_macos_video_codec_chain_offers_videotoolbox_before_falling_through_to_the_refusing_backend -->
  <!-- verify: cargo test -p streamlib-engine --lib a_refusal_names_the_platform_the_direction_and_the_stream -->
  <!-- verify: cargo test -p streamlib-engine --lib core::annex_b_access_unit -->
  <!-- verify: cargo test -p streamlib-engine --lib device_without_video_refuses_every_codec_operation_naming_its_direction -->
  <!-- verify: cargo test -p streamlib-media-builtins --test video_encoders_refuse_knobs_videotoolbox_does_not_honour_at_setup -->
  <!-- verify: cargo test -p streamlib-engine --features hardware-tests --test videotoolbox_arm_round_trips_the_psnr_references -->
- **DECIDED** — An encoded frame is an ordinary bag: the bitstream rides inline as a
  msgpack `bin` field beside the producer-written stream metadata the delivery-profile
  decision already specified (sync-point flag, group index, sequence). No pooled-buffer
  or surface-id carriage for encoded bytes unless a measured need appears. The keys are
  the wire contract, the way `AudioBlock`'s six are: `codec` (`"h264"` / `"h265"`, the
  elementary-stream identity), `bitstream` (msgpack `bin`, one Annex-B access unit),
  `is_sync_point`, and `group_index` with `sequence_index` (the producer-scoped ordering
  pair), `width` and `height` (the coded extent, before crop), and `color` (the H.273 tuple).
  Timestamp rides the frame header like every bag. A bag the decoder cannot read is
  refused by name, never reshaped — the audio wire codec's doctrine — and the ordering
  pair is an encoded-frame key that never reaches a decoded bag: a decoded frame is an
  ordinary `VideoFrame`, so nothing downstream of the decoder joins on it. The
  ring-overwrite loss §Processor model leaves OPEN is stream-corrupting for an encoded
  link until the next sync point; the discard-to-sync-point doctrine makes it
  survivable — a reader enters a stream only at a sync point and discards back to one
  after a `sequence_index` step other than exactly one, counting what it lost — and
  that OPEN stays its own decision, named here so a codec ship is never read as having
  resolved it. [codec-blocks — SHIPPED #2083, #2085]
  <!-- verify: cargo test -p streamlib-media-builtins --lib encoded_video_frame -->
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
  <!-- verify: cargo test -p streamlib-engine --lib vulkan::video::decode::decoded_picture_display_window -->
  <!-- verify: cargo test -p streamlib-media-builtins --test h265_decoder_completes_the_round_trip -->
- **DECIDED** — Proof precedes surface: camera → encode → decode → display is proven
  through the engine-owned PSNR rig (`runtime/streamlib-engine/tests/fixtures/`, Y ≥ 35 dB
  floor) before any codec block's API lands. The rig is built on the control plane's own
  observation surface — tap the encoded and decoded channels, exchange surface ids for exact
  pixel bytes — with PSNR a first-class calculation in the proof tooling, never a
  display processor writing frames to disk for a script to score. A codec block ships only with (i) a rig round-trip carrying the
  PSNR floor, run through `/verify-live`, and (ii) CI-named GPU-free tests: bitstream
  parsing, VUI/color translation, config resolution, container bytes. The rig is an
  engine-owned Rust fixture app, `cargo run -p streamlib-engine --example
  codec_roundtrip_rig` — engine-owned means CI compiles it so it cannot rot between rig
  runs while running it stays rig-only, and no test reaches into a consumer for its
  fixtures. Pairing a decoded frame to its reference is a filename contract
  (`<reference_stem>__<n>.png`) over one reference per run, not a key on the wire:
  best-match pairing was rejected as vacuous, since it satisfies the `swap-channels`
  injection by re-pairing a swapped red onto `solid_blue.png` — the exact regression
  that mode exists to catch. [codec-blocks — SHIPPED #2084, #2085, #2086]
  <!-- verify: cargo build -p streamlib-engine --example codec_roundtrip_rig -->
  <!-- verify: bash runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh -->
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
  <!-- verify: cargo test -p xtask psnr -->
  <!-- verify: cargo test -p xtask codec_proof_image_measurement -->
- **DECIDED** — The vivid drift lock is per codec, not per rig: the H.265 arm locks
  against `psnr_vivid_baseline_h265.tsv` and H.264 keeps the unsuffixed file it was
  captured under, tolerance ±0.05. Measured over exact decoded pixels, the bt601/bt709
  green rise reads 0.0965 off a 0.0029 floor. Measured, the two codecs agree to 0.0001 on every channel, so one shared file would
  have passed both arms here; the split is headroom for a codec that does reconstruct a
  saturated primary differently, so that it cannot be read as a colour regression.
  [codec-blocks — SHIPPED #2085, #2086]
  <!-- verify: bash runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh -->
- **DECIDED** — On the rig the decoder enters at `sequence_index=0` with zero frames
  discarded, and the encoder writes parameter sets on every IDR, so even a late subscriber re-enters within
  one GOP. A failed header extraction refuses rather than degrading to an empty header, and
  the minted header is checked with the engine's own NAL reader. Cam Link capture ran 18
  clean runs across release and debug at 1080p60 and 4K30 with no `DEVICE_LOST`. Decode runs
  at 3.75 ms/frame, so the decoder lag that triggered the H.265 shutdown race is absent; if
  sustained decoder lag reappears, re-run that scenario on the rig rather than assuming it
  fixed.
  [codec-blocks — SHIPPED #2084, #2085, #2086]
  <!-- verify: cargo test -p streamlib-media-builtins --test h264_decoder_completes_the_round_trip -->
- **DECIDED** — The four video blocks reach Python as marker classes beside
  `CameraSource`, through the three touchpoints a native built-in owns and no fourth — a
  processor extension owns none of them, being an ordinary Python processor class the
  wheel never has to know about (extension-model) — : one entry in
  `native_processor_marker_classes!` (a constructor-less `#[pyclass]` unit struct, a `type`
  class attribute naming the processor's own minted import path, and the `add_class`
  line), a re-export with its `__all__` entry, and a stub
  entry gated by stubtest with no allowlist. Configured the
  one way a built-in is configured — `stream.add(H265Encoder)`,
  `stream.add(H264Encoder, config={"keyframe_interval_seconds": 2})` — and resolving on both
  floors, since the codec seam made the blocks platform-free and they register
  everywhere. The wheel links all four and registers them at import, so the blocks need no
  engine registration. The stub docstring is where a
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
  stream-graph — SHIPPED #2567]
  <!-- verify: pytest sdk/tatolab-stream/tests/test_built_in_node_classes.py::test_the_built_in_class_cannot_be_instantiated -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_video_codec_blocks.py::test_the_round_trip_wires_without_an_adapter -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_video_codec_blocks.py::test_node_name_defaults_to_the_type_name -->
- **DECIDED** — `tatolab.stream.EncodedVideoFrame` is the Python cast over the encoded-frame
  bag's wire keys: pure Python beside `audio_block.py`, read with
  `ctx.inputs.read("encoded_video", into=EncodedVideoFrame)`, owing no `.pyi` entry
  because pyright checks it from source. It composes nothing surface-shaped — the
  access unit rides inline and arrives as `bytes`, so there is no surface, no claim and
  no lifetime contract, `AudioBlock`'s reasoning verbatim. Construction is the
  validation and the wire keys are the constructor keywords; the bitstream is stored
  under the Rust struct's own field name so one vocabulary serves both languages, and
  it stays off the repr. `color` is absent-means-unspecified — the H.273 rule — and
  every other key is required and refused by name when missing or mistyped: a
  `bitstream` that is not `bytes`, a `codec` naming neither elementary stream, a `bool`
  where an integer field is required, and a colour enumerant H.273 cannot place, that
  last naming this bag's own `color` key and the axis rather than a video frame's
  `color_info`. A key this cast does not read is read past, never refused. There is no
  to-bag helper and no numpy property: an access unit is opaque to everything but a
  decoder, a container or a socket, and producing an encoded bag from Python is
  spelling the keys as a bag literal and writing it with the timestamped write —
  the implicit one would stamp the moment of publication rather than the frame's own
  instant. [python-codec-block-api — SHIPPED #2106, #2114]
  <!-- verify: pytest sdk/tatolab-stream/tests/test_encoded_video_frame_cast.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_encoded_video_frame_cast.py -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_encoded_video_frame_cast.py::test_an_encoded_video_frame_takes_no_surface_and_holds_no_claim -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_encoded_video_frame_cast.py::test_the_bitstream_crosses_the_wire_as_bytes -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_encoded_video_frame_cast.py::test_the_cast_offers_no_way_back_onto_the_wire -->
- **DECIDED** — The per-block ship bar the entry above states is met for a Python
  surface by agreement, never by a second rig: below the marker the path is
  byte-identical to the one the engine-owned rig scored, so the live proof is a
  Python-authored round trip locking to the same per-codec vivid baseline within the
  same ±0.05. `e2e_fixture_psnr_vivid.sh` carries a `PIPELINE=python` arm (default
  `rust`) differing in its launch argv alone — one timeout, one environment, one
  redirect, and the same tap, `exchange`, scoring and comparison after launch — over an
  engine-owned fixture app whose `@stream` adds four nodes, beside `audio_loopback_node.py`,
  taking its codec, camera and control-plane port as arguments the way the Rust rig
  does. Two refusals ride the arm rather than a note: `BASELINE_CAPTURE=1` is refused
  on it, because a baseline written through the arm whose whole proof is locking to the
  Rust rig's number leaves nothing to lock to; and a venv whose extension predates the
  markers exits naming `maturin develop`, since a stale wheel would score the old code.
  Both codecs PASS through the arm, log gates at zero, clean exit. The reference-PNG rig
  gets no Python arm — nothing Python-specific sits on the colour path.
  [python-codec-block-api — SHIPPED #2107]
  <!-- verify: PIPELINE=python bash runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh -->
  <!-- verify: git ls-files runtime/streamlib-engine/tests/fixtures/codec_roundtrip_node.py -->
- **DECIDED** — An encoded audio packet is an ordinary bag, the encoded-frame convention
  applied to audio: `codec` (`"opus"`), `bitstream` (msgpack `bin`, one Opus packet as
  RFC 6716 §3 frames it), `is_sync_point` (`true` on every packet — a decoder enters at
  any), `group_index` and `sequence_index` (each packet its own group), `sample_rate`
  (`48000`, Opus's own clock), `channels`, `sample_count` (per-channel samples the packet
  spans, `960` for 20 ms — `AudioBlock`'s unit), and `pre_skip` (the encoder's lookahead
  in 48 kHz samples, the `OpusHead` PreSkip a container writes and a decoder trims). The
  stamp rides the frame header and names the packet's first sample, carried from the
  window block the encoder consumed with the timestamped write. Refused by name, never
  reshaped: a missing key, a `codec` other than `opus`, a non-`bin` `bitstream`, and a bag
  with none of these keys — the encoded-video bag's three refusals spelled again. The Rust
  struct is `EncodedAudioPacket` — *packet*, because Opus uses *frame* for a subdivision
  of one, and a name that means two things at the seam it crosses is the wrong name.
  [opus-mp4-recording-rung — SHIPPED #2125]
  <!-- verify: cargo test -p streamlib-media-builtins --lib encoded_audio_packet::tests::encoded_audio_packet_msgpack_wire_carries_the_documented_keys -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib encoded_audio_packet::tests::the_bitstream_crosses_the_wire_as_a_binary_payload_not_an_array -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib encoded_audio_packet::tests::a_bag_with_no_encoded_packet_keys_is_refused_naming_the_keys -->
- **DECIDED** — `tatolab.stream.EncodedAudioPacket` is the Python cast, pure Python beside
  `encoded_video_frame.py`, read with `into=EncodedAudioPacket`, every rule of the video
  cast verbatim: the wire keys are the constructor keywords, `bool` is refused where an
  integer is required, unknown keys are read past, the payload is stored under the Rust
  struct's own field name and stays off the repr, and there is no to-bag helper and no
  numpy property. [opus-mp4-recording-rung — SHIPPED #2126]
  <!-- verify: pytest sdk/tatolab-stream/tests/test_encoded_audio_packet_cast.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_encoded_audio_packet_cast.py -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_encoded_audio_packet_cast.py::test_an_encoded_audio_packet_takes_no_surface_and_holds_no_claim -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_encoded_audio_packet_cast.py::test_the_cast_offers_no_way_back_onto_the_wire -->
- **DECIDED** — `OpusEncoder` is `execution = reactive`, `scheduling = high` like the
  video blocks, input `audio` declaring `delivery_profile = "ordered"` and
  `audio_window(sample_rate = 48_000, dtype = "f32", window_size = 960, hop = 960)` — no
  channel count — so the engine resamples and frames, and `process()` receives one 20 ms
  Opus frame per dispatch in the source's own channels. Framing is the window contract's
  job, never the encoder's. The encoder mints from the first block's `channels`, the video
  encoder's first-frame pattern: one or two channels through libopus's `Encoder`, three
  to eight through `MSEncoder` with channel mapping family 1 (the standard surround order
  both MP4 and WebRTC accept), more than eight refused by name; a block whose count
  changes re-mints, as an extent change re-mints video, without resetting the sequence.
  Output `encoded_audio`; `pre_skip` is the minted encoder's reported lookahead. Config,
  both optional so `{}` is legal: `bitrate_bps` (absent → libopus's automatic rate) and
  `application` (`"audio"`, `"voip"`, `"lowdelay"`; absent → `"audio"`). FEC and DTX off.
  [opus-mp4-recording-rung — SHIPPED #2125]
  <!-- verify: cargo test -p streamlib-media-builtins --lib opus_encoder::tests::the_input_declares_a_window_contract_that_follows_its_sources_channel_count -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib audio_window_to_encoded_packet_encoder::tests::the_encoder_mints_from_the_first_windows_channel_count -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib audio_window_to_encoded_packet_encoder::tests::a_window_whose_channel_count_changes_re_mints_without_resetting_the_sequence -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib opus_stream_layout::tests::three_to_eight_channels_ride_mapping_family_one_in_vorbis_order -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib opus_stream_layout::tests::a_channel_count_opus_cannot_place_is_refused_naming_the_count_and_the_range -->
- **DECIDED** — `OpusDecoder` is `reactive`/`high`, input `encoded_audio` (`ordered`,
  declaring no window contract), output `audio` as `AudioBlock` bags: `f32`, `48000`, the
  packet's `channels` and `sample_count`, stamp equal to the packet's, published through
  the timestamped write; one or two channels through `Decoder`, three to eight through
  `MSDecoder`. No config. It enters at any packet and trims `pre_skip` at entry so its
  first emitted sample is the stamped instant. A `sequence_index` step other than one is a
  gap: reset, re-enter, log the count, invent nothing — no concealment, no FEC decode.
  That is the drop-at-the-edge and flush-not-interpolate doctrine applied to a codec: a
  decoder that concealed a lost packet would invent 20 ms of audio, so the gap stays
  derivable from the stamps instead. [opus-mp4-recording-rung — SHIPPED #2125]
  <!-- verify: cargo test -p streamlib-media-builtins --lib opus_decoder::tests::the_encoded_input_is_ordered_and_declares_no_window_contract -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib encoded_packet_to_audio_block_decoder::tests::a_tone_survives_the_round_trip_at_one_two_and_six_channels -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib encoded_packet_to_audio_block_decoder::tests::the_first_emitted_sample_is_the_anchoring_packets_stamped_instant -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib encoded_packet_to_audio_block_decoder::tests::a_sequence_index_gap_resets_the_decoder_and_re_enters_counting_what_was_lost -->
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
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_third_party_notices.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py::test_the_native_extension_links_nothing_the_host_may_not_supply -->
- **DECIDED** — `Mp4Sink` muxes the encoded elementary streams the blocks produce
  (H.264/H.265 video, Opus audio) in pure Rust through `mp4-atom` — no ffmpeg subprocess,
  no raw-frame transcode path, no new `DT_NEEDED` entry.
  [codec-blocks; opus-mp4-recording-rung — SHIPPED #2127]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py::test_the_native_extension_links_nothing_the_host_may_not_supply -->
- **DECIDED** — `Mp4Sink` is `reactive`/`high` with one `ordered` input, `tracks`, and no
  output. Any number of links may enter it and **each inbound link is one track**, named
  by its source channel name, so two cameras are two video tracks and three microphones
  three audio tracks with no configuration. A link is already the engine's unit of a
  stream and MP4, CMAF, MoQ and WebRTC all model a stream as a track, so a fixed
  video-plus-audio pair is not the shape — and a caption or data
  track then needs only a bag convention, not a sink change. The track's kind is the bag's
  `codec`: `h264`/`h265` a video track, `opus` an audio track, anything else refused by
  name. At `setup()` the sink enumerates its inbound links, refusing by name when there
  are none; it opens `path` (required, created or truncated) and refuses by name a path it
  cannot open, the named-device shape. Truncating is the call: an app is re-run from the
  same `stream.py`, wall-clock file naming would be a fourth surface the clock entry bans, and
  refusing an existing file fails every second run.
  [opus-mp4-recording-rung — SHIPPED #2127]
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_sink::tests::the_only_port_is_one_ordered_input_and_there_is_no_output -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_sink::tests::the_config_names_the_file_and_nothing_else -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::the_file_opens_with_the_brands_and_one_trak_per_link_named_after_its_producer -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::a_bag_on_a_link_the_sink_never_enumerated_is_an_error_not_a_latch -->
- **DECIDED** — The layout is fragmented: `ftyp`, one `moov` with every track's sample
  entry and `trex`, then `moof` + `mdat` per fragment, one `traf` per track. `moov` is
  written once every track has delivered its first sync-point bag, since sample entries
  need the parameter sets and the Opus header; a link still silent is named once a second,
  and cannot hold the others' samples without bound. A fragment closes at the first video
  track's sync points — each second when no video is wired — and carries every track's
  samples stamped within that span. Why fragmented: teardown is not a promise (a panicked
  thread, SIGKILL, the untrusted tier) and a flat file whose trailing `moov` never lands
  is nothing, while this one plays to its last closed fragment; and it is the shape (CMAF)
  a networking sender emits, so the writer is reused there rather than being a dead end.
  [opus-mp4-recording-rung — SHIPPED #2127]
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::the_header_waits_until_every_link_has_delivered_a_sync_point -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::a_fragment_closes_at_the_pacing_video_tracks_sync_points -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::a_file_truncated_at_any_fragment_boundary_re_parses_cleanly -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::a_link_that_never_delivers_cannot_hold_the_others_samples_without_bound -->
- **DECIDED** — Video sample entries are `avc1`/`avcC` and `hvc1`/`hvcC` from the first
  sync-point access unit's parameter sets: H.264's profile, compatibility and level bytes
  are the SPS payload's first three; H.265's profile-tier-level, chroma and bit depths come
  from the engine's own SPS parser, platform-free in `core/h265_sequence_parameter_set.rs`,
  never a second one for the same bytes. Parameter-set NALs
  are stripped from samples — ISO/IEC 14496-15 forbids in-band sets under `avc1`/`hvc1`,
  and `hvc1` is what Apple hardware plays. Every remaining NAL is 4-byte length-prefixed, the walk being the
  engine's one platform-free Annex-B walk, shared with the VideoToolbox arm, rather than a
  fourth splitter; a sync-point bag is a sync
  sample. A parameter set that changes mid-file, a track whose `codec` changes, and an
  Opus track whose `channels` changes are each refused by name, **per track and never per
  file**: there is no second sample entry to switch to — one lives only in the one `moov`
  (14496-12 §6.1.2) and `dOps` shall carry the identification header's count
  (Opus-in-ISOBMFF §4.3.2) — so the sink says so once naming the link and that track's
  last written stamp, stops writing it, reads and discards every later bag it carries, and
  every other track keeps recording. A `moof` owes a `traf` to no track (§8.8.6), so a
  track that stops appearing is a legal file, and one microphone's format change must not
  end two cameras' recording. The refusal is the built-in's own latch, the shape both
  encoders already use: a `reactive` processor has no `Error` state to reach — the runner
  logs an `Err` from `process()` and carries on. [opus-mp4-recording-rung — SHIPPED #2127;
  macos-capability-parity — SHIPPED #2413, #2414]
  <!-- verify: cargo test -p streamlib-media-builtins --lib an_h265_and_opus_recording_is_byte_identical_to_the_golden_file -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_track_sample_entry::tests::avcc_takes_profile_compatibility_and_level_from_the_sps_payloads_first_three_bytes -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_track_sample_entry::tests::hvcc_takes_chroma_and_bit_depths_from_the_engines_own_parser -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::no_parameter_set_nal_survives_into_any_sample -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::every_sample_nal_inside_mdat_is_four_byte_length_prefixed -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::a_mid_file_parameter_set_change_stops_that_track_and_leaves_the_others_recording -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::an_opus_track_whose_channel_count_changes_stops_naming_both_counts -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::fragments_keep_closing_after_the_only_video_track_latches -->
- **DECIDED** — An Opus track is the `Opus` sample entry with `dOps` (version 0, the bag's
  `channels`, PreSkip = `pre_skip`, InputSampleRate 48 000, gain 0; mapping family 0),
  timescale 48 000, each sample's duration its `sample_count` — except the sample before a
  capture gap, whose duration is extended to cover it so the dropout is represented rather
  than elided (§8.8.12.2; the trigger is a stamp a whole packet or more past the accounted
  position, so arrival jitter is never written in as timing). **PreSkip is the encoder's
  reported lookahead (312 at 48 kHz), deliberately below the 80 ms (3 840) floor
  Opus-in-ISOBMFF §4.3.2 states.** That floor is RFC 7845 §4.2's recommendation for
  *cropping an existing stream* rendered as a `shall`; the spec's own §4.7 example writes
  312, and no shipping muxer writes anything else (FFmpeg, GStreamer `qtmux`,
  gst-plugins-rs `fmp4mux`, Xiph `libopusenc`). The field is not informative in practice —
  FFmpeg, Chromium, ExoPlayer and Android all discard exactly this many decoded samples —
  so 3 840 would destroy 73.5 ms of real audio and lead the video by it. With no edit list
  (the epoch rule), a player that keeps media time after the trim places the first real
  sample 6.5 ms late: the residual every FFmpeg- and GStreamer-authored Opus MP4 carries,
  below any lip-sync threshold, and present in every option.
  [opus-mp4-recording-rung — SHIPPED #2127]
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_track_sample_entry::tests::an_opus_entry_states_the_bags_channels_and_the_encoders_lookahead -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::an_opus_track_states_its_channels_and_the_encoders_pre_skip -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::an_opus_samples_duration_is_its_own_sample_count -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::a_capture_gap_lands_in_the_preceding_samples_duration_while_that_sample_is_still_pending -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::a_capture_gap_after_a_fragment_closed_lands_in_the_next_fragments_decode_time -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::arrival_jitter_below_one_packet_is_not_written_into_the_container_as_timing -->
- **DECIDED** — **Three to eight channels record no Opus track yet.** `mp4-atom` 0.15
  writes `ChannelMappingFamily` 0 unconditionally and refuses any other value on read, so
  mapping family 1 has no representation in the container writer. Such a track is refused
  by name, naming the container rather than the codec: `OpusEncoder` still mints the
  stream — the layout places 1–8 channels — and only recording it does not follow. Owner
  ruling 2026-09-03, taken over hand-splicing the `dOps` bytes (which is the hand-written
  box writer this rung rejected) and over carrying a second vendored fork. The gap is
  tracked as #2139; `camera-audio-recorder` is mono or stereo, so the showcase is
  unaffected. [opus-mp4-recording-rung — SHIPPED #2127]
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_track_sample_entry::tests::a_channel_count_needing_mapping_family_one_is_refused_naming_the_container -->
- **DECIDED** — Time is the plan's own subtraction written into the container: the epoch
  is the earliest first stamp across tracks, each track's first `tfdt` is its own offset
  from it, no edit list, no drift correction. Video timescale is 1 000 000 000 — a legal
  `u32`, so the monotonic-nanosecond deltas the whole data plane shares land exactly, with
  no 90 kHz rounding carry across a long recording — with 64-bit `tfdt`; a video sample's
  duration is the delta to the next, so one frame per track is held back and the last
  takes its predecessor's at teardown. A bag stamped at or before its track's last written
  one is dropped and counted, a producer bug on an `ordered` input, named as such.
  [opus-mp4-recording-rung — SHIPPED #2127]
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::each_tracks_first_tfdt_is_its_own_offset_from_the_earliest_stamp -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::a_video_samples_duration_is_the_delta_to_its_successor -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::a_bag_stamped_at_or_before_the_last_written_one_is_dropped_and_counted -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::a_run_ending_on_a_sync_point_still_gives_its_last_frame_a_duration -->
- **DECIDED** — `teardown()` closes the open fragment, held-back frames included, and owes
  nothing else. [opus-mp4-recording-rung — SHIPPED #2127]
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::the_checked_in_inspector_fixture_is_what_this_writer_produces -->
- **DECIDED** — `OpusEncoder`, `OpusDecoder` and `Mp4Sink` reach Python through the three
  touchpoints a native built-in owns and no fourth, and no Linux split — nothing here is
  platform-bound, so they register unconditionally beside the audio built-ins. The stub
  docstrings state the engine's own behavior rather than an aspiration: the encoder's
  window and first-block mint, its two config keys, the decoder's entry and gap rule, the
  sink's track-per-link rule, its `moov` wait, fragment rule and truncate-at-setup.
  `Mp4Sink` records on both floors: the SPS reader, RBSP bit reader and emulation-prevention
  removal it reads live in the platform-free `core/h265_sequence_parameter_set.rs` and
  `core/nal_unit_raw_byte_sequence_payload.rs`, re-exported through `streamlib::sdk`, so
  nothing it reads sits under the Vulkan Video tree.
  [opus-mp4-recording-rung — SHIPPED #2126, #2128; macos-capability-parity — SHIPPED #2414;
  stream-graph — SHIPPED #2567]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_opus_blocks.py::test_the_round_trip_wires_without_an_adapter -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_mp4_sink.py::test_two_encoders_wire_into_the_one_input_without_an_adapter -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_built_in_node_classes.py::test_the_built_in_class_cannot_be_instantiated -->
- **DECIDED** — The rung's CI-run, GPU-free proof: the stage with `channels` absent emits
  the source's count and converts when one is declared, in Rust and through the Python
  declaration; the link-naming read returns the link a synthetic frame was pushed on, with
  counting untouched; the `EncodedAudioPacket` wire and cast; the Opus bodies against the
  real library with no `Runtime` — a tone through encode → decode within a stated floor
  for one, two and six channels, `pre_skip` aligning the first sample, a gap resetting;
  and container bytes — the writer body driven with synthetic bags over checked-in H.264
  SPS/PPS and H.265 VPS/SPS/PPS fixtures, re-parsed with `mp4-atom`. The same inspection
  ships as `cargo xtask mp4-inspect <file>` — tracks, names, sample entries, fragments,
  durations as JSON — so nothing downstream needs ffprobe.
  [opus-mp4-recording-rung — SHIPPED #2123, #2124, #2125, #2126, #2127]
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer -->
  <!-- verify: cargo test -p streamlib-engine --lib core::annex_b_access_unit -->
  <!-- verify: cargo test -p xtask mp4_inspect -->
  <!-- verify: cargo test -p xtask mp4_inspect::tests::a_real_sink_recording_reports_both_tracks_under_their_link_names -->
- **DECIDED** — Rig-only, `requires_gpu` and said in the module docstring:
  `test_opus_blocks.py` — a Python known-signal source → `OpusEncoder` → probes →
  `OpusDecoder` → probes: every bag casts, `sequence_index` advances by one,
  `sample_count` is 960, decoded blocks are 48 kHz `f32` in the source's channels;
  `test_mp4_sink.py` — two sources into one sink give a file whose `mp4-inspect` names two
  tracks after their producers. [opus-mp4-recording-rung — SHIPPED #2126, #2128]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_opus_blocks.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_mp4_sink.py -->
- **DECIDED** — Live, two arms on engine-owned fixtures beside `audio_loopback_node.py`
  and `codec_roundtrip_node.py`. `opus_roundtrip_node.py`: `KnownAudioSignalSource →
  OpusEncoder → OpusDecoder → CapturedAudioWaveformRecorder`, scored by
  `known_audio_signal.py` — tone identity and the DTMF timing grid intact within its own
  floor, a lossy codec's verdict being the analysis's, never a sample-exact match — with
  no audio device in the path, so a failure here with the loopback green is the codec's.
  `recording_node.py`: the vivid camera and the known signal → `H264Encoder` and
  `OpusEncoder` → `Mp4Sink`, stopped by SIGTERM (a run needing SIGKILL is a hard fail —
  teardown is what closes the last fragment), then `mp4-inspect` PASS, then the
  decode-back: `codec_roundtrip_rig --source mp4:<path>` demuxes the video track with
  `mp4-atom`, turns length prefixes back into start codes, re-prepends the parameter sets
  from the sample entry and replays it through `H264Decoder` to `xtask psnr
  channel-means`, locked to the per-codec vivid baseline within ±0.05. That lock is the
  whole argument: the container sits in the middle of the path the codec rig already
  scored, so a mismatch is a regression in the writer and never a reason for a third
  baseline. [opus-mp4-recording-rung — SHIPPED #2126, #2128]
  <!-- verify: bash runtime/streamlib-engine/tests/fixtures/verify_opus_roundtrip.sh -->
  <!-- verify: bash runtime/streamlib-engine/tests/fixtures/e2e_fixture_recording.sh -->
- **DECIDED** — The held codec consumers resolve through this align, per §Consumers: a held
  codec package is mined for its logic and deletes in the change that ships its block, and a
  codec proof example deletes into the engine-owned proof rig — its job becomes the rig's
  job, and a test owns its fixtures. `packages/jpeg`, `examples/jpeg-psnr` and the fixture
  that drives it (`e2e_fixture_psnr_jpeg.sh`) delete outright: their block is retired,
  nothing is mined and no rig job is owed.
  [codec-blocks — SHIPPED #2087; opus-mp4-recording-rung — SHIPPED #2129; amended by
  jpeg-after-the-robotics-cut: the JPEG package, example and fixture delete outright (owner,
  2026-10-04)]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-09-01-codec-roundtrip-reproof.md -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-09-03-opus-mp4-recording-rung.md -->
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
  refuses by name, because a denied microphone delivers silence, and discovery retries after
  a local-network denial. A runtime started from a terminal credits that terminal — the
  responsible code — and only Apple's Terminal is exempt from the local-network check. The
  prompt's exact wording under `SMAppService` is an acceptance check of the installer change.
  Owner, 2026-10-01, on #2560's research. [runtime-hosting; one-runtime-per-machine]

## Networking — transport, moq, webrtc — IN-FLIGHT (→ stream-graph, runtime-hosting)

- **DECIDED** — Cross-language interop happens on the wire between nodes, as
  self-describing bags — never in-graph. [importable-python-library — SHIPPED #1715]
- **DECIDED** — WebRTC ships as an extension wheel under §Packages & extension model rather
  than as a built-in — not every app needs it, and a capability with a consumer is what proves
  the extension model. Its processors are ordinary processor extensions, and a runtime
  capability the wheel needs is exposed as engine code — a split of concerns, expected to be
  rare. WebRTC is an edge source/sink processor pair ingesting and egressing external
  streams at a runtime boundary. [extension-model; networking-extension-wheels — SHIPPED
  #2153]
- **DECIDED** — The wheel sits on the encoded side of the codec blocks and touches no raw
  frame, surface or GPU: `WhipPublisher` consumes `EncodedVideoFrame` and
  `EncodedAudioPacket` bags downstream of `H264Encoder` and `OpusEncoder`; `WhepPlayer` emits
  the same bags upstream of `H264Decoder` and `OpusDecoder`. Audio is in scope from the first
  rung. Both are ordinary processor extensions — `@processor` classes in the wheel calling the
  wheel's own Rust — each in its own helper, on the tokio runtime the wheel's support hook
  brought up. [extension-model; networking-extension-wheels — SHIPPED #2151]
- **DECIDED** — Many tracks follow the `Mp4Sink` shape: the publisher takes one track per
  inbound link and derives its session media description from them. The player exposes one
  output per track kind — `encoded_video` and `encoded_audio` — never one port per track:
  ports are declared statically by decorator, and a decoder downstream wants a port it can
  name at wiring time. `WhepPlayer` takes no media name, because a WHEP answer names the
  session's media and there is nothing left to choose. Endpoint and credential configuration
  is ticket-level, as for every built-in's config.
  [extension-model; networking-extension-wheels — SHIPPED #2150, #2151]
- **DECIDED** — The WebRTC processors are typed, and the Python surface has no raw byte
  port for them: the publisher reads `EncodedVideoFrame` / `EncodedAudioPacket` and hands
  the bitstream to the wheel's Rust; the player writes the bag literal against the wire
  contract, filling every required key from the stream itself — the extent from the SPS, the
  ordering pair from its own counters, a sync point from the access unit, the audio
  parameters from the session description — rather than from config. What crosses a
  network on a media track is a bitstream and the keys a decoder needs, never a serialised
  link payload. [extension-model; moq-data-tracks — SHIPPED #2172, #2173]
- **DECIDED** — A received stream reaches the bag through the manual-source shape: the wheel's Rust receives on
  its own runtime and a processor-owned thread writes, so no engine seam is added for it. Two
  budgets the wheel lives inside, ticket-level but named here: a helper stops on the shutdown
  ladder §Processor model states, whose `teardown()` budget is five seconds, so a WHIP
  `DELETE` must fit inside it; and connecting inside `setup()` spends the sixty-second
  registration budget.
  [extension-model; local-transport-hardening — SHIPPED #2264, #2266]
- **DECIDED** — The proof bar is the codec blocks' two halves. CI-run, GPU-free and
  endpoint-free: RTP packetising and depacketising round trips, SDP construction and
  parsing, and the bag literal the player writes checked against the wire contract. Live,
  rig-only: WHIP publish to Cloudflare Stream and WHEP play back from it, with credentials
  outside the tree, in the fixture-script shape the codec rig set (owner, 2026-09-04).
  [extension-model]
- **DECIDED** — `packages/streamlib-webrtc/`: a standalone maturin project — own
  `Cargo.toml` (`[workspace]` root, `[lib] name = "_native"`, `crate-type = ["cdylib"]`,
  `pyo3` on `abi3-py310`, `webrtc 0.17`, `tokio`, `hyper` + `hyper-util` + `hyper-rustls`,
  `rustls`, `bytes`; no engine crate), own lockfile, `pyproject.toml` depending on `streamlib`
  by version, `python/streamlib_webrtc/` with `_native.pyi` and `py.typed`. `src/` carries the
  WHIP and WHEP sessions (`whip_session.rs`, `whep_session.rs`, over `http_signalling.rs`) and
  the RFC 6184 depacketiser `h264_rtp_depacketiser.rs` with its tests.
  `extension.py:load` brings up the tokio runtime and the rustls provider once and
  registers `webrtc`. [networking-extension-wheels — SHIPPED #2150]
- **DECIDED** — `WhipPublisher`: `@processor`, one fan-in input `tracks` (`ordered`), the
  `Mp4Sink` shape — each inbound link is one RTP track, video or audio by the bag's
  `codec`, the session's SDP built from the links `inbound_link_names` reports at
  `setup()`; config `url` and optional `bearer_token`. `WhepPlayer`:
  `@processor(execution = "manual")`, outputs `encoded_video` and `encoded_audio`, config
  `url` and optional `bearer_token`; `start()` hands `ctx.outputs` to a processor-owned
  thread that connects, drains the session and writes bag literals — extent from the SPS,
  `group_index` advancing on each IDR and `sequence_index` within it, `is_sync_point` from
  the access unit, Opus parameters from the SDP answer, the stamp from the RTP clock mapped
  onto the monotonic clock — and `stop()` closes the session inside the 5 s budget. **No
  session is minted in `setup()`, and a refused connect is retried rather than ending the
  stream** (2026-09-05, found by the live proof): a WHEP endpoint answers `409 Conflict`
  while the input it fronts has not started publishing, the ordinary state of a player
  brought up beside its publisher, so the player carries a bounded backoff — a fresh session
  per attempt, since a closed peer connection
  cannot be dialled again. A bag the engine refuses is the one failure not retried: it names
  its port and ends the thread, because reconnecting would spend an endpoint's session
  forever on a bag refused every time. [networking-extension-wheels — SHIPPED #2150]
- **DECIDED** — `python-wheel.yml` carries an `extension-wheels` job over a matrix holding
  `packages/streamlib-webrtc`: install the just-built `streamlib` wheel into the venv,
  `maturin develop` the extension, `cargo test` its crate, `mypy.stubtest` over its
  `_native`, pyright over its Python, pytest with `-m "not requires_gpu"`, and the
  portability gate over its `.so`. `release-please-config.json` carries a package entry per
  wheel (independent versions and tags); the release workflow builds and attaches each wheel
  on its own tag; `build_simple_index.py` is multi-project — a set of published names, one
  PEP 503 directory each, `streamlib-webrtc` the one extension among them — with its tests.
  [networking-extension-wheels — SHIPPED #2152; amended by package-split-and-lend: one
  version for everything released from this repository, in place of independent versions and
  tags (§Distribution & versioning)]
- **DECIDED** — The proof, as built. CI-run, GPU-free, endpoint-free, owned by the wheel:
  the RFC 6184 packetise/depacketise round trip (the carried tests plus STAP-A and FU-A
  cases), SDP offer construction and answer parsing, and the player's bag literal checked
  against the wire contract on the `wired_link` fixture pattern. Live, rig-only, under
  `/verify-live` with a networking arm: WHIP publish of the vivid camera and the known signal
  to Cloudflare Stream and WHEP play-back of the same stream — credentials read from the
  environment, absent ones reported as cannot-run, never as pass. The decode-back is the
  lock: `WhepPlayer` → `H264Decoder` → tap and exchange → `xtask psnr channel-means` against
  the per-codec vivid baseline within ±0.05 — the network sits inside a path the codec rig
  already scored, so a mismatch is the wheel's. [networking-extension-wheels — SHIPPED #2153]
  <!-- verify: git ls-files packages/streamlib-webrtc -->
  <!-- verify: pytest packages/streamlib-webrtc/tests/test_processors.py -->
- **DECIDED** — A runtime has a name, and a port is addressed `<runtime name>/<display name>/<port>`
  — the string `tap` spells a channel with. The name belongs to the runtime rather than to its
  control plane and is a field of `Runner`, taken from `Runtime(runtime_name=…)` — keyword-only,
  the constructor's only keyword, stub-gated, and what `run` / `dev`'s `--runtime-name` fills —
  or `Runner::new_with_runtime_name` in Rust, else `STREAMLIB_RUNTIME_NAME`, else the default
  `<hostname>-<app directory name>-<id>`: every forbidden character replaced by `-`, the id four
  base-36 characters of an FNV-1a hash over the app directory's full path — the virtual camera's
  own recipe — resolved from `STREAMLIB_APP_DIRECTORY`, else the wheel's captured entry directory
  for a hand-run `python <script>.py`, else the working directory. A host that reports no name takes a
  stand-in, said once. Each part of an address is one address chunk — non-empty, no `/`, `*`,
  `$`, `#` or `?`, not beginning with `@`, spaces and unicode legal — checked against the rule's
  own table, and an explicit runtime name that breaks it is refused at construction naming the
  character. The type is `PortAddress` (`core/graph/edges/port_address.rs`, the rule in
  `core/runtime/address_chunk.rs`); `tap` resolves a channel naming this runtime only and
  refuses another runtime's name by name. The registry entry, `--node`, `tap`'s channel and
  `graph`'s top-level `runtime_name` read the one name. Nothing refuses a duplicate: two runs
  from one directory both start, and a `--node` name two live registry rows hold is refused
  naming both. `run` and `dev` take no other naming flag. `runtime_id` stays per-run — logs,
  the registry file, iceoryx2 names — and is never an address.
  [runtime-mesh — SHIPPED #2282, #2284; zenoh-and-moq-wheel-removal — SHIPPED #2643, #2645;
  reopened by one-runtime-per-machine: whether addresses gain a stream level; amended by
  runtime-hosting: the runtime name gives way to the machine name (the address entry below)]
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::runtime_name -->
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::address_chunk -->
  <!-- verify: cargo test -p streamlib-engine --lib core::graph::edges::port_address -->
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::runtime::tests::two_runners_given_one_runtime_name_both_construct -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_tap_naming_another_runtime_is_refused_naming_that_runtime -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_runtime_name.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py::test_a_verb_targets_a_node_by_its_runtime_name -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py::test_a_verb_given_a_name_two_live_runtimes_hold_is_refused_naming_both -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py::test_a_retired_flag_is_a_usage_error -->
- **DECIDED** — A link's ends are both on this runtime. `OutputLinkPortRef` and
  `InputLinkPortRef` name a port of this runtime's graph and nothing else, and the graph holds
  one link collection, which every traversal walks; `LinkState` has no state that waits on
  another runtime. A snapshot whose link end names a runtime is refused by name at load rather
  than read as a local end. In Python `connect(source, destination)` takes a reference minted by this
  stream's builder on each end, and a value of any other kind is refused naming the spelling
  that would work. The engine reads no bag key to carry a link. Linking to another machine is the
  sharing step's (the moq-on-the-tailnet entries below).
  [zenoh-and-moq-wheel-removal — SHIPPED #2643]
  <!-- verify: cargo test -p streamlib-engine --lib core::graph_snapshot::tests::a_link_end_naming_a_runtime_is_refused_naming_that_runtime -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_runtime_load.py::test_a_link_end_naming_a_runtime_is_refused_by_load_naming_it -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_stream_graph_builder.py::test_connect_refuses_an_input_as_its_source_naming_the_fix -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-10-05-zenoh-and-moq-wheel-removal.md -->
- **DECIDED** — A link between streams is always pulled. Only the stream that owns the input
  creates it, reading a port the source stream has exposed — private on the machine, public off
  it — and the source is never asked. No runtime pushes its output into another's input, and no
  runtime wires two others. A machine that wants
  another's port finds it among that machine's exposed ports and pulls it — from its own stream,
  from code on the machine, or by opening the port's URL. Owner, 2026-10-04: a source never
  wires itself into a reader, the way a server never wires its URL into a client's browser.
  [exposure-levels]
- **DECIDED** — Every stamp in the tree is this machine's. The one monotonic clock of §Media I/O
  is one clock per machine, and a stamp is never compared against one from another clock. No
  link can carry a stamp from another machine, so nothing reads or renders a clock identity,
  and `Mp4Sink` compares first stamps across tracks with no another-machine refusal. How a stamp taken on another machine's clock is marked is the sharing step's OPEN below.
  [runtime-mesh; zenoh-and-moq-wheel-removal — SHIPPED #2643]
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-10-05-zenoh-and-moq-wheel-removal.md -->
- **OPEN** — A common clock across machines: intended, do not build until designed. Direction:
  machines negotiate a shared network time (PTP, NTP or similar) so stamps from different
  machines become comparable; until then the per-clock rule above stands. A relay — a
  decoder, an encoder, any Python `write(…, timestamp_ns=)` — restates an upstream stamp on a
  local output, so whatever closes this also says how a restated stamp keeps the machine it
  was taken on. The question stands for stamps that cross machines over MoQ.
  [runtime-mesh; cross-runtime-links; moq-on-the-tailnet]
- **DECIDED** — The runtime decides what leaves the machine; who reads a public port off the
  machine is the tailnet's access rules or a relay's. [one-runtime-per-machine;
  moq-on-the-tailnet]
- **DECIDED** — streamlib does not solve every networking problem. Reaching a machine behind
  NAT, a peer machine's identity, and encryption between machines are left to Tailscale or a
  relay the machine dials out to. streamlib owns what leaves a machine, streams as URLs, and
  many streams on one runtime (owner, 2026-09-30). Nothing is built for a private network
  without a tailnet. [one-runtime-per-machine; moq-on-the-tailnet]
- **DECIDED** — The address is `<machine>/<stream>/<node>/<port>`: the stream takes the place
  of today's runtime name, the runtime is addressed by its machine (default the hostname),
  every existing address maps across with one segment prefixed, and the same string is the
  address in Python, the CLI, the URL path and the graph (owner, 2026-09-30). Collisions (owner,
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
  Owner, 2026-10-02 (runtime-hosting decision 3). [one-runtime-per-machine; runtime-hosting;
  amended by moq-on-the-tailnet: a machine name is its tailnet name (the entry below)]
- **DECIDED** — Exposure: every output port of a stream is internal, private or public, and the
  runtime enforces it at the stream's edge, never inside the stream. **Internal**, the default:
  any node of the stream may link to it, and nothing outside the stream may read it.
  **Private**: any other stream on the machine, and code on the machine, may read it.
  **Public**: private, plus a URL reachable off the machine, which other machines and tools
  pull. `stream.expose(output)` makes an
  output private and `stream.expose(output, Exposure.PUBLIC)` public; a level is an enum
  member, never a string. The stream's function sets where its exposures start; `expose` at
  the CLI, the app or the local API changes them while the stream runs, the change applies at
  once — a reader the new level no longer allows is cut off — and neither the runtime nor the
  stream restarts. The engine checks the live exposures wherever a read crosses a stream's
  edge — another stream's link, a reader or URL on the machine, a reader on another machine —
  and a port is read from outside its stream only through exposure: no debugging tap or other
  door bypasses it, and a stream's own logs are how its insides are seen. Nothing leaves the
  machine until a port is public. The CLI and the app list every running stream with its
  private and public ports; an internal port may be listed and is never readable. Owner,
  2026-10-04. [exposure-levels; one-runtime-per-machine; stream-graph; amended by
  moq-on-the-tailnet: off the machine a public port is read over MoQ on the tailnet, and at a
  relay once the machine has joined one; machines are listed from Tailscale's status and asked
  for their public ports]
- **DECIDED** — Every public port is reachable by URL, and every private port from any tool
  on the machine. A user with no account can see and use their own streams locally (owner,
  2026-09-30). [one-runtime-per-machine; amended by moq-on-the-tailnet: live data has one form,
  MoQ, played by a browser through the viewer page or read by another runtime; HTTP serves the
  listing, the read-only MCP, snapshots and small samples, and off the machine both need a
  tailnet or a relay]
- **OPEN** — The URL grammar and the forms. Direction (review, not decided): a machine exports
  one namespace, `/<stream>/<node>/<port>/<form>`, every level listable, an unexposed port
  simply absent from it; the form a child segment, never a query parameter. Live data has one
  form, `moq`, and `ts`, `hls`, raw H.264, fMP4 and `whep` are not forms.
  Direction for the rest, names undecided: `page` for the viewer, and a snapshot and a
  bounded-sample form (`png`, `ndjson`). Undecided:
  how a relay prefixes the namespace; whether a stream's description is written by its author,
  generated from the nodes' descriptions and a live sample, or both (it stays documentation,
  never a contract at the port); and the certificate a browser is shown on the engine's QUIC
  listener. Known: Tailscale Funnel carries no UDP, so live data from a machine goes over the
  tailnet directly or through a relay; the HTTP passes.
  [one-runtime-per-machine; moq-on-the-tailnet]
- **DECIDED** — End-to-end encryption through a relay is not a launch requirement; it is a
  later change, and the design keeps it possible. A hosted relay's operator can see the
  streams passing through it until that change lands, and that is said plainly; a
  self-hosted relay is the user's own and exposes nothing. What
  stays true so the door stays open: a relay never parses the payloads it forwards; the engine
  never reads a bag's payload; a frame's sequence, stamp and keyframe flag stay readable so
  delivery and drop counting work; and no key ever lives on a relay. When it lands it is the
  WebRTC insertable-streams and MoQ secure-objects shape — encrypt each frame at the source
  inside the transport encryption, decrypt in the player, relays untouched — with a pre-shared
  key per team first (carried to a browser in a URL fragment, which never reaches a server),
  per-stream keys through the control client later, and the team's devices signing the key
  set the way Tailnet Lock does. Its cost, accepted for later: every server-side form
  (snapshots) then comes from the machine, and stock ffmpeg and curl work only through a TLS
  tunnel ending on the machine. Owner, 2026-09-30. [one-runtime-per-machine; amended by
  moq-on-the-tailnet: the relay is a separate program the machine joins]
- **DECIDED** — Off a machine, real-time data travels over MoQ on QUIC and over nothing else.
  On one machine streams share data through shared memory, as today. Each machine's engine
  serves a MoQ endpoint itself, on its tailnet address, and a reader on another machine
  subscribes to it directly; no MoQ relay is needed between machines on one tailnet. A
  link between streams on two machines is still pulled by its reader from a public port
  (pull-only, above), and nothing is sent for a port nobody subscribes to. Zenoh is removed —
  no session, no runtime mesh, no mesh name, no multicast discovery, no router. Owner,
  2026-10-04. [moq-on-the-tailnet]
- **DECIDED** — Tatolab builds nothing Tailscale already does. Reach between machines, machine
  names, encryption on the wire, identity and discovery come from the tailnet. A machine with
  no Tailscale runs every stream, shares between streams on itself, and can join a relay; it is
  never read directly by another machine, and nothing is built for a plain LAN — no discovery,
  no certificate minting, no name claiming, no peer authentication. Who may read a public port
  on the tailnet is Tailscale's access rules: the engine checks the port's exposure and adds no
  per-reader check, so the stream map and peer authentication are not built. Owner, 2026-10-04.
  [moq-on-the-tailnet]
- **DECIDED** — A machine's name is its tailnet name. The engine reads the local Tailscale's
  status for two things: its own machine name, and the list of the tailnet's other machines.
  Whether a listed machine runs Tatolab, and what it exposes, is learned by asking that
  machine. `desk/<stream>/<node>/<port>` therefore names the tailnet machine `desk`, and a full
  tailnet name is accepted in the machine position for a machine shared in from another
  tailnet. With no Tailscale the machine's name is its hostname and matters only on that
  machine. Owner, 2026-10-04. [moq-on-the-tailnet]
- **DECIDED** — The engine stands on the moq-dev line of MoQ, the `moq-net` and `moq-tokio`
  crates, pinned to exact versions, and serves sessions itself with no relay program beside it.
  Known (2026-10-04): the line's compatibility with Cloudflare's hosted relays is
  claimed upstream and unverified here. Owner, 2026-10-04. [moq-on-the-tailnet]
- **DECIDED** — The internet edge is a relay the machine has joined, and joining is a machine
  setting. A machine joined to a relay offers every one of its public ports there, sending a
  port only while someone subscribes; there is no fourth exposure level and no per-port
  internet flag, and who reads at the relay is the relay's own access rules. The runtime joins
  any relay from an address and a credential and holds nothing else about relays — no account,
  team, directory, signed link or billing, which belong to a separate private service. No relay
  is written or bundled here; a person who wants their own runs an existing open-source relay
  program. The relay is never a role of the runtime. Owner, 2026-10-04. [moq-on-the-tailnet]
- **DECIDED** — HTTP carries only what is not real time: the listing of exposed ports, a
  read-only MCP, snapshots, small samples and the viewer page — never live media, in any
  browser and for any tool. `tatolabd` asks Tailscale to serve its HTTP, on a port of its own,
  the first time a port goes public; where Tailscale refuses for lack of rights it says the
  one command to run once, and it never alters any other serve setting. Off the machine that
  HTTP shows public ports only, as on any address but loopback (§Control plane, the local
  API). Live forms over HTTP — MPEG-TS, HLS, raw H.264 and fMP4 — and a `whep` form are not
  built. Owner, 2026-10-04. [moq-on-the-tailnet]
- **DECIDED** — The viewer page is in the first version of sharing: every public port has a
  page that plays it, served with the HTTP above and built on the MoQ stack's own browser
  player, never one written here. One path serves every browser — QUIC and MoQ. No WebSocket
  or other TCP fallback is built or enabled. Owner, 2026-10-04. [moq-on-the-tailnet]
- **DECIDED** — Until the sharing step nothing links one machine to another and the tree holds
  no MoQ; everything on one machine works, and the WebRTC extension stays as it is. The `tap` and `exchange` verbs, retired
  by the exposure entry above, stay until the sharing step and are deleted in the change that
  builds the snapshot and sample forms: from then the repo's live verification reads a port
  the way a user does — the fixture stream exposes it private, and the check fetches exact,
  full-resolution snapshots from the machine's local HTTP listing.
  The one-runtime-per-machine sequence continues through runtime hosting, its in-flight
  changes built minus their mesh parts, then
  accelerators optional, then one sharing step — the entries above — in place of that
  sequence's steps 8 and 9, then resources, packs and the app. Owner, 2026-10-04.
  [moq-on-the-tailnet; zenoh-and-moq-wheel-removal — SHIPPED #2633, #2643, #2645, #2649]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-10-05-zenoh-and-moq-wheel-removal.md -->
- **OPEN** — The sharing step's details, decided at its own align and built against by nothing
  until then: which versions of `moq-net` and `moq-tokio` are pinned; how groups are cut for
  data that is not video, without the engine reading a bag;
  what a public port whose bags name a surface serves off the machine, a browser needing
  encoded video rather than raw pixels; how a relay is joined and how a machine's ports
  are named there; the certificate the engine's QUIC listener shows a browser; how the HTTP
  that `tailscale serve` fronts shows public ports only, the loopback listener showing private
  ones too; what a browser that cannot play is told; the read-only
  MCP's tools and who may call it; the verbs that list machines and their public ports; what a
  control client hands the runtime beyond a relay address and a credential; and how a stamp
  taken on another machine's clock is marked. Direction (session's, not decided): a bag stands alone, so each may open
  its own group, video cutting at its keyframes; machine to machine needs no certificate from
  Tailscale, a tailnet address being authenticated already; `tailscale serve` supplies the
  HTTPS certificate for what it fronts, so the runtime requests and renews none. Known
  (2026-10-04): `tailscale serve` proxies no UDP, so the engine listens for QUIC on the tailnet
  address itself; changing serve settings needs root, Tailscale's operator user or, on macOS,
  the admin group, and HTTPS must be switched on for the tailnet; Funnel carries TCP only, on
  ports 443, 8443 and 10000; any local user reads Tailscale's status on Linux, the same user
  on macOS; the stack's player keeps WebKit on a WebSocket fallback by default, which the entry
  above rules out, so Safari and iOS play only where their own QUIC support holds. Known
  (2026-09-30): Safari accepts no self-signed certificate hash, so a Safari browser reaching the
  engine's QUIC listener needs a certificate it trusts, such as the tailnet's.
  [moq-on-the-tailnet]

## Language SDKs & parity — IN-FLIGHT (→ package-split-and-lend)
<!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py -->

- **DECIDED** — Python is the sole focus runtime: the importable PyO3 wheel is the
  primary authoring surface. TypeScript authoring is paused,
  not rejected — a future TypeScript SDK follows this same importable-library model
  (a native module a TypeScript app imports; Deno itself optional), aimed at the
  hobbyist / video-creator audience when it is scheduled.
  [importable-python-library — SHIPPED #1707, #1708; importable-python-library-ripout
  — SHIPPED #1715]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-10-importable-python-library-ripout.md -->
- **DECIDED** — The Python SDK carries a GIL-release contract: every native binding
  that can block releases the GIL around the blocking call, and pixels never cross
  into Python as Python-owned objects — frames travel as handles / surface ids, and
  pixel memory is reached only through explicitly exported views (DLPack, the CUDA
  Array Interface, a mapped CPU buffer). The contract exists so a
  blocking native binding never stalls the threads of its own interpreter — the app's
  for the app-side bindings, the helper child's for a processor's. It is never a
  co-tenancy remedy: no two Python processors share an interpreter.
  [importable-python-library — SHIPPED #1707, #1708; helper-process-placement-only —
  SHIPPED #1714]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_the_gil_is_released_while_run_blocks -->
- **DECIDED** — The wheel carries an interpreter-lifecycle contract: `rt.run()` owns
  SIGINT while it blocks (Ctrl-C returns cleanly and restores CPython's handler), and
  engine teardown strictly precedes interpreter finalization — all engine threads
  joined, or abandoned and named, and every anchored thread state released before
  `rt.run()` returns, with an `atexit`/context-manager guarantee on the exception path.
  Proven against a hand-run `python <script>.py` harness.
  [importable-python-library — SHIPPED #1707]
- **DECIDED** — `rt.run()` owns SIGINT, SIGTERM and SIGHUP through the whole teardown,
  engine drop included, and escalates on repeat: the first interrupt stops the graph
  gracefully; the second forces it — every helper's ladder skips to terminating its process
  group, and a native processor thread still inside its callback is abandoned; the third
  kills every helper's process group and exits with status 130 at once. A native processor
  thread that ignores shutdown past its budget is abandoned rather than joined: the engine
  stays alive beneath it, and `run()` raises naming the processor. An engine-chosen watchdog
  of about fifteen seconds ends a teardown hung anywhere else. The `run()` docstring states
  the same.
  Four readings the build settled. The watchdog arms when *any* engine teardown starts —
  `run()`'s, `shutdown()`, context-manager exit, `atexit` — and on expiry logs what is still
  running and ends the process with status 124, distinct from the third interrupt's 130; an
  embedding host (Isaac Sim, a notebook) therefore loses its interpreter, accepted so that
  nothing hangs the app. `run()` raises `RuntimeError` naming each abandoned processor by
  display name and id — every other `run()` failure already raises that type, and the CLI
  already reports it as a launch error — while a forced shutdown that abandoned nothing
  returns normally. Abandoning is what keeps the engine alive: the thread holds the runner
  and the engine is deliberately never dropped before process exit, so a thread that returns
  late runs neither tokio shutdown, nor the fd restore, nor device wait-idle on its own
  thread during interpreter finalization. And signal ownership stays scoped to `run()`:
  a teardown outside it — `shutdown()` before a run, `Drop`, `atexit`, `__exit__` — owns no
  signals, and the watchdog alone bounds it.
  [shutdown-ladder; local-transport-hardening — SHIPPED #2266; amended by one-runtime-per-machine: an installer-registered per-user service starts the runtime, which never detaches itself]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_ctrl_c_exits_cleanly -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_sigint_is_handed_back_to_cpython -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_a_second_ctrl_c_forces_the_shutdown_past_a_long_teardown -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_a_third_ctrl_c_kills_every_helper_process_group_and_exits_130 -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_sighup_tears_the_graph_down_gracefully -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_a_runtime_held_by_a_live_thread_is_torn_down_at_exit -->

## Distribution & versioning — IN-FLIGHT (→ package-split-and-lend, runtime-hosting)
<!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py -->

- **DECIDED** — Two artifacts, one version, released together: the streamlib wheel
  (Python API + CLI + engine) and the `streamlib` crate for Rust apps. Initial
  release channel is this repo's releases served through a static PEP 503 simple
  index (`pip install streamlib --index-url …` — one stable incantation) — PyPI
  publication waits for the project rename; the artifact is identical either way.
  Positioning is "realtime engine, Python authoring" — the Rust engine is named as
  material; never marketed as "a Python library" even though the shape is one.
  [importable-python-library — SHIPPED #1691, #1692, #1694, #1711; amended by
  one-runtime-per-machine: two distributions, a stream package and a runtime package; amended
  by tatolab-names: the crate is `tatolab-stream`; amended by package-split-and-lend: one
  version for everything (the entry below)]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli.py::test_the_scaffold_pins_streamlib_to_its_own_index -->
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
  refusal names the runtime's own version. Owner, 2026-10-02. [package-split-and-lend]
- **DECIDED** — Wheel portability model: what the host may supply is stated per platform,
  and nothing else is linked. On Linux, system libraries (Vulkan loader, window system,
  libcuda) are dlopen'd at runtime, never linked — the wgpu/opencv-python manylinux shape.
  On macOS a stock machine has no Vulkan driver, so the wheel carries one in
  `tatolab/runtime/_vulkan_driver/`: the Vulkan loader (built from source at the wheel's
  deployment target), MoltenVK and its unedited ICD manifest, and still links only
  `/usr/lib/` and `/System/`. Engine and helper alike dlopen that loader by absolute path,
  found beside the `_engine` image through `dladdr`, after the bare names and `VULKAN_SDK`
  and before the Homebrew prefixes, which are developer-machine fallbacks.
  `tatolab/runtime/__init__.py` names the bundled manifest to the loader additively — through
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
  adapter closure excludes skia. Helper processes import the wheel itself — one native
  artifact, no separate helper cdylib. The portability proof parses ELF and Mach-O itself:
  every Mach-O the installed wheel carries links only the system, carries
  `LC_CODE_SIGNATURE` — verified by `codesign --verify --strict` where the host has it, since
  a byte rewritten after signing keeps the load command — and names in `LC_BUILD_VERSION` a
  macOS no newer than the wheel's tag. A binary it cannot parse, a fat binary, or a
  `.so`/`.dylib` that is neither format fails rather than skips.
  [importable-python-library — SHIPPED #1691, #1692; python-kernel-surface — SHIPPED #1775;
  macos-platform-floor — SHIPPED #2362]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py::test_the_native_extension_links_nothing_the_host_may_not_supply -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py::test_the_glsl_compiler_is_linked_statically -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py::test_every_mach_o_the_wheel_carries_is_portable -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py::test_every_native_binary_the_wheel_carries_parses -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py::test_an_unsigned_binary_is_caught -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py::test_a_binary_needing_a_newer_macos_than_the_tag_is_caught -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_bundled_vulkan_driver.py -->
  <!-- verify: cargo test -p streamlib-consumer-rhi --lib vulkan_loader_library -->
- **DECIDED** — The macOS artifact is one `aarch64-apple-darwin` wheel at the same abi3
  floor, released beside the manylinux one at the same version. Apple Silicon only: no
  Intel wheel, no universal2, and Rosetta is not a supported path — not as a fallback, not
  as a courtesy (owner, 2026-09-19). It is built on a pinned `macos-15` runner, because the
  image decides the SDK. The deployment target is macOS 15.0, pinned once in
  `[tool.maturin.target.aarch64-apple-darwin]` and exported as `MACOSX_DEPLOYMENT_TARGET`,
  because maturin tags the wheel with it but does not hand it to the link; the bundled
  loader is built to it. 15.0 is the newest target that runner can both build for and run,
  clears MoltenVK's floor (12) and AVFoundation's `AVCaptureDeviceTypeExternal` (14), and
  every Apple Silicon Mac can run it; a Mac on 14 or older gets no matching distribution.
  No notarisation and no signing identity: nothing pip delivers is quarantined. Every
  post-link rewrite of a shipped binary — `lipo -thin` on MoltenVK — is re-signed ad hoc
  (`codesign -f -s -`) in the step that rewrites it, and signatures are verified in the
  built zip and again after install. The same workflow runs on every PR on a runner with no
  Vulkan SDK: it builds the wheel, checks signatures, runs the portability, notices and
  driver-search tests against the installed wheel, and runs the `--test-pattern` scaffold
  for twenty seconds, which must open the carried loader, raise nothing from the effect and
  show at least sixty frames. On release it attaches the wheel, and a failed macOS wheel
  withholds the simple index exactly as the manylinux one does.
  [macos-platform-floor — SHIPPED #2362]
  <!-- verify: grep -n 'macos-deployment-target = "15.0"' sdk/streamlib-python-wheel/pyproject.toml -->
  <!-- verify: grep -n "runs-on: macos-15" .github/workflows/macos-wheel.yml -->
  <!-- verify: grep -n "build-macos-wheel" .github/workflows/release-wheel.yml -->
- **DECIDED** — The wheel is built, tested and linted on the macOS lane, not excluded from
  it. `Rust Build (macOS)` compiles the whole workspace with the wheel in it (clippy on
  default targets, `check --all-targets`) and runs the wheel crate's unit tests; it then
  `maturin develop`s the wheel, gates `_engine.pyi` with `stubtest` against the macOS
  binary — one stub for both floors — and runs the Python suite's GPU-free half. The test
  target's pipe is `std::io::pipe()`, and `check-no-inheritable-descriptor` accepts that
  portable spelling for its two pipe entries, with every Linux flag it accepts or refuses
  unchanged; on Darwin that pipe is `pipe` then `fcntl`, not atomic, and the spawn-side
  answer to the race is `posix_spawn` (#2368). `lint-logging` evaluates `cfg` for macOS
  too, so `apple/` is linted. A test absent on macOS carries
  `linux_only_capability(reason=…)`, held to the closed list §Product states. The Python
  suite's `requires_gpu` half does not run on the macOS lane — it runs on each floor's rig;
  the runner's paravirtual Metal device serves the in-process adapter tests below.
  [macos-capability-parity — SHIPPED #2400]
  <!-- verify: cargo run -p xtask -- check-no-inheritable-descriptor -->
  <!-- verify: cargo test -p xtask check_no_inheritable_descriptor::tests::the_portable_std_pipe_is_accepted_and_a_refused_pipe_names_it -->
  <!-- verify: pytest sdk/tatolab-stream/tests/test_platform_markers.py -->
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
  <!-- verify: cargo test -p streamlib-adapter-vulkan -p streamlib-adapter-cpu-readback -->
  <!-- verify: grep -n "Absent on macOS" adapters/streamlib-adapter-opengl/src/lib.rs adapters/streamlib-adapter-cuda/src/lib.rs -->
  <!-- verify: grep -n "p streamlib-adapter-skia" .github/workflows/test.yml -->
- **DECIDED** — streamlib ships under a `tatolab.*` namespace, as a pure-Python stream package
  and a native runtime package, with support for Linux and Apple Silicon macOS and no
  streamlib-owned service (sentence 5, its "host" read as the runtime — the per-user service that
  keeps the runtime alive is registered by the installer, never by a streamlib package). The stream
  package and packs go through the package index; the runtime package ships inside the installer
  with the CLI and the desktop app, never through pip (owner, 2026-09-30). Known: maturin ships a native
  portion of a PEP 420 namespace beside a pure one, in wheels and editable installs alike, so no
  custom module system is needed. [one-runtime-per-machine; package-split-and-lend]

## Control plane & observability — IN-FLIGHT (→ stream-graph, runtime-hosting)
<!-- verify: cargo test -p streamlib-api-server tools_list_advertises_exactly_the_control_vocabulary -->

- **DECIDED** — The control plane carries no optional capability's routes natively. A
  capability extension that needs an endpoint contributes it through the `host` door
  (§Packages & extension model), served by the one control plane in the app process
  under the same `RuntimeOperations`-shaped discipline — a handler sees what the app
  process sees, the graph and what the extension registered, and no helper's private
  state. `graph` carries an `extensions` key: what loaded, one entry per capability with its
  name, version and distribution. The door's spelling is the first extension's to bring when
  it needs one.
  [extension-model; networking-extension-wheels — SHIPPED #2149, #2153]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-09-05-networking-extension-wheels.md -->

- **DECIDED** — One control plane: the api-server's HTTP + WebSocket + MCP surface,
  hosted in-process by any runtime that enables it. The MCP tool set is the canonical
  control vocabulary; the CLI is a pure JSON-RPC client of it — agents and humans use
  the same verbs; REST/WS routes serve the same operations for programmatic clients.
  The served MCP tool set is exactly the observation verbs `graph`, `tap`, `logs`,
  `exchange` and `shutdown` beside the four graph-mutation verbs the engine's runtime
  API has always carried — `add_processor`, `remove_processor`, `connect` and
  `disconnect`; `health` is a REST route and `nodes` a registry surface, neither of
  them a tool. Rejected: a control plane that never mutates the graph, code being the
  source of truth and the edit loop `dev` — a dynamic graph an agent cannot add to is not
  worth having (owner, 2026-09-06). A mutation compiles inside the call, so the caller learns whether
  its change took rather than reading a `Running` node with nothing flowing. A Python
  class is named by its import path — its descriptor registered when its decorator ran,
  its constructor at this first add exactly as a load supplies it; the app process
  imports the class, the processor runs in its own helper process — and a native
  built-in by the path `graph` reports for one;
  a link wired onto a running processor reaches it, and every channel is sized for a
  destination that connects later. `exchange` stays an observation verb because a read
  that costs the node a bounded copy is still a read. MCP is
  served by the node's local API, mounted with the node and sharing its lifecycle, through
  `rmcp`, the official Rust MCP SDK — no MCP protocol logic is hand-written, in the runtime
  or the CLI's client, which is `rmcp`'s client — in two framings over one handler:
  `POST /mcp`, `rmcp`'s Streamable HTTP service with its header checks and statuses, for
  the CLI's one-shot calls; and `/mcp/stdio`, an HTTP/1.1 `Upgrade: mcp-stdio` after which
  the connection carries MCP's stdio framing both ways, for the `mcp` verb. Beside its
  tools the node serves two resources, each rendered at the moment it is read:
  `streamlib://node-catalog`, every node type it can add with its description, derived
  config schema and ports — `/api/registry`'s document — and `streamlib://graph`, the
  `graph` tool's. It also serves four prompts, recipes rendered against the live graph and
  the catalog — `insert_node_between_linked_nodes`, `fan_output_to_another_consumer`,
  `show_channel_on_virtual_camera` and `look_at_what_a_channel_carries` — whose every
  step is a call to a served tool: a prompt is text, never a mutation path, so the tool
  set stays the whole of the control vocabulary.
  [importable-python-library; mcp-served-with-the-node — SHIPPED #1712;
  agent-readable-processor-catalog — SHIPPED #2232;
  control-plane-surface-pixel-exchange — SHIPPED #1972, #1974; local-transport-hardening —
  SHIPPED #2263, #2265; local-api — SHIPPED #2660, #2665, #2667; amended by
  moq-on-the-tailnet: `tap` and `exchange` leave the tool set at the sharing step]
  <!-- verify: cargo test -p streamlib-api-server the_upgraded_stream_serves_the_same_tools_and_resources_as_post_mcp -->
  <!-- verify: cargo test -p streamlib-api-server tools_list_advertises_exactly_the_control_vocabulary -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_live_graph_mutation.py -->
  <!-- verify: cargo test -p streamlib-api-server resources_list_names_the_node_catalog_and_the_live_graph -->
  <!-- verify: cargo test -p streamlib-api-server every_step_of_every_prompt_calls_a_tool_the_node_serves -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_newest_and_an_ordered_consumer_share_one_running_output_port_each_at_its_own_depth -->
- **DECIDED** — The local API speaks the graph's words: a tool's argument is spelled as `graph`
  renders the same thing, and no tool, result, resource or prompt says "processor" or "display
  name". `graph` renders a node's `name`, and each link end as `{node, port}`, with no
  `processor_id` beside it. The graph-mutation tools are `add_node(type, config, name)`,
  answering the name the node received; `remove_node(name)`; `connect`, each end given as
  `<end>_node` and `<end>_port`, answering a `link_id`; and `disconnect`, taking a `link_id`.
  Every tool
  addresses a node by its name within its stream, never by an id; `graph` still renders a
  node's `id` as a live key. While a runtime holds one stream, the name alone is the node; once
  one runtime hosts several, every tool that names or adds a node also names its stream, as the
  address does, spelled with the stream actions by the change that builds them. `tap`'s
  channel is `<runtime_name>/<node>/<port>`, the name read from `graph`'s top-level
  `runtime_name`; the catalog resource is
  `node-catalog`, listing `nodes`, each under the `type` `add_node` takes; the instructions and
  the prompts say node — `insert_node_between_linked_nodes` among them. `runtime_name` stays
  until one runtime hosts several streams and the address gains its machine and stream. The
  engine's Rust identifiers keep "processor" until the rename. It ships with the graph's one
  shape. Owner, 2026-10-02. [local-api; stream-graph; zenoh-and-moq-wheel-removal — SHIPPED
  #2643, #2645; amended by moq-on-the-tailnet: `tap`'s channel goes with `tap`, at the sharing
  step]
  <!-- verify: cargo test -p streamlib-api-server tools_call_connect_states_the_link_id_and_the_links_state -->
  <!-- verify: cargo test -p streamlib-api-server tools_call_connect_resolves_each_ends_node_by_its_cast_name -->
  <!-- verify: cargo test -p streamlib-api-server tools_call_disconnect_takes_a_link_id_alone -->
- **DECIDED** — `graph` carries the runtime's name as a top-level key, `runtime_name`, beside
  `nodes`, `links` and `extensions` — in the OpenAPI schema, the MCP tool, the generated schema
  and the prompt fixture — because `tap`'s channel is spelled from it and `tap` stays until the
  sharing step. `streamlib nodes` prints the registry table alone. Every
  channel reserves one subscriber slot beyond its fixed cap, `tap`'s, and no other. The key
  goes with the runtime name at runtime hosting. [zenoh-and-moq-wheel-removal — SHIPPED #2643,
  #2645]
  <!-- verify: cargo test -p streamlib-engine --lib core::json_schema::capability_extension_and_runtime_name_rendering_tests::the_runtime_name_is_a_top_level_key_and_no_mesh_key_renders -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::channel_max_subscribers_is_the_fixed_cap_plus_the_taps_reservation_and_refuses_past_it -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py::test_nodes_prints_the_registry_table_alone -->
- **DECIDED** — The api-server is engine-side infrastructure and relocates into the
  `runtime/` tree: it is a host — statically linked, never dlopen'd. Its new host is
  the wheel (and the `streamlib` crate for Rust apps); the relocation is a sequencing
  prerequisite of the rip-out. [control-plane-one-surface]
- **DECIDED** — The CLI ships inside the wheel and slims to `new` / `dev` / `run` (a
  thin runner over the same engine the wheel exposes) plus the observation verbs
  (`nodes` / `graph` / `tap` / `logs` / `exchange`), the `mcp` verb, and one machine-setup verb,
  `enable-virtual-camera`, which installs the loopback permission the virtual camera's
  loopback door needs behind the desktop's password prompt and touches no node.
  `exchange` takes a surface id, or a
  channel: the channel form composes tap → decode → exchange client-side in one warm
  process — one connection, the exchange fired the moment the bag lands, `--count` and
  every-Nth sampling as client flags. It is the cold-spawn latency fix and the
  throttling surface in one, and it adds nothing to the engine: the CLI stays a pure
  JSON-RPC client composing the same two operations any consumer composes. Python embeds
  the engine in-process via
  the wheel; the control plane exists to observe and drive *running* nodes, not to
  embed.
  [importable-python-library — SHIPPED #1683, #1711; importable-python-library-ripout
  — SHIPPED #1715; control-plane-surface-pixel-exchange — SHIPPED #1975;
  virtual-camera-sink — SHIPPED #2196; local-api — SHIPPED #2667; amended by
  one-runtime-per-machine: an installer-registered per-user service starts the runtime,
  which never detaches itself; amended by tatolab-names and
  package-split-and-lend: the CLI becomes the native `tatolab`, shipped with the runtime by the
  installer and never in a pip wheel; amended by moq-on-the-tailnet: `tap` and `exchange` leave
  the CLI at the sharing step]
  <!-- verify: sdk/streamlib-python-wheel/tests/test_cli.py::test_this_wheel_is_the_only_streamlib_cli -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py::test_the_channel_form_taps_then_exchanges_each_sampled_id -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_mcp_verb.py -->
- **DECIDED** — One engine-resolved runtime directory holds everything a live runtime puts
  on disk that means nothing once its processes are gone: the node registry, the
  surface-sharing socket, the local API socket and the iceoryx2 domain. On Linux it is
  `$XDG_RUNTIME_DIR/streamlib/` when that variable is set and non-empty, and otherwise —
  empty or unset, and on macOS always — `/tmp/streamlib-<uid>/`, created owner-only and
  checked as a real directory this uid owns with no group or other bits. The check runs once
  as the runtime starts, before its first node, socket or registry write, and a failure
  refuses the start by name; every user takes the resolved directory. No StreamLib variable
  overrides it — a container or CI job sets `XDG_RUNTIME_DIR` — so a runtime starts anywhere
  with nothing set, and the wheel's Python registry reader resolves identically. What a runtime *keeps* — logs, caches — stays under the project's
  `.streamlib/`. Node discovery is the per-user on-disk registry inside that directory: one
  JSON file per live node, written only by runtimes hosting their local API, pruned only
  when both liveness signals (a `graph` round trip over the socket, process check) fail.
  The entry carries the runtime's own `runtime_name` beside its `runtime_id`, taken from
  the runtime rather than from the local API it hosts, which carries no name of its own,
  and the `local_api_socket_path` a client dials; a reader refuses an entry of an earlier
  schema by name. Owner, 2026-09-14. [control-plane-one-surface; local-transport-hardening
  — SHIPPED #2261; runtime-mesh — SHIPPED #2282; local-api — SHIPPED #2660, #2663]
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::streamlib_runtime_directory -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_runtime_directory.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py::test_a_schema_two_entry_is_refused_by_its_version_and_never_pruned -->
- **DECIDED** — Observability: the JSONL log schema is a durable contract; tap forwards
  bags verbatim, trading completeness for guaranteed non-interference; graph and health
  inspection ride the same control plane. [control-plane-one-surface; amended by moq-on-the-tailnet: `tap` is deleted at the sharing step; logs, `graph` and health stand]
- **DECIDED** — The control plane exposes one composable door for pixels: `exchange`
  takes a published surface id and hands back that frame's image bytes, out of process,
  with no window in the graph and no display server in the path. It is its own verb,
  peer to `graph` / `tap` / `logs` — never a mode of any of them. Tap keeps exactly its
  shipped contract: bags forwarded verbatim as bytes, no decode, no field named, no new
  argument. Its non-interference guarantee is untouched because the two doors touch
  different systems — tap's guarantee is about the channel (one reserved subscriber
  slot, completeness traded away and reported as `dropped_bags`), while the exchange
  never attaches to a channel and is a pool consumer on the same terms as any typed cast
  in a downstream processor, one frame at a time. Composition happens entirely at the
  consumer: it decodes the bag itself, reads whatever field it knows carries a surface
  id, and calls `exchange` with that id. The engine therefore still inspects no bag
  content anywhere. This is how verification sees pixels, and equally how any API
  consumer sees them, because the door knows nothing about verification.
  [control-plane-surface-pixel-exchange — SHIPPED #1972; amended by moq-on-the-tailnet: `exchange` is deleted at the sharing step, the repo's
  verification reading a private port's snapshot instead]
  <!-- verify: cargo test -p streamlib-api-server the_tap_tool_schema_is_unchanged_by_the_exchange_joining_the_catalog -->
  <!-- verify: cargo test -p streamlib-engine --lib a_published_pool_frame_exchanges_through_the_runtime_operation_for_its_own_pixels -->
- **DECIDED** — The exchange is a pool claim, bounded to the copy. Inside one operation
  call: resolve the id, claim the frame through the pool's own claim seam (the refcount
  in-process, the checkout lease cross-process — the shipped seam, never a new one), run
  the GPU conversion and the GPU→CPU copy under the claim, release, then encode and
  return. Encoding happens after the release, so the claim window is the copy alone and
  an encoder's cost can never extend it. The producer never waits regardless: the pool
  skips claimed slots and grows to its cap, so an exchange costs the node memory at
  worst, never another processor's cadence. Without the claim a producer could recycle
  the slot mid-copy and the caller would receive a torn frame — half one frame, half the
  next — which is precisely the silent wrongness the surface-id lifetime contract exists
  to kill. [control-plane-surface-pixel-exchange — SHIPPED #1972; amended by moq-on-the-tailnet: `exchange` is deleted at the sharing step, the repo's
  verification reading a private port's snapshot instead]
  <!-- verify: cargo test -p streamlib-engine --lib sequential_exchanges_of_one_frame_never_pin_more_than_one_hold -->
- **DECIDED** — Staleness fails loud and composes as a retry, never as wrong pixels. A
  surface id is per-frame (`<slot>#<generation>`), and resolving a retired one is refused
  with the recycled-frame error before any bytes move — `410 Gone` over REST, never a
  `200` carrying the slot's newer pixels. So when an exchange succeeds the bytes are
  exactly the tapped bag's frame — the generation grammar is what proves the pairing —
  and when it is too slow the caller taps a newer bag and exchanges that.
  Sample-and-exchange-as-you-go is therefore the intended loop, and temporal sampling
  falls out of composition rather than needing a batched verb. The publish-to-claim
  window is the one every pool consumer already obeys: it rides pool depth, and
  outwaiting it is an error. [control-plane-surface-pixel-exchange — SHIPPED #1972; amended by moq-on-the-tailnet: `exchange` is deleted at the sharing step, the repo's
  verification reading a private port's snapshot instead]
  <!-- verify: cargo test -p streamlib-engine --lib a_retired_frame_id_is_refused_at_the_exchange_naming_the_recycling -->
  <!-- verify: cargo test -p streamlib-api-server tools_call_exchange_on_a_recycled_frame_is_a_tool_error_naming_the_recycling -->
- **DECIDED** — The engine converts, in the RHI, or the caller gets nothing viewable: a
  camera frame is NV12 or YUYV and converting it is the RHI's existing job, while
  readback is an always-present `GpuContext` capability. No pixel conversion happens
  outside the RHI and no second converter is built. The operation reaches the engine
  through `RuntimeOperations` and nothing else — the api-server's HTTP task deliberately
  holds only `Arc<dyn RuntimeOperations>`, the trait gains one operation, and `Runner`
  implements it over the pool's own claim and the RHI's color converter, blit and texture
  readback, on Linux and macOS alike; it never rode the export staging on either floor. On
  macOS it runs the same conversion under MoltenVK, a pooled frame's IOSurface pages reaching
  the GPU through their host-pointer import, never a CPU read of the surface — so `streamlib
  tap` → `exchange`, and the repo's own live verification with it, answer on a Mac. No new
  surface-resolution path exists — the backing resolution is the one `ResolvedSurfaceBacking`
  — and the caller needs no Vulkan device, no surface
  socket and no runtime link.
  [control-plane-surface-pixel-exchange — SHIPPED #1972; macos-capability-parity — SHIPPED
  #2406; amended by one-runtime-per-machine: accelerators are optional; amended by moq-on-the-tailnet: `exchange` is deleted at the sharing step, the repo's
  verification reading a private port's snapshot instead]
  <!-- verify: cargo test -p streamlib-engine --lib a_pooled_rgba_frame_exchanges_for_the_pixels_the_bag_published -->
  <!-- verify: cargo test -p streamlib-engine --lib a_texture_backed_frame_exchanges_for_the_pixels_its_producer_rendered -->
- **DECIDED** — Two spellings of one operation: MCP tool and REST route serve the same
  `exchange` with the same arguments, differing only in result shape. REST serves the
  exact frame as a binary `image/png` body — lossless, full resolution, no base64
  inflation: the evidence and PSNR path, and what the CLI writes into a
  caller-named directory. The MCP tool returns an image content block, downscaled by
  default to a declared long-edge cap (~1568 px, the resolution ceiling vision models
  actually use), with the result stating the true extent and the exact-bytes route — the
  agent's in-session view, always under the per-image payload ceiling. The downscale
  rides the RHI's existing blit path, never a second scaler, and raw unconverted planes
  stay deferred until something needs them. Both spellings are gated as the rest of the
  local API is, by the auth entry below. [control-plane-surface-pixel-exchange — SHIPPED
  #1972, #1974; local-api — SHIPPED #2663; amended by moq-on-the-tailnet: `exchange` is
  deleted at the sharing step, the repo's verification reading a private port's snapshot
  instead]
  <!-- verify: cargo test -p streamlib-api-server the_exchange_route_answers_the_operation_bytes_verbatim_as_an_image -->
  <!-- verify: cargo test -p streamlib-api-server tools_call_exchange_states_the_true_extent_the_id_and_the_exact_bytes_route -->
- **DECIDED** — There is no observer effect: reading a channel does not require terminating
  it in a window, so a mid-graph channel is observable in the topology that ships. Window capture survives only where
  the window is genuinely the subject — the present and swapchain path.
  [control-plane-surface-pixel-exchange — SHIPPED #1972, #1976; amended by moq-on-the-tailnet: once `exchange` is deleted at the sharing step the same holds through a private port's snapshot — a port is read with no node added to the graph]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-26-control-plane-surface-pixel-exchange.md -->
- **DECIDED** — Auth and remote-access posture: whoever can open the local API's socket may
  call it — the owning user, by file permission — and nothing off the machine can call it at
  all, so control carries no token and no account. What a runtime offers other machines is
  exposure, decided in §Networking. Owner, 2026-10-01.
  [control-plane-bind-posture; local-api — SHIPPED #2663]
  <!-- verify: cargo test -p streamlib-api-server the_socket_is_only_its_owners_to_open -->
- **DECIDED** — The local API is reachable only on its own machine. Each machine's runtime
  serves one local API on a socket in its runtime directory that only the owning user can
  open — `local-api-<runtime_id>.sock`, chmod 0600 inside the 0700 directory, a live
  duplicate refused by a connect probe naming the path and a stale file replaced — carrying
  the router and control vocabulary unchanged; no network address serves control. A client
  picks a runtime by `--node <runtime name or id>`, or takes the sole live one. The URL forms are a separate listener that changes nothing — private and public
  ports on loopback — because browsers and ffmpeg cannot dial a socket. A runtime is never
  driven from another machine
  through its local API: changing a stream on another machine means running the CLI or an
  agent on that machine, over ssh for example, and a fleet-wide path is the external control
  client's. Owner, 2026-10-01. [one-runtime-per-machine; local-api — SHIPPED
  #2660, #2662, #2663; amended by moq-on-the-tailnet: the URL listener's tailnet half,
  through `tailscale serve`]
  <!-- verify: cargo test -p streamlib-api-server serving_the_router_on_the_local_api_socket_opens_no_tcp_listener -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_launch.py::test_a_launched_node_listens_on_no_tcp_socket -->
- **DECIDED** — An MCP host reaches the local API by launching the CLI's `mcp` verb as a
  stdio server — `claude mcp add streamlib -- <cli> mcp`. The verb resolves the runtime,
  opens its socket, sends the one `/mcp/stdio` upgrade request, and then copies bytes,
  stdin → socket and socket → stdout; it parses no message, so the tools, resources, prompts
  and protocol revision stay the runtime's alone. Stdin closing half-closes the socket and
  the verb exits when the runtime closes its side; the runtime going away exits it non-zero
  with one stderr line naming the runtime; no live runtime at launch is a one-line refusal
  naming `streamlib nodes`. Run over ssh — `ssh <machine> <cli> mcp` — it is how an agent
  changes a stream on another machine. No network listener serves MCP. The runtime serves
  only the latest MCP revision `rmcp` speaks, 2026-07-28 (stateless: no `initialize`
  handshake and no session); a host that speaks only an earlier revision is refused with the
  protocol's own unsupported-version error naming the served revision.
  `subscriptions/listen` is acknowledged with an empty set — `listChanged` stays `false` —
  and held until cancelled or the local API stops. Agents hosted in a cloud, which can
  neither launch a command nor reach a machine's loopback, are not served by the local API.
  Owner, 2026-10-01; `rmcp`, owner, 2026-10-06. [local-api — SHIPPED #2665, #2667]
  <!-- verify: cargo test -p streamlib-api-server an_initialize_handshake_is_refused_with_the_unsupported_version_error_naming_the_latest -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_mcp_verb.py::test_the_verb_opens_the_stream_with_one_upgrade_and_copies_both_ways_untouched -->
- **DECIDED** — `graph` returns every stream the runtime holds, and every stream action the CLI
  has — `run` attached, `run -d`, `stop`, `start`, `rm`, `streams`, `expose` — is also a tool,
  beside the graph-mutation tools, which stay, so an agent can do whatever the CLI can; the
  change that builds them spells them. One runtime per machine owned by one user (§Product)
  means one socket per machine. Owner, 2026-10-01; confirmed 2026-10-02. [runtime-hosting;
  local-api]

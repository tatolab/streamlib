# package-split-and-lend

Step 3 of the one-runtime-per-machine pivot: the Python library's rename, its split, and the
runtime becoming a program of its own. After it:
- `tatolab-stream` (`tatolab.stream`) is pure Python; a stream's venv installs it, every stream
  and node module imports from it, and a stream imports, type-checks, compiles and is tested
  with no engine anywhere;
- `tatolabd` is a native binary: the runtime process, hosting the engine for one stream, started
  in the foreground by `tatolab run`, running no Python; `tatolab` is a native CLI beside it;
- `tatolab.runtime` — the bindings a node's calls go through, and the bootstrap — sits in a lend
  directory beside them; each processor interpreter starts from the stream's venv interpreter
  with that directory prepended to `PYTHONPATH`, the build-id check kept;
- the runtime process imports no project code: a node's ports come from the stream's own
  interpreter, and compiling happens in the project's interpreter;
- the runtime refuses by name what a graph holds that it does not understand, and loads every
  graph an older stream recorded;
- the tests divide as the code does — a stream suite with no runtime, a runtime suite with no
  Python node, an integration suite where running both is the point;
- no package extends the engine: the capability-extension hook is deleted (decision 2);
- pip publishes `tatolab-stream`, `tatolab-webrtc`, and no engine.

Unchanged, mapped below: one stream per runtime process, no service and no installer, the Rust
crate names of the engine, the `STREAMLIB_*` engine variables.

**Scale gate — this skill, plus the existing ADR.** The Python API's public contract, the
processor model and distribution all move. The rationale is
`docs/decisions/package-split-and-lend.md` (#2583), which this PR extends with decisions 1
and 2 and their rejected alternatives.

**Decision 1 — RESOLVED (c), owner, 2026-10-02:** the runtime process is native `tatolabd` now,
"especially for agent development": no launcher and no runtime-side Python environment are
built to be thrown away. Owner, same day: long term, installing and managing the runtime moves
into the app; running it as a service now would cause chaos — so `tatolabd` here is started in
the foreground by `run`, one per stream, and nothing registers a service.

**Cleaning as we go — the rules this change holds itself to** (owner, 2026-10-02): every slice
deletes what it replaces in the same PR, its tests included; nothing is built as a bridge; every
concept has one declaration — a second form of it is generated and gated, never hand-held equal;
the inventory below names every piece of the old shape and the slice that ends it; code moves
into the target layout below, never beside it.

**Precondition.** Every entry built is DECIDED: §Packages `ARCHITECTURE.md:516-534` (the split
and the lend), `:538-550` (the names), `:159-170` (the handshake re-read), `:281-294` (an
extension depends on `tatolab-stream`); §Product `:107-113` (pip never carries the runtime),
`:114-129` (where no installer put a runtime, `tatolabd` runs in a terminal); §Processor model
`:1065-1080` (the exec, amended), `:1260-1270` (the build-id check), `:1361-1365` (the
environment beside the graph); §Distribution `:4254-4262`. Not built against: needs
`:1366-1373`, packs `:551-566`, the control client `:535-537`, several streams per runtime
(step 4). **Sequencing:** after #2569 and #2575. #2576 (`streamlib mcp` in the Python CLI) would
be rewritten by S5 — re-pointing it at the native CLI is a tracker call for `/derive-tickets`.

**Verified against the tree 2026-10-02 (HEAD c3d123d37).** Four read-only sweeps.
- Importing any `streamlib` module loads `_engine` (`__init__.py:28-78`); `@processor` calls the
  native `register_declared_processor_class` (`_processor_declaration.py:431`), so no node
  module imports without the engine. The thirteen built-ins are marker pyclasses
  (`src/python_native_builtin_blocks.rs:18-96`) whose `type` is the Rust `module_path!()`
  (`sdk/streamlib-macros/src/codegen.rs:412-419`) and whose config is an untyped dict described
  in prose (`_engine.pyi:97-494`); the Rust `Config`s derive `JsonSchema` but not
  `deny_unknown_fields` (`camera_source.rs:34-37`), so a misspelled setting is dropped at
  `processor_instance_factory.rs:290-295`. Everything a node is handed is a pyclass;
  `_engine.pyi` (2419 lines) is stubtest-gated at `python-wheel.yml:161`, `test.yml:873`.
- Spawn lives in the PyO3 crate: one `OnceLock` interpreter captured from `sys.executable` and
  `sys.path[0]` (`src/python_helper_process_spawn_host.rs:104-127`), `-m streamlib._helper`
  (`:44`, `:226-274`), `PYTHONPATH` the app directory then the parent's (`:278-289`). The parent
  imports project code to run the entry file (`cli.py:165-183`), to read a stamp
  (`python_processor_declaration.rs:34-171`) and to resolve an MCP `add_processor`
  (`python_runtime_lifecycle.rs:278-298` → `python_processor_registration.rs:183-210`). The
  build id is checked first in `_helper.py:1276-1296`; nothing checks the interpreter's shape.
- The engine, SDK, api-server and built-ins build without Python; the signal ladder is the
  engine's (`core/signals.rs`, `core/runtime/runtime.rs`). No production Rust binary exists.
  `cli.py` is 1440 lines: `new`, `enable-virtual-camera`, `run`/`dev`, `nodes`, `graph`, `tap`,
  `exchange`, `logs`.
- macOS: `dladdr` finds `_vulkan_driver/` beside `_engine`
  (`runtime/streamlib-consumer-rhi/src/vulkan_loader_library.rs:66-95`).
- One venv runs everything: CI `maturin develop`s and runs pytest there
  (`python-wheel.yml:103-110`, `test.yml:862-867`); `build_simple_index.py:31` publishes
  `streamlib` and `streamlib-webrtc`.

---

## Decision 2 — RESOLVED (c): the capability-extension hook is deleted

Owner, 2026-10-02. A native `tatolabd` runs no Python, so the hook's runtime-process call site
(§Packages `:239-275`) could not stand; options were (a) processor interpreters only, (b) an
interpreter embedded in `tatolabd` for hooks alone, (c) no hook. The owner chose (c): the
runtime is the host and streams are its guests, so no package extends the engine — one shared
runtime serves every stream on the machine, and code inside it could crash, read or send out
every stream's data. A package does its own setup where its nodes run, at import or on first
use (the shipped two install a TLS provider and a network thread pool); a node's lifecycle
methods are unchanged; an outside program watches the local API's events. An engine-grade
capability enters as a built-in under `:211-225`. Replaced at the fold: `:192-210` (two
mechanisms), `:239-258`, `:259-275`, `:276-280`, the hook half of `:281-294`, and `graph`'s
`extensions` key. Retired as residue, the questions they asked having no subject left: the
OPENs `:295-298` (how an extension's engine-grade capability is reached) and `:299-304`
(extension native code in the app process). The control-client entry `:514-515` stands; how a
client plugs in stays OPEN `:535-537`, its "role beside today's two" wording residue.

## Target layout

```
sdk/tatolab-stream/                 pure distribution: tatolab/stream/ (no tatolab/__init__.py)
  tatolab/stream/_built_in_nodes.py   generated from the runtime's descriptors, gated on drift
  scaffold_template/                  the files `tatolab new` writes, embedded by the CLI
  tests/                              the stream suite
sdk/streamlib-python-wheel/         PyO3 crate → tatolab/runtime/: processor-interpreter bindings
                                      and bootstrap only (crate name is step 10's)
runtime/tatolabd/                   bin tatolabd — the runtime process
runtime/tatolab-cli/                bin tatolab — the CLI
runtime/streamlib-engine/src/core/compiler/compiler_ops/
                                    processor-interpreter spawn and describe, beside subprocess_bridge.rs
tests/stream-on-runtime/            the integration suite
target/tatolab-runtime/             bin/tatolabd, bin/tatolab, lib/tatolab/lend/tatolab/runtime/ —
                                      the install prefix's own shape, which the installer will copy
```

## ADDED: §Packages — `tatolab.stream`, the stream package

- **Distribution.** A pure build backend; `requires-python >=3.10`; dependencies are what its own
  modules import, never the runtime. A module belongs here iff a stream or node module imports
  it: the declarations (`@node`, `@input`, `@output`, `@stream`, `Stream`, the references,
  `compile_stream_to_graph`), `_node_config_schema`, the data types (`AudioBlock`,
  `EncodedAudioPacket`, `VideoFrame` and its colour types, `EncodedVideoFrame`), the composable
  pieces (`ClaimedSurfacePixelAccess`, `PixelAccessToOneClaimedSurface`, `GlslPixelEffect`, the
  `ModelInputTensor` family, `NodeOutputTextureRing`), `clock`, `log`, and
  `_cross_floor_check`, which the compile entry runs.
- **Built-ins are generated pure classes.** `cargo xtask generate-built-in-node-classes` writes
  `_built_in_nodes.py` from each built-in's descriptor and `Config` `JsonSchema` — its `type`
  (`processor_class_import_path()`), its doc line, a `TypedDict` for its config; CI fails when a
  regeneration differs. One declaration, the Rust one:
  ```python
  class CameraSourceConfig(TypedDict, total=False):
      device_id: str
  class CameraSource:
      """Captures frames from a camera device."""
      type: ClassVar[str] = "<CameraSource's processor_class_import_path()>"
  stream.add(CameraSource, config={"device_id": "/dev/video2"})  # pyright checks the keys
  ```
- **Runtime-backed names are declared once, here**: the contexts, `LinkInputDataReader`,
  `LinkOutputDataWriter`, `NodeLinkDataAccess`, `GpuContext*`, `GpuSurfaceHandle`, the
  kernels, `MonotonicTimer`, the texture exports, `NodeOwnedWindow*`, the bag codec pair, `monotonic_now_ns`,
  `gpu_limited_access_of_the_typed_read_in_progress`, and `start_monotonic_timer(interval_ns)`,
  the one way a node starts a timer — `MonotonicTimer(n)` is gone, with no alias (owner,
  2026-10-07). A class is a `typing.Protocol` carrying today's stub
  signatures; a function resolves the runtime's on first call and, with nothing lent, raises
  `RuntimeError` naming itself and saying it runs in a processor interpreter. `_engine.pyi`
  shrinks to the bootstrap's private surface; a conformance gate holds every pyclass to its
  Protocol member for member, against the signatures pyo3 publishes — the stubtest role, single
  source. Pure pieces wrapping native ones bind them at call time.
- **`@node` registers nothing** — the stamp only; `register_declared_processor_class` goes.
- **The proof** is the stream suite itself (below), run with no runtime on any path.

## ADDED: §Packages — `tatolab.runtime` and the lend directory

- `tatolab/runtime/`, a regular package: `__init__.py` naming the bundled ICD before `_engine`
  loads (today's `tatolab/runtime/__init__.py:17-20`); `_engine` (`module-name =
  "tatolab.runtime._engine"`), the bindings only; `_processor_interpreter_bootstrap.py`
  (today's `_helper.py`); on macOS `_vulkan_driver/`. No `Runtime`,
  no CLI, no `testing`, no control-plane client: the Python-hosted engine is deleted.
- maturin builds it as a wheel that `cargo xtask build-runtime` unpacks into
  `target/tatolab-runtime/lib/tatolab/lend/` beside the two binaries — the runtime unit's one
  build, used by developers, CI and later the installer; never installed, never published. A
  gate fails any `tatolab/__init__.py` in the tree or a built artifact. `dladdr` finds
  `_vulkan_driver/` unchanged.

## ADDED: §Processor model — the environment, the lend at spawn, describe

- **Spawn moves into the engine**, beside `subprocess_bridge.rs`, Python-free: the `OnceLock`
  capture is deleted, and `StreamEnvironment { project_directory, interpreter }` is recorded per
  loaded stream, beside its graph. The command is `environment.interpreter
  <lend>/tatolab/runtime/_processor_interpreter_bootstrap.py`; `PYTHONPATH` = the lend
  directory, then the project directory; `PYTHONHOME` removed; the working directory is the
  project; `STREAMLIB_*` unchanged. Run by path, never `-m` or `-c`, the bootstrap first drops
  its own directory from `sys.path`. `tatolabd` finds the lend directory relative to its own
  executable (`../lib/tatolab/lend`).
- **The bootstrap's order**: (1) import `tatolab.runtime`; an interpreter that cannot load it —
  not CPython, below 3.10, free-threaded, a foreign architecture — writes a refusal to raw stderr
  naming its path, implementation, version, free-threading, architecture and the import error,
  and exits; (2) the build-id check as `_helper.py:1276-1296`; (3) today's sequence.
- **Describe.** The runtime runs the same command with `--describe <import paths>` at load for
  every Python `type` a graph names, and at a live `add_processor` for one not yet described; the
  bootstrap prints today's `PythonProcessorDeclaration` as JSON. A type that will not import or
  carries no stamp is refused by name, quoting the interpreter's stderr. The resolver
  (`register_processor_class_by_import_path`, `install_unregistered_processor_type_resolver_once`)
  is deleted.

## ADDED: §Product — `tatolabd` and `tatolab`

- **`tatolabd --stream-graph <file> --project <dir> --interpreter <path>`** hosts one stream in
  the foreground: the engine and its built-ins, `load_graph_snapshot` with the environment, the
  local API on its socket, the engine's signal ladder, its logs on stderr. No Python in-process.
- **`tatolab`** — today's verbs, native. `run` and `dev`: find `<project>/.venv/bin/python`
  (absent → refuse by name, pointing at `uv sync`), run `tatolab.stream`'s compile entry in it
  with the project as import root (graph JSON on stdout, cross-floor warnings on stderr, a failed
  compile's traceback as the error), start `tatolabd` attached, forward Ctrl-C; `dev` restarts it
  on an edit. `graph`, `tap`, `logs`, `nodes`, `exchange`, `mcp` speak the local API. `new` writes
  the embedded templates with `dependencies = ["tatolab-stream", "numpy>=2.1"]`.

## ADDED: §Processor model — the runtime refuses by name what it does not understand

- Every built-in `Config` takes `#[serde(deny_unknown_fields)]`; the factory's error names the
  node, its `type` and the setting. A `type` the runtime lacks is refused at load naming it; a
  built-in absent on this floor is refused there naming the floor (`VirtualCameraSink` moves from
  `stream.add()` to load). A graph key the loader neither reads as spec nor knows as one of `graph`'s
  live keys is refused by name. A golden graph checked in here, holding every key and built-in
  `type`, loads on every later build; a shape change adds a golden beside it, never edits one.

## ADDED: §Product — the tests divide as the code does

- **The stream suite** (`sdk/tatolab-stream/tests`): pure Python in a venv holding only
  `tatolab-stream` — declarations, the builder and its name resolution, compile, the generated
  built-ins, data types, the cross-floor check, the scaffold templates (import, ruff, pyright).
  Runtime-backed objects are test doubles of their Protocols. Never GPU, never a subprocess of
  the runtime. Both CI lanes.
- **The runtime suite**: Rust — the engine crates, `tatolabd` and `tatolab` — fed graph data and
  native built-ins only; no Python node. `requires_gpu` halves stay rig-only as today.
- **The integration suite** (`tests/stream-on-runtime/`): explicitly both — compile a fixture
  stream, start `tatolabd`, drive it over the local API: processor interpreters, the lend,
  describe, escalate ops, pixel and device exchange, extensions, the CLI end to end.
- The division is structural, not a lint: each CI job installs only what its suite may touch,
  so a stream test reaching for the runtime fails to import. GPU-optional testing later divides
  these three, not one mixed suite.

## MODIFIED: stream-graph (in flight)

- `Runtime.load` is deleted with the Python-hosted engine; `tatolabd` loads a graph and an
  environment. `@stream` functions compile in the project's interpreter (`:533-534`).
- The loader reads the spec keys, skips `graph`'s live keys, and refuses any other.

## MODIFIED: records re-spelled at the fold

- §Packages `:184-191`, `:226-237`, decision 2's entries, `:281-294` — `tatolab-webrtc`,
  depending on `tatolab-stream`. The MoQ wheel is gone (#2633), and the mesh with the mesh
  observation and configuration files the inventory lists (#2643, #2645).
- §Product `:21-31`, `:39-46`, `:60-82` (one suite on both floors → three; the closed list
  refuses at load), `:83-98`. §Processor model `:1065-1080`, `:1260-1270`. §Language SDKs
  `:4095-4142` — the GIL-release contract stays for processor interpreters; `rt.run()`'s signal
  ownership and teardown become `tatolabd`'s, the Python-specific halves deleted.
  §Distribution `:4147-4217` — artifacts, portability over `lend/`, the macOS done-proof (a venv
  holding only `tatolab-stream`, `tatolab new`, `tatolab run --test-pattern` twenty seconds,
  sixty frames). Every `<!-- verify: pytest sdk/streamlib-python-wheel/tests/… -->` is re-pointed
  to the suite its test lands in.
- §Processor model's entry "A Python processor class registers its descriptor … when
  `@processor` runs" (`:1338-1341`) — `register_declared_processor_class` and
  `install_constructor_for_registered_descriptor` are deleted; `@node` registers nothing
  (above), and a type is described at load.
- §Media I/O `:2630-2633` (the audio built-ins) and the codec-blocks entry `:2818-2830`, and
  `docs/plan/diagrams/system.mmd`'s media node — built-ins reach Python as classes generated
  into `tatolab/stream/_built_in_nodes.py` from their descriptors (#2587), not as native marker
  classes, and a built-in's `type` is `tatolab.stream:<Class>` as built.
- `README.md:86-94` and `docs/architecture/` in the shipping tickets; CLAUDE.md's "Reading the
  Python surface", `placement.md` and the skills that spawn `streamlib` in their own
  operating-model PR, as `flow.md` requires.

## Inventory — what the old shape leaves, and the slice that ends it

Every row ends in a deletion, a move into the target layout, or a rewrite, in the named slice and
its PR. Paths under `sdk/streamlib-python-wheel/` unless rooted. One file per row is never left
"for later".

| Old shape | Ends | Slice |
|---|---|---|
| `src/python_runtime_lifecycle.rs` (715 lines, the `Runtime` pyclass: capture, resolver, `load`, control-plane hosting, `run`'s GIL detach and signal hand-back, shutdown, `__enter__`, atexit), `src/python_control_plane_hosting.rs`, `src/python_test_harness_endpoints.rs`, `python/tatolab/runtime/testing.py`, the `Runtime` subclass and `_live_runtimes` (`tatolab/runtime/__init__.py:35-72`) | deleted | S4 |
| `src/python_native_builtin_blocks.rs`, `src/python_processor_registration.rs`, `src/python_processor_import_path.rs` (identity checked at decoration and registration) | deleted; identity is checked by the builder and by describe | S2, S3 |
| `src/python_helper_process_spawn_host.rs` (2074 lines), `src/helper_process_shutdown_ladder.rs` (798) | moved into the engine, Python-free | S3 |
| `src/python_logging.rs:162-186` and `core/logging/mod.rs:31` (app-process Python log records) | deleted; the helper drain stays | S4 |
| The hook: `src/python_capability_extension_host.rs`, `python/tatolab/runtime/_capability_extensions.py`, its two call sites (`Runtime.__init__`, `_helper.py`), `graph`'s `extensions` key in `GraphResponse`, OpenAPI and MCP, `generate_third_party_notices.rs:1277-1312`'s entry-point discovery, `test_capability_extensions`, `extension_fixtures/` | deleted; the notices find `packages/*` by path | S7 |
| `src/python_runtime_mesh_observation.rs`, `cli.py`, `_control_plane_client.py`, `_node_registry.py`, `_runtime_log_reader.py`, `_surface_image_exchange.py` | rewritten in `tatolab`, then deleted | S5 |
| `runtime/streamlib-engine/src/core/signals.rs:9-12` (names CPython), the hand-back bookkeeping | the doc and the dead arm deleted; the ladder is reused as-is through `start_and_wait_for_shutdown` | S4 |
| `_engine.pyi` beyond the bootstrap's surface | replaced by the Protocols and the conformance gate | S2 |
| CI: one venv with `maturin develop` (`python-wheel.yml:101-188`, `:255-305`, `:375-401`, `:436-465`; `test.yml:772`, `:856-883`); `macos-wheel.yml:112-151`; `release-wheel.yml:206-247`; `build_simple_index.py:31`; release-please's wheel `pyproject.toml` bump | three suite jobs and the runtime-unit build; the simple index deleted, the release uploading `tatolab-stream` and `tatolab-webrtc` to PyPI (owner, 2026-10-08) | S2–S5 |
| xtask paths (`check_no_in_process_placement.rs:52`, `:114`; `check_clock_usage.rs:107`, `:628`, `:775`; `lint_logging.rs:50-56`; `check_boundaries.rs:521`, `:988`, `:2223-2268`; `generate_third_party_notices.rs:852`, `:1204`, `:1277-1312`; `main.rs:142`, `:383`) | re-pointed | S1 |
| ≈20 engine fixtures calling `streamlib` or `Runtime` (`runtime/streamlib-engine/tests/fixtures/*.sh`, `*_node.py`) | rewritten to `tatolab` and `tatolabd` | S4, S5 |
| `.claude/` skills (eight live-ops and verify skills), `agents/evidence-verifier.md`, `hooks/rig-brake.sh:107` and its test | corrected in one operating-model PR | after S5 |

**The 64 test files** (34 `requires_gpu`). Each moves, splits or dies in the slice that touches
its subject; no test runs against a path its slice deleted.
- **Stream suite (6 whole):** `glsl_pixel_effect_refusals`, `model_input_tensor_kernel_refusals`,
  `processor_config_class`, `processor_declaration`, `processor_output_texture_ring`,
  `platform_markers`. S2.
- **Integration suite (29 whole):** the kernels, exchanges, casts, links, helper process and
  placement, live mutation, MCP, the CLI launch — the files whose subject is a node running in
  its interpreter. Those that host `Runtime` in the test process (`single_processor_pipeline`,
  `inbound_link_naming` and the `*_app.py` harnesses behind `start_app_under_test`) are rewritten
  onto `tatolabd`. S4.
- **Runtime suite (4):** `runtime_directory`, `wheel_portability`, `third_party_notices`,
  `bundled_vulkan_driver` — following the native artifacts. S3–S5.
- **Deleted (3 whole):** `graph_readiness`, `runtime_mesh_configuration`, `runtime_name` — the
  Python `Runtime`'s own surface; their environment-variable arms return as `tatolabd` tests. S4.
- **Split or deleted (22):** pure halves to the stream suite, the running halves to integration,
  named-device refusals of native built-ins to the runtime suite, and the in-process halves
  deleted — `interpreter_lifecycle:53-319`, `graph_building:42-80`, `:187`, `cli:150-413`,
  `cli_observation_verbs:1222-1238`. S2–S5. `test_capability_extensions`, counted here, is
  deleted whole with the hook, S7.

## Left to later changes

| Not here | Because | Lands with |
|---|---|---|
| `run` loading into the running `tatolabd` instead of starting its own; several streams per `tatolabd`; the state directory; `run -d`; the `<machine>/` segment | step 4 (`:107-129`) | runtime hosting |
| Installing and managing the runtime | owner: the app, long term | step 4's align and the app |
| The engine's Rust crate names, `STREAMLIB_*` | step 10 | the app |
| Needs, packs, the control client, engine-grade extension capabilities | OPEN | their steps |
| The thirteen examples | converted consumers | backlog filed at ship |
| Testing a user's stream without a runtime | separate work (`:127-129`) | its own change |

## Assumptions stated, not asked

- The owner's test division governs this repository's suites; a user-facing way to test a stream
  without a runtime stays the separate work `:127-129` records.
- `<project>/.venv/bin/python` only; a relocated venv is refused by name until a need appears.
- Describe costs one interpreter start per load and per live add of an undescribed type.
- A processor interpreter's working directory becomes the project directory.
- The two extensions move here, the canary §Consumers reserves.
- `cargo xtask build-runtime` and `generate-built-in-node-classes` are permanent builds, not
  bridges; each has one caller set (CI, developers, later the installer) and nothing imports them.

## Slices, as ticketed — each deletes what it replaces, tests included

Derived 2026-10-02, milestone #61; "(ultracode)": `/implement` builds it only with ultracode on.
- **S6 — refusal by name** and the golden graph: #2585. Blocked by #2565.
- **S1 — the namespace:** #2586 (ultracode). Blocked by #2569, #2575.
- **S2 — the stream package stands alone**, as two: the generated built-ins, #2587 (blocked by
  #2586 and by #2585, which moves the macOS refusal off the marker); then the Protocols, the
  gate, `@node` registering nothing and the stream suite, #2588 (ultracode; blocked by #2587).
- **S3 — spawn, describe and the lend in the engine:** #2590 (ultracode). Blocked by #2588.
- **S7 — the extensions and the hook:** #2591. Blocked by #2590; it lands before S4, so
  `tatolabd` is never built around the hook.
- **S4 — `tatolabd`, with S5's `new`, `run` and `dev`** (today's `run` builds the Python `Runtime`
  in its own process, `cli.py:262`): #2592 (ultracode). Blocked by #2590, #2591.
- **S5 — the rest of `tatolab`**, pip ships no engine: #2593 (ultracode). Blocked by #2592 and
  #2576, whose `streamlib mcp` stays as filed and is ported here.
- Operating-model PRs: #2589, CLAUDE.md (after #2588); #2594, skills and hooks (after #2593, #2577).

## REMOVED

- REMOVED: sdk/streamlib-python-wheel/python/streamlib
- REMOVED: streamlib._engine
- REMOVED: streamlib._helper
- REMOVED: streamlib.cli:main
- REMOVED: from streamlib import
- REMOVED: pip install streamlib
- REMOVED: streamlib.extensions
- REMOVED: CapabilityExtensionHost
- REMOVED: register_capability
- REMOVED: _capability_extensions
- REMOVED: register_declared_processor_class
- REMOVED: register_processor_class_by_import_path
- REMOVED: install_unregistered_processor_type_resolver_once
- REMOVED: capture_helper_process_launch_environment
- REMOVED: native_builtin_class_import_path
- REMOVED: PythonCameraSourceBlock
- REMOVED: sdk/streamlib-python-wheel/src/python_runtime_lifecycle.rs
- REMOVED: sdk/streamlib-python-wheel/src/python_control_plane_hosting.rs
- REMOVED: sdk/streamlib-python-wheel/src/python_added_processor.rs
- REMOVED: sdk/streamlib-python-wheel/src/python_test_harness_endpoints.rs
- REMOVED: sdk/streamlib-python-wheel/src/python_helper_process_spawn_host.rs
- REMOVED: emit_app_process_python_log_record
- REMOVED: launch_app_node
- REMOVED: start_app_under_test

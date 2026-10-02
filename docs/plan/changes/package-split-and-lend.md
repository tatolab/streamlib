# package-split-and-lend

Step 3 of the one-runtime-per-machine pivot: the Python library's rename and its split. After it:
- `tatolab-stream` (`tatolab.stream`) is pure Python; a stream's venv installs it, every stream and
  node module imports from it, and a stream imports, type-checks and compiles with no engine there;
- `tatolab.runtime`, the engine's native part, is built into a lend directory, never pip-installed;
- the runtime starts each processor interpreter from the stream's venv interpreter with the lend
  directory prepended to `PYTHONPATH`, its own bootstrap run by path, the build-id check kept;
- the runtime process imports no project module: it learns a Python node's ports by asking the
  stream's interpreter, and compiling happens in the project's interpreter;
- the runtime refuses by name what it does not understand in a graph — a node type it lacks, a
  setting it does not know, a key it does not read — and a graph recorded today loads in every
  later runtime;
- pip publishes `tatolab-stream`, `tatolab-moq`, `tatolab-webrtc`, and no engine.

Unchanged, mapped below: one engine per `run` process, the installer and the native `tatolabd`,
the Rust crate and directory names, the `STREAMLIB_*` engine environment variables.

**Scale gate — this skill, plus the existing ADR.** The Python API's public contract moves (every
import path; `Runtime.load` gains the stream's environment; built-ins stop being native types),
the processor model moves (how an interpreter starts, where a node's descriptor comes from), and
distribution moves. The rationale is `docs/decisions/package-split-and-lend.md` (#2583); decision
1 below amends its Consequences when ruled.

**Precondition.** Every entry built is DECIDED: §Packages `ARCHITECTURE.md:516-534` (the split and
the lend), `:538-550` (the names), `:159-170` (the handshake clause re-read), `:281-294` (an
extension depends on `tatolab-stream`); §Product `:107-113` (pip never carries the runtime);
§Processor model `:1065-1080` (the exec, amended), `:1260-1270` (the build-id check),
`:1361-1365` (the environment beside the graph); §Distribution `:4254-4262`. Not built against:
needs `:1366-1373`, packs `:551-566`, the control client `:535-537`; the hosting entries
`:114-129` are DECIDED and step 4's. **Sequencing:** builds after #2569 (stream-graph's contract)
and #2575 (the local API's port gone), renaming the surface they leave rather than racing them.

**Verified against the tree 2026-10-02 (HEAD c3d123d37).** Three read-only sweeps: the wheel's
native boundary, interpreter spawn and the handshake, packaging and CI.

**Where the line falls today**
- Importing any `streamlib` module loads `_engine` (`__init__.py:28-78`). Pure on their own:
  `audio_block`, `encoded_audio_packet`, `_processor_config_schema`, `_capability_extensions`,
  `_control_plane_client`, `_cross_floor_check`, `_node_registry`, `_processor_hosting`,
  `_bundled_vulkan_driver`. Native at import: `_processor_declaration.py:27`, `clock.py:37-39`,
  `log.py:18`, `glsl_pixel_effect.py:32`, `claimed_surface_pixel_access.py:30-36`,
  `processor_output_texture_ring.py:28`, `model_input_tensor_kernel.py:29`,
  `_sampled_source_landing.py:15`, `_surface_image_exchange.py:35`, `testing.py:26-27`; and
  `video_frame.py`, `encoded_video_frame.py` through `claimed_surface_pixel_access`.
- `@processor` stamps in pure Python (`_processor_declaration.py:399-432`), then calls the native
  `register_declared_processor_class` (`:431`; `src/python_processor_registration.rs:132-157`),
  so no node module imports without the engine.
- The thirteen built-ins are frozen marker pyclasses with no constructor
  (`src/python_native_builtin_blocks.rs:18-96`), recognised by type identity
  (`native_builtin_class_import_path`, `:105+`; `classify_processor_class`,
  `python_runtime_lifecycle.rs:44-73`); their `type` is the Rust `module_path!()`
  (`sdk/streamlib-macros/src/codegen.rs:412-419`); their config is an untyped dict described in
  docstring prose (`_engine.pyi:97-494`). The Rust `Config`s derive `JsonSchema` but not
  `deny_unknown_fields` (`camera_source.rs:34-37`), so a misspelled setting is dropped silently
  at `processor_instance_factory.rs:290-295`.
- Everything a node is handed is native — contexts, link readers and writers, `GpuContext*`,
  `GpuSurfaceHandle`, kernels — as are the bag codec pair (`src/python_bag_conversion.rs:62-81`)
  and `monotonic_now_ns`. `AudioBlock`, `EncodedAudioPacket`, `ColorInfo`, `ContentLight`,
  `MasteringDisplay` are pure dataclasses. `_engine.pyi` is 2419 lines, stubtest-gated bare at
  `python-wheel.yml:161` and `test.yml:873`.

**Spawn and the handshake**
- One process-global interpreter: `capture_helper_process_launch_environment` reads
  `sys.executable` and `sys.path[0]` under the GIL into a `OnceLock`
  (`src/python_helper_process_spawn_host.rs:104-127`, from `Runtime()` at
  `python_runtime_lifecycle.rs:324`); the command is `<it> -m streamlib._helper` (`:44`,
  `:226-274`); `PYTHONPATH` is the app directory then the parent's own (`:278-289`);
  `PYTHONHOME` removed; working directory inherited; no override of any kind.
- The parent imports project code three ways: `run` executes the entry file in-process
  (`cli.py:165-183`); `rt.add` reads the stamp off the class object
  (`python_processor_declaration.rs:34-171`); the MCP `add_processor` resolver imports a module
  in the runtime process (`python_runtime_lifecycle.rs:278-298` →
  `python_processor_registration.rs:183-210`).
- The build id is composed in `engine_build_id_composition.rs`, compiled in at
  `subprocess_bridge.rs:74`, checked first in `_helper.py:1276-1296`; tests
  `test_helper_process.py:2015`, `:2043`, `:2063`, `test_helper_placement.py:414`. No
  interpreter-shape check exists; the floor is packaging only (`requires-python>=3.10`,
  `abi3-py310`).
- The engine, SDK, api-server and built-ins carry no pyo3 and build without Python; spawn, the
  descriptor reader and the resolver live only in the wheel crate.
- macOS: `dladdr` finds `_vulkan_driver/` beside the `_engine` image
  (`runtime/streamlib-consumer-rhi/src/vulkan_loader_library.rs:66-95`); `__init__.py:18-21`
  names the ICD before `_engine` loads.

**Packaging and CI**
- maturin: `module-name = "streamlib._engine"`, `python-source = "python"`, console script
  `streamlib = "streamlib.cli:main"`. CI `maturin develop`s into one venv and runs pytest there
  (`python-wheel.yml:103-110`, `test.yml:862-867`); `macos-wheel.yml:85-151` stages the driver,
  builds, installs and runs the scaffold.
- `scripts/build_simple_index.py:31` publishes `streamlib`, `streamlib-moq`, `streamlib-webrtc`.
  Both extensions depend on `streamlib>=0.20.0`, register under `streamlib.extensions`, and link
  no streamlib crate.
- xtask reads wheel paths: `lint_logging.rs:50-56`, `check_clock_usage.rs:107`,
  `check_boundaries.rs:521`, `:988`, `:2223-2268`, `check_no_in_process_placement.rs:52`,
  `:1185`, `generate_third_party_notices.rs:852`, `:1204`, `:1277-1312`; so do the rig-brake
  and licence-header script tests.

---

## [NEEDS DECISION] 1 — what the runtime process runs on until `tatolabd` exists

This change is done when a stream runs from a venv holding no engine; step 4 builds the native
`tatolabd`. In between, `run` still starts one runtime process per stream (stream-graph leaves it
so), and today that process is the stream venv's own interpreter with the engine imported into it
(`cli.py:216-293`). Once the venv holds no engine, something else hosts it.

- **(a) The local build carries an environment of its own.** `cargo xtask build-runtime` writes
  `target/tatolab-runtime/`: `lend/` (the lend directory), `environment/` (an empty uv venv), and
  `bin/tatolab`, a launcher starting the runtime process from that venv with `lend/` on its path.
  A stream's venv is touched only by the compile and its processor interpreters. Consequence: an
  extension installed in a stream's venv runs its hook in that stream's processor interpreters
  only; the runtime process sees what sits beside the runtime — nothing yet — so `graph`'s
  `extensions` key lists runtime-side registrations, the shape the packs direction (`:562-566`)
  already describes, and the hook test's app-process arm installs its distribution beside the
  runtime. Step 4 replaces `bin/` and `environment/` with `tatolabd`; compile, describe, spawn
  and `lend/` carry over.
- **(b) The stream's venv interpreter with the lend prepended** — the exec a processor
  interpreter gets. Least new machinery; venv hooks keep running in the runtime process. Built to
  be deleted: one runtime process cannot run from many venvs, so step 4 replaces it.
- **(c) Pull `tatolabd` forward.** After this change the runtime process needs Python only for
  the CLI, the `Runtime` class and the runtime-side hook; spawn and describe already leave it.
  Merges two steps #2582 separated on purpose.

**Recommendation: (a)** — the only option whose runtime process already has step 4's shape,
independent of every stream's environment; its throwaway is a launcher script and an empty venv.

---

## ADDED: §Packages — `tatolab.stream`, the stream package

- **Distribution.** `tatolab-stream` in `sdk/tatolab-stream/`, a pure build backend, shipping
  `tatolab/stream/` and never `tatolab/__init__.py`; `requires-python >=3.10`; its dependencies
  are what its own modules import, never the runtime. A module moves here iff a stream or node
  module imports it: the declarations (`@node`, `@input`, `@output`, `@stream`, `Stream`, the
  references, `compile_stream_to_graph`, as #2564 and #2567 leave them) and
  `_processor_config_schema`; the data types `AudioBlock`, `EncodedAudioPacket`, `VideoFrame`
  with its colour types, `EncodedVideoFrame`; the composable pieces `ClaimedSurfacePixelAccess`,
  `PixelAccessToOneClaimedSurface`, `GlslPixelEffect`, the `ModelInputTensor` family,
  `ProcessorOutputTextureRing`; `clock` and `log`.
- **Built-ins are pure classes**, each carrying the `type` the runtime registers (its
  processor's `processor_class_import_path()`) and a `TypedDict` mirroring its Rust `Config`:
  ```python
  class CameraSourceConfig(TypedDict, total=False):
      device_id: str
      ...
  class CameraSource:
      """Captures frames from a camera device."""
      type: ClassVar[str] = "<CameraSource's processor_class_import_path()>"
  stream.add(CameraSource, config={"device_id": "/dev/video2"})  # pyright checks the keys
  ```
  `Stream.add` records `type` and the config; whether this floor has it is the runtime's answer
  at load, below. A runtime-side test holds every class's `type` to the registered processor and
  its `TypedDict` keys to the Rust `Config`'s `JsonSchema`.
- **Runtime-backed names are declared here and bound in the interpreter**: the contexts,
  `LinkInputDataReader`, `LinkOutputDataWriter`, `ProcessorLinkDataAccess`, `GpuContext*`,
  `GpuSurfaceHandle`, the kernels, `MonotonicTimer`, the texture exports,
  `ProcessorOwnedWindow*`, `CapabilityExtensionHost`, `encode_bag_to_msgpack_bytes`,
  `decode_msgpack_bytes_to_python_object`, `monotonic_now_ns`,
  `this_machines_stamp_clock_identity`. A module imports them from `tatolab.stream` with no
  runtime present — a class as a `typing.Protocol` carrying today's stub signatures, a function
  resolving the runtime's on first call. With no runtime lent, a call raises `RuntimeError`
  naming it and saying it runs in a processor interpreter; in one, a node is handed the
  runtime's own objects. Pure pieces wrapping native ones (`glsl_pixel_effect`,
  `claimed_surface_pixel_access`) bind them at call time, never at import.
- **`@node` registers nothing**: the stamp and no more — `_processor_declaration.py:431` and
  `register_declared_processor_class` go; the stamp reaches the runtime through describe.
- **The proof**: in a venv holding only `tatolab-stream`, the scaffold's `stream.py` and nodes
  import, pass pyright and `compile_stream_to_graph`, with no runtime on any path.

## ADDED: §Packages — `tatolab.runtime`, the runtime portion, and the lend directory

- **`tatolab/runtime/`, a regular package**: `__init__.py` naming the bundled ICD before
  `_engine` loads (today's `streamlib/__init__.py:18-21`); `_engine`
  (`module-name = "tatolab.runtime._engine"`) and `_engine.pyi`; the `Runtime` class (today's
  `__init__.py:191-222`); the bootstrap (`_helper.py` → `_processor_interpreter_bootstrap.py`);
  the CLI, `testing`, `_capability_extensions`, `_control_plane_client`, `_runtime_log_reader`,
  `_node_registry`, `_processor_hosting`, `_cross_floor_check`, `_scaffold_template/`; on macOS
  `_vulkan_driver/`.
- **The lend directory.** maturin builds the portion from `sdk/streamlib-python-wheel/` as a
  wheel that `cargo xtask build-runtime` unpacks into `target/tatolab-runtime/lend/` — never
  installed, never published. A gate fails any `tatolab/__init__.py` in the tree or in a built
  artifact. `dladdr` finds `_vulkan_driver/` beside `_engine` unchanged; the ad hoc re-signing of
  a rewritten Mach-O moves with the staging script. Developers and CI run the runtime from this
  build, as the ADR's Consequences state.

## ADDED: §Processor model — the stream's environment and the lend at spawn

- **The environment** is recorded per loaded stream, beside its graph:
  ```python
  runtime.load(graph, environment=StreamEnvironment(project_directory=project, interpreter=project / ".venv/bin/python"))
  ```
  The `OnceLock` capture (`python_helper_process_spawn_host.rs:104-127`) is deleted;
  `spawn_host_for_processor_node` (`:1306-1333`) reads the stream's environment.
- **The command** (`build_helper_process_command`, `:226-274`): `environment.interpreter
  <lend>/tatolab/runtime/_processor_interpreter_bootstrap.py`; `PYTHONPATH` = the lend directory,
  then the project directory, the runtime process's own not passed on; `PYTHONHOME` removed as
  today; working directory the project directory; the `STREAMLIB_*` variables unchanged. The
  bootstrap runs by path, never `-m` or `-c`, and first drops its own directory from `sys.path`,
  so neither the working directory nor the script's shadows a module.
- **The bootstrap's order**: (1) import `tatolab.runtime`; an interpreter that cannot load it —
  not CPython, below 3.10, free-threaded, a foreign architecture — writes a refusal to raw stderr
  naming the interpreter's path, implementation, version, free-threading, architecture and the
  import error, and exits, as the build-id refusal does; (2) the build-id check exactly as
  `_helper.py:1276-1296`; (3) today's sequence (`:1299-1373`).
- **Describe.** The runtime learns a Python node type's descriptor by running the stream's
  interpreter, lent the same way, on the bootstrap's describe entry — at load for every Python
  `type` a graph names, at a live `add_processor` for one not yet described. The output is
  today's `PythonProcessorDeclaration` as JSON, read by the same Rust; a type that will not
  import or carries no stamp is refused by name, quoting the interpreter's stderr.
  `register_processor_class_by_import_path` and `install_unregistered_processor_type_resolver_once`
  are deleted: the runtime process imports no project module.

## ADDED: §Product — the `tatolab` CLI from the local build (decision 1)

- `target/tatolab-runtime/bin/tatolab` carries today's verbs as stream-graph and local-api leave
  them; the `streamlib` console script is gone.
- **`run` and `dev`**: find the project directory and `<project>/.venv/bin/python`, refusing by
  name and pointing at `uv sync` when it is absent; run `tatolab.stream`'s compile entry in that
  interpreter with the project as import root, which writes the graph as JSON on stdout (on
  failure its own traceback is the error); then `Runtime(...)`, `load(graph, environment=...)`,
  host the local API, `run()`. `dev` restarts the same on an edit. The cross-floor check reads
  source as text, as today.
- **`new`** writes `dependencies = ["tatolab-stream", "numpy>=2.1"]` against the same index.

## ADDED: §Processor model — the runtime refuses by name what it does not understand

- Every built-in `Config` takes `#[serde(deny_unknown_fields)]`; the factory's error
  (`processor_instance_factory.rs:290-295`) names the node, its `type` and the unknown setting.
- A `type` the runtime has not registered is refused at load naming it; a built-in absent on
  this floor is refused there too, naming the floor — `VirtualCameraSink`'s refusal moves from
  `rt.add()` to load.
- A graph key the loader neither reads as spec nor knows as one of `graph`'s live keys is
  refused by name.
- A golden graph checked in here, holding every key and every built-in `type`, is loaded by a
  test on every later build; a change of shape adds a new golden beside it, never edits it.

## MODIFIED: stream-graph (in flight) — `load` takes a graph and an environment

- `Runtime.load(stream_or_graph)` becomes `Runtime.load(graph, *, environment)`: a `@stream`
  function compiles in the project's interpreter, never the runtime process (`:533-534`). #2567
  builds the function form; this change removes it.
- The loader "reads the spec keys and ignores the rest" becomes: reads the spec keys, skips the
  live keys `graph` renders, refuses any other.

## MODIFIED: records re-spelled at the fold

- §Packages `:184-191` depends on `tatolab-stream`; `:226-237` — `tatolab.stream` exports the two
  codec names, bound to the runtime's in a processor interpreter; `:239-275` — the group is
  `tatolab.extensions`, `host` is `tatolab.stream.CapabilityExtensionHost`, the runtime-process
  call site reads the runtime's own environment (decision 1); `:281-294` — `streamlib-moq` →
  `tatolab-moq` (`tatolab.moq`, native `tatolab.moq._native`) and `streamlib-webrtc` →
  `tatolab-webrtc`, depending on `tatolab-stream`, their lanes running with a lend; their
  directories keep their names, which CLAUDE.md's licensing rule cites.
- §Product `:60-82` — the closed list refuses at load; `:83-98` — the scaffold depends on
  `tatolab-stream`. §Processor model `:1065-1080` — the exec is the environment's interpreter;
  `:1260-1270` — the child imports the lent portion, the interpreter refusal first.
- §Distribution `:4147-4155` — artifacts are `tatolab-stream` (one `py3-none-any` wheel) and the
  two extensions on the index, named at `build_simple_index.py:31`; no engine is published until
  step 4's installer. `:4156-4217` — the portability, signing and bundled-driver proofs run over
  `lend/`; the macOS workflow builds the runtime, makes a venv holding only `tatolab-stream`, and
  runs `tatolab new` then `tatolab run --test-pattern` for twenty seconds — open the lent loader,
  raise nothing, sixty frames. That is the done-proof on the Apple floor; Linux runs the same.
- `README.md:86-94` and the `docs/architecture/` pages naming `streamlib` imports are corrected
  in the shipping tickets. CLAUDE.md's "Reading the Python surface" path, `placement.md`'s
  "exec of `sys.executable`" and the skills that spawn `streamlib` are corrected in their own
  operating-model PR, as `flow.md` requires.

## Left to later changes, so nothing is lost

| Not here | Because | Lands with |
|---|---|---|
| Native `tatolabd`, the installer, many streams per runtime, the state directory | step 4 | runtime hosting |
| `sdk/streamlib-python-wheel`, the `streamlib` crates, `STREAMLIB_*`, engine identifiers | the Rust names are step 10's | the app |
| Publishing `tatolab-stream` to PyPI | outward-facing; names free 2026-10-01, unregistered | the owner's call |
| Needs from nodes; packs; the control client | OPEN | steps 6, 7, 8 |
| The thirteen examples (`import streamlib`, the engine wheel) | converted consumers | backlog filed at ship |
| Testing a stream without a runtime | separate work (`:127-129`) | its own change |

## Assumptions stated, not asked

- Runtime-backed classes are declared twice — `Protocol`s in `tatolab.stream`, the stub in
  `_engine.pyi` — held equal member for member by a runtime-side test while stubtest holds the
  stub to the binary. Bites as two declarations instead of one file: drift fails CI but is not
  impossible by construction.
- Built-in `TypedDict`s are held to the Rust `JsonSchema` by a test, not generated from it.
- The project interpreter is `<project>/.venv/bin/python`, uv's default; a relocated venv is
  refused by name until a real need appears.
- Describe costs one interpreter start per load and per live add of an undescribed type.
- A processor interpreter's working directory becomes the project directory.
- The two extensions move inside this change, the canary §Consumers reserves, as stream-graph
  moved them to `@node`.
- The wheel's suite runs in a dev venv with `tatolab-stream` editable and `lend/` on
  `PYTHONPATH`, the test process playing the runtime process; the no-runtime proof runs in a
  second venv holding `tatolab-stream` only.

## Expected slices

- **S1 — the namespace.** `streamlib` → `tatolab.stream` + `tatolab.runtime`, still one maturin
  project, no `tatolab/__init__.py`: every import in the engine tree and its tests, the stub's
  `module-name`, stubtest lines, xtask paths, notices, script tests, the entry-point group.
  Blocked by #2569, #2575.
- **S2 — the stream package needs no runtime.** Pure built-ins, runtime-backed declarations,
  `@node` registering nothing, describe, the resolver deleted, the no-runtime proof. Blocked by S1.
- **S3 — two distributions and the lend.** `sdk/tatolab-stream/`, `xtask build-runtime`, the
  environment beside the graph, spawn with the lend, the bootstrap by path and its refusal, CI
  over `lend/`. Blocked by S2.
- **S4 — `tatolab` from the local build** (decision 1). The launcher, compile in the project's
  interpreter, `new`, the done-proof on both floors, the release publishing `tatolab-stream`
  alone. Blocked by S3.
- **S5 — refusal by name.** `deny_unknown_fields`, unknown types and floors, unknown keys, the
  golden graph. Blocked by #2565 only.
- **S6 — the extensions.** `tatolab-moq`, `tatolab-webrtc`, `tatolab.extensions`. Blocked by S3.

## REMOVED

- REMOVED: sdk/streamlib-python-wheel/python/streamlib
- REMOVED: streamlib._engine
- REMOVED: streamlib._helper
- REMOVED: streamlib.cli:main
- REMOVED: from streamlib import
- REMOVED: pip install streamlib
- REMOVED: streamlib.extensions
- REMOVED: register_declared_processor_class
- REMOVED: register_processor_class_by_import_path
- REMOVED: install_unregistered_processor_type_resolver_once
- REMOVED: capture_helper_process_launch_environment
- REMOVED: native_builtin_class_import_path
- REMOVED: PythonCameraSourceBlock

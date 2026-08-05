# StreamLib Architecture Plan

The single source of architectural decisions. Sessions implement this plan; they do not make
architecture. A decision missing here stops work and comes back to the owner — it is never
inferred from existing code, consumers, or history. This document and the diagrams under
`diagrams/` (Mermaid `.mmd`, the committed source — Excalidraw files are generated views,
never round-tripped back) move together: every DECIDED entry is represented in the diagram.

Legend: **DECIDED** — build exactly this. **OPEN** — do not build; needs an owner decision.

## Product (the MVP sentence) — IN-FLIGHT (→ importable-python-library)

- **DECIDED** — A Python developer on Linux with an NVIDIA GPU pip-installs streamlib
  (initially from this repo's releases; PyPI after the project rename) into an
  ordinary uv-managed venv, runs `streamlib new` then `streamlib dev`,
  sees their camera live in a window within a minute, and makes the pipeline theirs by
  editing the scaffolded processor — zero ceremony: no manifest, no `main()`, no schema
  wrangling, a fast edit loop. Every ticket traces to this sentence or does not
  exist. [importable-python-library]
- **DECIDED** — Terms of the sentence: StreamLib is an importable Python library — one
  PyPI wheel carrying the Python API, the CLI, and the Rust engine (PyO3, the
  pydantic-core model); a StreamLib app is a normal Python codebase — one venv, one
  Python version, ordinary PyPI dependencies, nothing dynamically downloaded;
  `dev`/`run` find `app.py`'s `setup(rt)` by convention, `-f <file>` overrides;
  processors are Python classes written in the app or imported from pip-installed
  packages, and `rt.add` takes the class; the pipeline API is `add`/`connect`.
  [importable-python-library]
- **DECIDED** — The zero-ceremony bar (the sentence is untrue until all hold): no
  manifest authoring; no boilerplate entry; bags/schemas fixed (no engine schema
  matching, cast-at-read, no versions at the code layer); scaffolding for app and
  processor; the scaffold pins `.python-version` (3.12) and the wheel supports a small
  Python version range. [importable-python-library]
- **DECIDED** — Rust authoring stays a supported capability: a Rust app is a plain
  cargo project depending on the `streamlib` crate — no wrapper generation, no special
  format; third-party Rust processors for Rust apps are ordinary cargo dependencies,
  source-compiled. [importable-python-library]

## Packages & extension model — IN-FLIGHT (→ importable-python-library)

- **DECIDED** — PyPI and cargo are the package systems. The custom module system is
  deleted in full: `streamlib_modules/`, the `.slpkg` format, `streamlib.lock`, the
  package source, the `add`/`install`/`link`/`pkg` verbs, `BuildOrchestrator` and all
  runtime downloading or compiling. Compilation happens at publish time, by the
  author, with standard tools (maturin/CI for wheels, cargo for crates) — StreamLib
  never compiles user code. [importable-python-library]
- **DECIDED** — The plugin ABI is deleted: no dlopen'd processor cdylibs, no `repr(C)`
  vtable surface, no load handshake, no build fingerprints. The extension paths are
  Python packages and Rust source crates only. [importable-python-library]
- **DECIDED** — Third-party native code (closed-source included) ships as an ordinary
  Python package whose native internals expose capabilities to Python as handles —
  frames, FDs, exportable device allocations, buffers — wrapped by a Python
  processor. It never links the engine and never speaks streamlib internals; the
  CPython ABI is the only
  binary boundary, and no process ever holds two streamlib engines — the app process
  runs the one engine, and a helper process imports the same wheel as a processor
  host, never as a second engine. Handles it exposes must be genuinely transferable
  across a process boundary (an fd, an exportable allocation) — an
  address-space-local pointer is not a handle. [importable-python-library]
- **DECIDED** — The engine's handle-shaped primitive surface is the public contract
  for native interop: DMA-BUF / OPAQUE_FD import and export, the present target,
  texture rings, codec byte pumps, the audio clock, color resolution — surfaced to
  the Python ecosystem as DLPack and the CUDA Array Interface (DLPack first). The
  contract is zero-CPU-copy, stated honestly: tiled engine textures reach a linear
  tensor via one GPU blit into an exportable staging buffer, because DLPack expresses
  strided linear memory only. The Vulkan↔CUDA and Vulkan↔GL interop adapters survive
  as in-process capabilities (torch/cupy and GL consumers); only their cross-DSO
  `-abi` halves die with the plugin ABI. [importable-python-library]

## Processor model & scheduling — IN-FLIGHT (→ schema-free-ports, processor-class-identity, importable-python-library, in-process-hosting-ripout)

- **DECIDED** — A link is pure plumbing: output port → input port, carrying a bag
  (self-describing msgpack named map). The engine has no type layer: ports carry no
  type declaration, connect never inspects or compares types and never warns, no read
  path examines a tag, and the frame header carries no schema ident. Consuming is a
  cast at read time; a mismatch surfaces as a decode failure at the consuming
  processor. [schema-free-ports]
- **DECIDED** — A port declares three things and nothing else: name, description, and
  — on an input — delivery profile. Type information belongs to the authoring language
  and never reaches the engine: in Python the port method's return annotation is the
  declaration, read by humans and type checkers only, with `ctx.inputs.read(port)`
  yielding the bag as a mapping and `read(port, into=T)` the opt-in strictness dial
  (a TypedDict casts for free, a dataclass or pydantic model constructs and validates,
  raising at read); in Rust the read target's `Deserialize` impl is the validation,
  always on, with no free-cast mode. [schema-free-ports]
- **DECIDED** — Channel policy (delivery profile, ring depth, overflow) is declared
  port-locally at the consuming input port. Every input port declares its delivery
  profile explicitly — there is no default and nothing left to infer one from, so an
  input port without one is a wiring error. [schema-free-ports]
- **DECIDED** — There is no schema layer: no JTD, no schema registry, no embedded
  schemas, no codegen and no generated type classes, and no schema identity grammar
  anywhere in the engine or the authoring surfaces. [schema-free-ports]
- **DECIDED** — Port rendering in the control plane is name, description, delivery
  profile, and direction; no port carries a type in `graph`, `tap`, or any snapshot.
  [schema-free-ports]
- **DECIDED** — Three execution modes (reactive / manual / continuous); one dedicated
  OS thread per processor with descriptor-driven priority (realtime / high / normal);
  synchronous lifecycle traits; Full/Limited capability typestate on the phase axis
  (setup/teardown vs process). [execution-model]
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
  placement. [helper-process-placement-only]
- **DECIDED** — The MVP edit loop is re-running `dev` (warm restart is sub-second by
  construction). Reload-on-save is a nicety, not MVP-gating, and when built it is
  processor-granular — stop the processor, respawn its helper (a fresh interpreter
  re-imports the class), rewire its ports — never module-loading machinery.
  [importable-python-library]
- **DECIDED** — A processor's identity is its class, named by its fully-qualified
  import path (`my_app.filters:BlurProcessor` in Python, the type path in Rust) —
  derived mechanically, never authored, and the same string in the registry, in the
  control plane's type field, and for spawning the processor's helper process — which
  is how every Python processor runs. A processor defined in the entry file run as
  `python app.py` identifies as `__main__:<Type>` and is a wiring error at `rt.add`,
  with an error naming the fix (move the class to an importable module and import it
  from the entry file — one import line). The entry file itself may still run as
  `__main__`; only processor classes may not live there.
  The `@org/package/Type` identity grammar is deleted along with
  the `@app/local` synthesis; `@processor` declares execution, interval, scheduling
  priority, and description only. [schema-free-ports; `__main__` clause reversed by
  helper-process-placement-only]
- **DECIDED** — An instance's display name is the human-facing label — passed at `add`,
  readable off the returned handle, and the prefix on its log records; it defaults to
  the class's short name and the engine disambiguates duplicates within one graph.
  Identity is never derived from it. [schema-free-ports]
- **OPEN** — Additional execution flavors to scale processor count (lightweight /
  green-thread style): intended, do not build until designed; hard constraint — no new
  configuration dials. [execution-model]

## Graphics (RHI / GPU)

- **DECIDED** — All Vulkan lives in the RHI (`vulkan/rhi/` + `streamlib-consumer-rhi`); one
  kernel abstraction per pipeline kind; consumers go through `GpuContext` only.
- **DECIDED** — The engine's kernel primitives are exposable to Python as configured
  blocks: shader/compute source and binding config passed from Python, compiled and
  executed by the engine on its device — no user-side Vulkan, ever. Shape only; the
  Python-facing API design is its own session. [importable-python-library]
- **OPEN** — Everything else.

## Media I/O — camera, display, audio — IN-FLIGHT (→ importable-python-library, one-monotonic-clock)

- **DECIDED** — First-party camera, display, and audio are native built-in processors
  in the engine tree, statically linked into the wheel — pre-built named blocks
  instantiated and configured from Python (`rt.add(CameraSource)`), whose per-frame
  paths never enter the interpreter. Lag-by-design ends: built-ins ship inside the
  wheel, current by construction. [importable-python-library]
- **DECIDED** — Built-ins are written against the same handle-shaped hardware
  primitives third parties get — DMA-BUF / OPAQUE_FD import-export, present target,
  audio clock, color resolution, codec sessions — never against private engine guts;
  the layering wall survives the ABI's deletion as internal discipline.
  [importable-python-library]
- **DECIDED** — V4L2 is the only capture backend (platform floor: Linux + NVIDIA).
  Apple capture (AVFoundation) is post-MVP and undesigned; only the TCC permission
  shims exist. [media-io-layering]
- **DECIDED** — Windowing: the built-in display owns window creation and the event
  pump; the raw-window-handle seam remains the internal boundary — the engine mints
  the present target from the raw handle and owns every swapchain and acquire detail,
  plus the platform main-thread event loop where the OS demands it (in the importable
  arrangement the process main thread belongs to the user's script; `rt.run()` blocks
  with the GIL released while the engine pumps). [importable-python-library]
- **DECIDED** — Camera → GPU transport: zero-copy DMA-BUF import when the device
  exports it, transparent CPU-upload fallback otherwise, selected automatically —
  no configuration dial. [media-io-layering]
- **DECIDED** — Python-authored media processors (vendor or user) run in their own
  helper process like every other Python processor and are supported where deadlines
  allow: camera-class sources and block-level audio fit within the helper hop's
  budget; vsync-paced present loops and device audio callbacks stay native, always —
  a deadline the cross-process hop cannot meet, not a GIL argument.
  [importable-python-library, helper-process-placement-only]
- **DECIDED** — One clock on the data plane: every timestamp a processor stamps, reads,
  or compares — frames, bags, audio ticks, `ctx.time` — is the machine's monotonic clock
  (`CLOCK_MONOTONIC` on Linux, `mach_absolute_time` on Apple), the same epoch the V4L2
  and ALSA driver stamps carry, comparable across every node on a host. No
  process-relative epoch anywhere, and each language exports exactly one name for it.
  Wall clock is permitted on exactly four observability surfaces and nowhere else: log
  record `host_ts` and `source_ts`, log file naming, and the control-plane pubsub event
  timestamp — their job is correlating with the outside world, which monotonic time
  cannot do. A wall-clock value never enters the data plane and is never compared against
  a media timestamp; a fifth surface is a plan change, not a judgement call.
  [one-monotonic-clock]
- **OPEN** — Audio backend: PipeWire-native on Linux is the intent (the current
  CPAL → ALSA path is interim); do not build until a research memo settles it. A/V
  sync model likewise OPEN. The engine's decided audio surface is the clock
  primitive. Hard constraint for the memo: the backend must be dlopen-at-runtime or
  live outside the wheel — a linked libasound/libpipewire is not
  manylinux-portable. [media-io-layering]

## Networking — transport, moq, webrtc

- **DECIDED** — Cross-language interop happens on the wire between nodes, as
  self-describing bags — never in-graph. [importable-python-library]
- **OPEN** — Everything else (transport choice, moq, webrtc, mesh discovery).

## Language SDKs & parity — IN-FLIGHT (→ importable-python-library)

- **DECIDED** — Python is the sole focus runtime: the importable PyO3 wheel is the
  primary authoring surface. The Deno SDK and the subprocess-polyglot machinery are
  deleted with the module system they are built on. TypeScript authoring is paused,
  not rejected — a future TypeScript SDK follows this same importable-library model
  (a native module a TypeScript app imports; Deno itself optional), aimed at the
  hobbyist / video-creator audience when it is scheduled. [importable-python-library]
- **DECIDED** — The Python SDK carries a GIL-release contract: every native binding
  that can block releases the GIL around the blocking call, and pixels never cross
  into Python as Python-owned objects — frames travel as handles / surface ids, and
  pixel memory is reached only through explicitly exported views (DLPack, the CUDA
  Array Interface, a mapped CPU buffer). The contract exists so a
  blocking native binding never stalls the threads of its own interpreter — the app's
  for the app-side bindings, the helper child's for a processor's. It is never a
  co-tenancy remedy: no two Python processors share an interpreter.
  [importable-python-library, helper-process-placement-only]
- **DECIDED** — The wheel carries an interpreter-lifecycle contract: `rt.run()` owns
  SIGINT while it blocks (Ctrl-C returns cleanly and restores CPython's handler), and
  engine teardown strictly precedes interpreter finalization — all engine threads
  joined and every anchored thread state released before `rt.run()` returns, with an
  `atexit`/context-manager guarantee on the exception path. Proven against a real
  `python app.py` harness, the arrangement the spike never ran.
  [importable-python-library]

## Distribution & versioning — IN-FLIGHT (→ importable-python-library)

- **DECIDED** — Two artifacts, one version, released together: the streamlib wheel
  (Python API + CLI + engine) and the `streamlib` crate for Rust apps. Initial
  release channel is this repo's releases served through a static PEP 503 simple
  index (`pip install streamlib --index-url …` — one stable incantation) — PyPI
  publication waits for the project rename; the artifact is identical either way.
  Positioning is "realtime engine, Python authoring" — the Rust engine is named as
  material; never marketed as "a Python library" even though the shape is one.
  [importable-python-library]
- **DECIDED** — Wheel portability model: system libraries (Vulkan loader, window
  system, libcuda) are dlopen'd at runtime, never linked — the wgpu/opencv-python
  manylinux shape; "baked in" means our Rust is compiled in, not that system deps
  are static. abi3 across a small range of GIL-enabled CPython builds only
  (free-threaded builds wait for the stable ABI to exist for them). The wheel's
  adapter closure excludes skia. Helper processes import the wheel itself — one
  native artifact, no separate helper cdylib. [importable-python-library]

## Control plane & observability — IN-FLIGHT (→ importable-python-library)

- **DECIDED** — One control plane: the api-server's HTTP + WebSocket + MCP surface,
  hosted in-process by any runtime that enables it. The MCP tool set is the canonical
  control vocabulary; the CLI is a pure JSON-RPC client of it — agents and humans use
  the same verbs; REST/WS routes serve the same operations for programmatic clients.
  Post-pivot the vocabulary is observation-shaped: graph, tap, logs, health, nodes.
  The live-mutation verbs (submit / replace / connect / remove) and their MCP tools
  are removed — code is the source of truth; the edit loop is `dev`, not live
  mutation. `dev` binds loopback by default. [importable-python-library]
- **DECIDED** — The api-server is engine-side infrastructure and relocates into the
  `runtime/` tree: it is a host — statically linked, never dlopen'd. Its new host is
  the wheel (and the `streamlib` crate for Rust apps); the relocation is a sequencing
  prerequisite of the rip-out. [control-plane-one-surface]
- **DECIDED** — The CLI ships inside the wheel and slims to `new` / `dev` / `run` (a
  thin runner over the same engine the wheel exposes) plus the observation verbs
  (`nodes` / `graph` / `tap` / `logs` / `mcp`); build-orchestration, packaging,
  provisioning, and codegen verbs are deleted. The standalone streamlib-runtime
  binary retires. Python embeds the engine in-process via the wheel; the control
  plane exists to observe and drive *running* nodes, not to embed.
  [importable-python-library]
- **DECIDED** — Node discovery is a per-user on-disk registry — one JSON file per live
  node in the OS's standard per-user runtime directory — written only by
  control-plane-hosting runtimes, pruned only when both liveness signals (control
  round-trip, process check) fail. [control-plane-one-surface]
- **DECIDED** — Observability: the JSONL log schema is a durable contract; tap forwards
  bags verbatim, trading completeness for guaranteed non-interference; graph and health
  inspection ride the same control plane. [control-plane-one-surface]
- **OPEN** — Auth and remote-access posture.

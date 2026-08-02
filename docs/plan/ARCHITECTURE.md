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
  wrangling, hot-reload on save. Every ticket traces to this sentence or does not
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
  frames, FDs, device pointers, buffers — wrapped by a Python processor. It never
  links the engine and never speaks streamlib internals; the CPython ABI is the only
  binary boundary, and exactly one streamlib engine lives in a process.
  [importable-python-library]
- **DECIDED** — The engine's handle-shaped primitive surface is the public contract
  for native interop: DMA-BUF / OPAQUE_FD import and export, the present target,
  texture rings, codec byte pumps, the audio clock, color resolution. The
  Vulkan↔CUDA and Vulkan↔GL interop adapters survive as in-process capabilities
  (torch/cupy and GL consumers); only their cross-DSO `-abi` halves die with the
  plugin ABI. [importable-python-library]

## Processor model & scheduling — IN-FLIGHT (→ schema-agreement-ripout, importable-python-library)

- **DECIDED** — A link is pure plumbing: output port → input port, carrying a bag
  (self-describing msgpack named map). Producer and consumer type declarations are
  unilateral hints, never compared; consuming is a cast at read time. The engine
  mediates no schema agreement: connect never refuses a link (advisory log at most),
  no per-read tag matching, the wire tag is inert observability metadata, and versions
  never appear at the code layer — resolution-time only. [data-plane-cast-not-contract]
- **DECIDED** — Channel policy (delivery profile, ring depth, overflow) is declared
  port-locally at the consuming input port, never carried by schemas; a concretely-typed
  input port with no declared delivery profile is a wiring error, not a silent default.
  [data-plane-cast-not-contract]
- **DECIDED** — Three execution modes (reactive / manual / continuous); one dedicated
  OS thread per processor with descriptor-driven priority (realtime / high / normal);
  synchronous lifecycle traits; Full/Limited capability typestate on the phase axis
  (setup/teardown vs process). [execution-model]
- **DECIDED** — Execution placement is an engine concern, never a user-facing runtime
  definition. Both placements are first-class: in-process (lowest latency, shares the
  app's interpreter) and helper processes spawned from that same interpreter and venv
  (`sys.executable` — each with its own GIL, the isolation model dora-style systems
  run on). The engine chooses per processor; placement heuristics are engine
  implementation, not plan-level commitments. Same user code either way — no
  interpreter zoo, no per-processor environments, no placement configuration surface
  beyond a single opt-in. [importable-python-library]
- **DECIDED** — JTD schemas are advisory, experimental type information behind a
  rip-out-cheap seam — never a requirement for accessing data, in-graph or on the
  wire, and never baked in as fundamental (replaceable wholesale, e.g. by Arrow,
  without touching the data plane). No user-facing codegen verb.
  [importable-python-library]
- **OPEN** — Additional execution flavors to scale processor count (lightweight /
  green-thread style): intended, do not build until designed; hard constraint — no new
  configuration dials. [execution-model]

## Graphics (RHI / GPU)

- **DECIDED** — All Vulkan lives in the RHI (`vulkan/rhi/` + `streamlib-consumer-rhi`); one
  kernel abstraction per pipeline kind; consumers go through `GpuContext` only.
- **OPEN** — Everything else.

## Media I/O — camera, display, audio — IN-FLIGHT (→ importable-python-library)

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
- **DECIDED** — Python-authored media processors (vendor or user) are supported where
  deadlines allow: camera-class sources and block-level audio are viable behind the
  SDK's GIL-release contract; vsync-paced present loops and device audio callbacks
  stay native, always. [importable-python-library]
- **OPEN** — Audio backend: PipeWire-native on Linux is the intent (the current
  CPAL → ALSA path is interim); do not build until a research memo settles it. A/V
  sync model likewise OPEN. The engine's decided audio surface is the clock
  primitive. [media-io-layering]

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
  into Python — frames travel as handles / surface ids. [importable-python-library]

## Distribution & versioning — IN-FLIGHT (→ importable-python-library)

- **DECIDED** — Two artifacts, one version, released together: the streamlib wheel
  (Python API + CLI + engine) and the `streamlib` crate for Rust apps. Initial
  release channel is this repo's releases (pip/uv-installable wheel artifacts) —
  PyPI publication waits for the project rename; the artifact is identical either
  way. Positioning is "realtime engine, Python authoring" — the Rust engine is named
  as material; never marketed as "a Python library" even though the shape is one.
  [importable-python-library]

## Control plane & observability — IN-FLIGHT (→ importable-python-library)

- **DECIDED** — One control plane: the api-server's HTTP + WebSocket + MCP surface,
  hosted in-process by any runtime that enables it. The MCP tool set is the canonical
  control vocabulary; the CLI is a pure JSON-RPC client of it — agents and humans drive
  the same verbs; REST/WS routes serve the same operations for programmatic clients.
  [control-plane-one-surface]
- **DECIDED** — The api-server is engine-side infrastructure and relocates into the
  `runtime/` tree: it is a host — statically linked, never dlopen'd — and cannot follow
  the packages tree out of the repo. [control-plane-one-surface]
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

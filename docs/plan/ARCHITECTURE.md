# StreamLib Architecture Plan

The single source of architectural decisions. Sessions implement this plan; they do not make
architecture. A decision missing here stops work and comes back to the owner — it is never
inferred from existing code, consumers, or history. This document and the diagrams under
`diagrams/` (Mermaid `.mmd`, the committed source — Excalidraw files are generated views,
never round-tripped back) move together: every DECIDED entry is represented in the diagram.

Legend: **DECIDED** — build exactly this. **OPEN** — do not build; needs an owner decision.

## Product (the MVP sentence) — IN-FLIGHT (→ macos-platform-floor)
<!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_launch.py -->

- **DECIDED** — A Python developer on Linux with an NVIDIA GPU pip-installs streamlib
  (initially from this repo's releases; PyPI after the project rename) into an
  ordinary uv-managed venv, runs `streamlib new` then `streamlib dev`,
  sees their camera live in a window within a minute, and makes the pipeline theirs by
  editing the scaffolded processor — zero ceremony: no manifest, no `main()`, no schema
  wrangling, a fast edit loop. Every ticket traces to this sentence or does not
  exist. [importable-python-library — SHIPPED #1683, #1684, #1711]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli.py::test_new_writes_a_working_app -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_launch.py::test_the_scaffolded_app_reaches_a_running_graph -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_launch.py::test_every_helper_interpreter_goes_live_inside_the_startup_budget -->
- **DECIDED** — Terms of the sentence: StreamLib is an importable Python library — one
  PyPI wheel carrying the Python API, the CLI, and the Rust engine (PyO3, the
  pydantic-core model); a StreamLib app is a normal Python codebase — one venv, one
  Python version, ordinary PyPI dependencies, nothing dynamically downloaded;
  `dev`/`run` find `app.py`'s `setup(rt)` by convention, `-f <file>` overrides;
  processors are Python classes written in the app or imported from pip-installed
  packages, and `rt.add` takes the class; the pipeline API is `add`/`connect`.
  [importable-python-library — SHIPPED #1683, #1707, #1708]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_graph_building.py -->
- **DECIDED** — The zero-ceremony bar (the sentence is untrue until all hold): no
  manifest authoring; no boilerplate entry; bags/schemas fixed (no engine schema
  matching, cast-at-read, no versions at the code layer); scaffolding for app and
  processor; the scaffold pins `.python-version` (3.12) and the wheel supports a small
  Python version range. [importable-python-library — SHIPPED #1684, #1711; the
  bags/schemas clause with schema-free-ports #1814]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli.py::test_the_scaffolded_processor_lives_outside_the_entry_file -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_launch.py::test_the_edit_loop_survives_a_bad_save_and_shows_a_good_one -->
- **DECIDED** — Rust authoring stays a supported capability: a Rust app is a plain
  cargo project depending on the `streamlib` crate — no wrapper generation, no special
  format; third-party Rust processors for Rust apps are ordinary cargo dependencies,
  source-compiled. [importable-python-library — SHIPPED #1715]

## Packages & extension model — IN-FLIGHT

- **DECIDED** — PyPI and cargo are the package systems. The custom module system is
  deleted in full: `streamlib_modules/`, the `.slpkg` format, `streamlib.lock`, the
  package source, the `add`/`install`/`link`/`pkg` verbs, `BuildOrchestrator` and all
  runtime downloading or compiling. Compilation happens at publish time, by the
  author, with standard tools (maturin/CI for wheels, cargo for crates) — StreamLib
  never compiles user code.
  [importable-python-library; importable-python-library-ripout — SHIPPED #1715 for the
  verbs, `BuildOrchestrator` and every runtime build path; `streamlib-jtd-codegen` is
  gone with schema-free-ports #1813, and the remaining `.slpkg`, lockfile and
  package-source residue rode `streamlib-idents` into processor-class-identity —
  SHIPPED #1837, #1841, which deleted the crate whole]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-10-importable-python-library-ripout.md -->
- **DECIDED** — The plugin ABI is deleted: no dlopen'd processor cdylibs, no `repr(C)`
  vtable surface, and none of that ABI's load-time machinery — no dlopen load handshake, no
  cdylib build fingerprints. The scope is the deleted ABI and nothing wider: a helper process
  imports the one wheel and checks that the engine it imported is its parent's build, which
  is the handshake §Processor model states, not an ABI surface returning. The extension paths are
  Python packages and Rust source crates only — and an extension wheel is a Python
  package: Rust inside, loaded across the CPython ABI, never dlopen'd by the engine
  (extension-model, 2026-09-04).
  [importable-python-library; importable-python-library-ripout — SHIPPED #1715; the clause
  scoped to the ABI by local-transport-hardening — SHIPPED #2262]
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
  [importable-python-library — SHIPPED #1710, #1756, #1757]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py -->
- **DECIDED** — First-party optional capabilities ship the same way third-party native
  code does: as separate PyPI extension wheels — Rust inside for speed, a Python
  processor as the binding for any processor the wheel supplies — depending on the
  `streamlib` wheel as a binary and never building it from source. Optional means an app can be complete without it. The engine
  is not the home of every capability; it is the home of what belongs in core. Direction
  declared by the owner 2026-09-04, superseding the general rule — carried since the
  2026-08-02 pivot and applied nine times since — that a first-party native capability is
  a built-in. [extension-model]
- **DECIDED** — Two extension mechanisms, recorded as the current best understanding of
  the shape and expected to flex during the align and implementation. A *processor
  extension* is a Python processor class in a pip-installed package whose per-frame work
  runs in native code the same wheel carries: `rt.add(TheClass)` is its registration, as
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
  reaching past the surface. Known gaps at the pivot: a Python compute dispatch cannot
  bind a storage buffer, and codec sessions are not exported to Python. [extension-model;
  clause (c) added and first fired by virtual-camera-sink — SHIPPED #2196, #2197, #2198]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_virtual_camera_sink.py -->
- **DECIDED** — The `streamlib` wheel exports its bag codec as two module-level functions
  with stub entries: `encode_bag_to_msgpack_bytes(bag: Mapping[str, Any]) -> bytes` and
  `decode_msgpack_bytes_to_python_object(msgpack_bytes: bytes) -> Any`. They are the
  existing `encode_bag_to_msgpack` and `decode_msgpack_to_python_object` made reachable,
  with exactly the codec's rules — a dict with string keys at every level, the eight value
  types, `bytes` as `bin` at 1×, refusal by name of anything else — and no new behavior.
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
  addition. [extension-model]
- **DECIDED** — The support hook's contract, as built. A wheel declares
  `[project.entry-points."streamlib.extensions"] <name> = "<module>:load"`; the engine
  reads `importlib.metadata.entry_points(group="streamlib.extensions")` and calls each
  `load(host)` once per process taking an engine role — from `Runtime.__init__` in the app
  process, and from `_helper.py` between the log sink's installation and the processor
  class's import. `host` is `streamlib.CapabilityExtensionHost`, a `#[pyclass]` with a stub
  entry: `role` (`"app"` or `"helper"`) and `register_capability(name, version)`. In the
  app process a registration lands on the runtime and renders in `graph`; in a helper it is
  recorded for the extension's own reads. A hook that raises fails `Runtime()` with the
  distribution named; in a helper it fails that processor's start through the log channel
  and the parent refuses the processor by name, inside the existing 60 s budget. A second
  registration of one capability name refuses at the second hook, naming both
  distributions. `GraphResponse` gains `extensions: [{name, version, distribution}]`, a
  third top-level key, in the OpenAPI schema and the MCP `graph` tool alike. No opt-out.
  Discovery and the loop are Python; the runtime-side registry and the `graph` key are the
  one engine change. [networking-extension-wheels — SHIPPED #2149]
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
  lands with the first extension that needs one. [extension-model]
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
  for native interop: DMA-BUF / OPAQUE_FD import and export, the present target,
  texture rings, codec byte pumps, the audio clock, color resolution — surfaced to
  the Python ecosystem as DLPack and the CUDA Array Interface (DLPack first). The
  contract is zero-CPU-copy, stated honestly: tiled engine textures reach a linear
  tensor via one GPU blit into an exportable staging buffer, because DLPack expresses
  strided linear memory only — and that blit reads the surface's pooled backing
  whenever one exists; a producer-internal texture never sources a cross-process
  export. The Vulkan↔CUDA and Vulkan↔GL interop adapters survive
  as in-process capabilities (torch/cupy and GL consumers); only their cross-DSO
  `-abi` halves die with the plugin ABI. [importable-python-library — SHIPPED #1710;
  surface-id-lifetime-contract — SHIPPED #1868 for the source clause]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_pixel_exchange.py -->
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
  flavour by name, pointing at `export_opaque_fd`. An export is taken from a
  resolved surface, never from a name: the fd reaches a helper at checkout, so a
  texture acquired but not yet resolved is refused telling the caller to resolve
  its surface id first, and every other refusal likewise names the flavour's own
  door. The recipe travels because a raw allocation is consumed as an image — a
  linear buffer mapping over tiled memory yields block-linear bytes, never pixels —
  and a successful import pins the payload past the exporter destroying the texture
  it came from. [raw-handle-export-contract — SHIPPED #1900]
  <!-- verify: cargo test -p streamlib-adapter-cuda --test opaque_fd_wheel_export_foreign_consumer a_wheel_exported_opaque_fd_read_by_a_foreign_process_shows_the_kernels_pixels -->
  <!-- verify: cargo test -p streamlib-adapter-cuda --test opaque_fd_image_consumer_rhi_round_trip an_exported_opaque_fd_pins_the_payload_past_source_texture_teardown -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_a_texture_handle_round_trips_across_the_process_boundary -->
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
  engine inspects no bag content anywhere, save the top-level `surface_id` a remote link
  carries across the runtime mesh (§Networking). The producer never waits on a consumer:
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
  texture-backed-cpu-reach — SHIPPED #1942 for the staged cpu() arm]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_claimed_surface_pixel_access.py::test_the_bare_object_hands_back_the_surfaces_own_capsule -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_claimed_surface_pixel_access.py::test_a_two_surface_type_is_refused_every_bare_door_naming_the_surfaces -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_claimed_surface_pixel_access.py::test_a_frame_that_cannot_take_a_write_back_arrives_read_only -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_video_frame_claim.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_compute_kernel.py::test_a_raise_inside_the_staged_cpu_door_discards_the_edit -->

## Consumers — examples & packages — SHIPPED
<!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-31-consumer-tree-disposition.md -->

- **DECIDED** — `examples/` is the in-repo showcase and living documentation of the
  current authoring idiom, converted gradually and never a contract source: engine
  contracts are stated in the engine and proven by engine tests, and no example is read
  to infer what the engine guarantees. An external examples repository (the
  framework-repo/examples-repo model) remains a possible later move and is not decided
  now. [consumer-tree-disposition — SHIPPED #2053, #2054, #2055, #2056, #2057, #2058,
  #2059; the never-a-contract-source clause with the example-coupled E2E deleted in
  #2052]
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
  engine-adjacent Rust crate. [consumer-tree-disposition — SHIPPED #2052;
  extension-model — the first extension wheel is networking, which is what gives
  `packages/` its first live entry and makes the deferred publish path owed; until then it
  is `test-fixtures` beside the held consumers]
  <!-- verify: grep -n "packages/" Cargo.toml -->
- **DECIDED** — Conversion is a from-scratch rewrite in the current idiom, never an
  in-place upgrade: start from the `streamlib new` scaffold, mine the old directory for
  its logic only, author against today's full surface (delivery profiles, window
  contracts, cast objects, kernels-as-objects), and delete the old directory in the same
  PR. Every pre-pivot consumer neither deleted nor held below is conversion backlog under
  this doctrine; that backlog was filed and executed in full — `audio-mixer-demo`,
  `microphone-reverb-speaker`, `raytracing-showcase` and `fisheye-object-detection` (was
  `cuda-fisheye-detection`) rewritten in Python, `camera-compute-kernel` (was
  `camera-plugin-sdk-compute`) and `camera-halftone` (mined from the retired Deno
  example) rebuilt as kernel examples, and `tokio-integration` rewritten as a plain cargo
  project. `examples/` now stands at fourteen converted beside two held: the two
  vulkan-video examples left the held column into the proof rig,
  `camera-codec-roundtrip` joined the converted one as the codec blocks' showcase — a
  showcase authored in the current idiom is an ordinary addition under the convention
  below, not conversion backlog — and `camera-audio-recorder` converted out of the held
  column as the recording showcase its rung mined `packages/mp4` for: `CameraSource →
  H264Encoder → Mp4Sink` beside `MicrophoneSource → OpusEncoder → Mp4Sink`, the camera
  also fanned to a `DisplayWindow`, Ctrl-C stopping and closing the file. The networking
  move then emptied the held networking column into the converted one:
  `examples/moq-roundtrip` was deleted and rewritten as `examples/moq-broadcast-roundtrip`
  (publish and subscribe in one app through the relay to a `DisplayWindow`),
  `examples/webrtc-cloudflare-stream` was replaced by `examples/camera-webrtc-publish`
  (camera and microphone through the codec blocks to `WhipPublisher`, credentials from the
  environment), and `examples/whep-player` — a printed deferral at HEAD — was deleted
  outright. `examples/camera-virtual-camera` then joined the converted column as the
  virtual camera's showcase, a showcase in the current idiom under the same convention:
  one `CameraSource` fanned to a `VirtualCameraSink` and to a Python effect feeding a
  second one, so a graph appears as two named cameras in any other application on the
  machine and both are gone at Ctrl-C.
  [consumer-tree-disposition — SHIPPED #2053, #2054, #2055, #2056, #2057, #2058, #2059;
  the count restated at codec-roundtrip-reproof #2087, python-codec-block-api #2108,
  opus-mp4-recording-rung #2129, networking-extension-wheels #2153 and
  virtual-camera-sink #2198]
  <!-- verify: git ls-files examples/camera-halftone examples/camera-compute-kernel examples/fisheye-object-detection examples/camera-codec-roundtrip examples/camera-virtual-camera -->
  <!-- verify: git ls-files examples/camera-audio-recorder/app.py examples/camera-audio-recorder/pyproject.toml -->
- **DECIDED** — Retired in one sweep, superseded by deleted machinery or shipped pivots:
  `examples/pipelines`, `examples/camera-deno-subprocess` (its halftone effect rebuilt as
  `examples/camera-halftone`), `examples/camera-python-subprocess`,
  `examples/polyglot-manual-source`, `examples/camera-rust-plugin`,
  `examples/vulkan-video-roundtrip-cdylib-camera`, `examples/dynamic-reconfigure`,
  `examples/api-server`, `examples/api-server-demo`, `examples/runtime-graph-json-demo`,
  `examples/hello-streamlib` (the `streamlib new` scaffold is the hello; `camera-display`
  is the canonical minimal example), and `packages/audio`, `packages/camera`,
  `packages/display`, `packages/frame-tap`, plus the `packages/core` stub. The tree's one
  test that read a consumer as its fixture was deleted with the example it read, no
  replacement owed — a test owns its fixtures, and CI reaching into `examples/` would
  make a consumer a contract source. [consumer-tree-disposition — SHIPPED #2052]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-31-consumer-tree-disposition.md -->
- **DECIDED** — A consumer blocked on an undecided domain is held in-tree until the
  align covering that domain mines it for logic; its deletion rides that change's own
  ship. Held on codec blocks: `packages/jpeg` and `examples/jpeg-psnr` — `packages/h264`,
  `packages/h265`, `examples/vulkan-video-roundtrip` and `examples/vulkan-video-psnr`
  resolved this way and are gone, mined and deleted by the change that shipped their
  blocks [codec-roundtrip-reproof — SHIPPED #2087], and `packages/opus`, `packages/mp4`,
  `examples/h264-opus-validator` and the held form of `examples/camera-audio-recorder`
  went the same way with the recording rung [opus-mp4-recording-rung — SHIPPED #2129].
  The networking holds went the same way with the extension wheels:
  `packages/{moq,webrtc}` were mined for their WHIP/WHEP signalling, RFC 6184
  depacketiser and catalog shape and deleted, and their three examples resolved with them
  [networking-extension-wheels — SHIPPED #2153]. Held on audio
  plugins: `packages/clap`. Held on screen capture: `packages/screen-capture`,
  `examples/screen-recorder`. [consumer-tree-disposition — SHIPPED #2052; the sweep left
  every held consumer untouched]
  <!-- verify: git ls-files packages/clap packages/screen-capture examples/screen-recorder -->
- **DECIDED** — No additional native-processor *distribution* mechanism is owed
  pre-1.0: an extension wheel is an ordinary Python package on the ordinary index, and
  closed-source Rust processors for Rust apps are deliberately not a path — a
  closed-source vendor ships the Python package whose native internals expose handles.
  What the extension-model pivot adds is not distribution but *registration*: the
  capability extension's support hook, declared by a standard entry point pip records
  and the engine runs once per process, which §Packages & extension model owns.
  [consumer-tree-disposition — SHIPPED; extension-model — the registration door is the
  one addition, 2026-09-04]
- **DECIDED** — Lag-by-design ends for a converted consumer: when an engine change
  breaks one, the breakage is filed as tracked backlog at the consumer and never
  blocks the engine change.
  The showcase is kept current by convention, with no CI presence — a compile/import
  smoke check is a later ticket-level choice if rot appears. One exception, rare by
  design: an example serving as the deliberate canary of in-flight work is updated
  in-stream at the most appropriate time; a canary is normally planned as a separate
  path an example later adopts, so in-stream example surgery stays the exception.
  [consumer-tree-disposition — SHIPPED; a standing convention, and by the same decision
  the showcase carries no CI check to run]

## Processor model & scheduling — IN-FLIGHT (→ cross-runtime-links, macos-platform-floor)

- **DECIDED** — A link is pure plumbing: output port → input port, carrying a bag
  (self-describing msgpack named map). The engine has no type layer: ports carry no
  type declaration, connect never inspects or compares types and never warns, no read
  path examines a tag, and the frame header carries no schema ident. Consuming is a
  cast at read time; a mismatch surfaces as a decode failure at the consuming
  processor. Two carve-outs, and only two: declaring an audio window contract **is** that
  port's opt-in to the engine reading its bags as `AudioBlock`, so the engine inspects a
  payload on exactly the ports that asked it to; and a remote link reads a bag's top-level
  `surface_id` to carry the frame across the runtime mesh (§Networking) — reading that one
  key, never a type, a tag or anything else in the bag. A link into a port with
  no contract is unchanged in every respect — still pure plumbing, `connect` still
  compares nothing, and the frame header still carries no schema ident.
  [schema-free-ports — SHIPPED #1814; the carve-out — audio-port-window-contract, SHIPPED
  #2033]
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
  processor id>/<output port>`, the name `graph` and `tap` already show — or, for a remote
  link, by its mesh address `<runtime name>/<display name>/<output port>` (§Networking), so a
  many-track sink fed across the mesh names its tracks the same way on every run. The mailbox
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
  [opus-mp4-recording-rung — SHIPPED #2124; the timestamped spelling with
  networking-extension-wheels — #2150]
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::input::tests::two_inbound_links_hand_a_reader_the_link_each_bag_arrived_on -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::input::tests::naming_the_inbound_link_a_bag_arrived_on_leaves_the_per_link_drop_counts_alone -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::input::tests::a_port_lists_the_inbound_links_wired_into_it_and_a_port_with_none_lists_none -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::input::tests::an_injected_bag_with_no_inbound_link_is_refused_by_name_rather_than_borrowing_one -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_inbound_link_read_with_timestamp.py -->
- **DECIDED** — The delivery profile is the whole of channel policy: one word, declared
  port-locally at the consuming input port. Every input port declares its delivery profile explicitly — there is no default
  and nothing left to infer one from, so an input port without one is a wiring error.
  Ring depth and overflow policy are engine-chosen and are not authorable: no port
  declares a depth, a leak policy, or a queue element, and there is no second surface
  that tunes one. [schema-free-ports — SHIPPED #1811; delivery-profile-vocabulary —
  SHIPPED #2024, #2025]
  <!-- verify: cargo test -p streamlib-engine missing_declaration_is_a_wiring_error_naming_the_port -->
  <!-- verify: sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_an_input_port_without_a_delivery_profile_is_refused -->
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
  port, and is deliberately uncounted. The clause is now true of the tree as well as of the
  intent: the mailbox eviction, the subscriber ring's overwrite, the two receive-seam
  discards and a write refused at the channel ceiling are each counted where they happen and
  rendered under `metrics`, for a helper-placed processor as for an app-process one. A
  `graph` that reports no drops is proof that none happened, with the two residuals the
  entries below state.
  [delivery-profile-vocabulary — SHIPPED #2023 for the app-process half; made true of the
  tree by loss-visibility — SHIPPED #2268, #2269, #2270]
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
  port counts neither** — the eviction at a skip-to-latest mailbox is the profile working,
  which is the tree catching up to the entry above rather than a new rule. And **a bag
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
  destination's link does not see this loss. Rejected — adding the refusals into each
  outbound link's `dropped_bags_by_link` at its destination, which would need per-link
  baselines at the producer and a merge of two processes' counters at render. Owner,
  2026-09-14. [loss-visibility — SHIPPED #2268]
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
  The wheel's stub changed after all: `ProcessorLinkDataAccess` gained `open_loss_count_board`
  and one optional keyword on each of its two link-opening methods, which the helper passes
  and no processor author ever names. No authoring surface moved. The engine half of the
  proof is CI-run; the end-to-end wheel arm — an overrun helper, a ceiling refusal and a
  SIGKILL'd helper's last counts, all read off a running node's `graph` — is
  `requires_gpu` and therefore rig-only.
  [loss-visibility — SHIPPED #2270]
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_helper_placed_destinations_node_renders_the_counts_its_helper_wrote_on_its_slot -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::helper_process_loss_count_board::tests::the_parent_reads_each_entry_only_for_the_wiring_it_assigned -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::helper_process_loss_count_board::tests::a_write_from_a_wiring_the_slot_has_moved_past_lands_nothing -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::input::tests::every_loss_counted_on_a_mirrored_link_reaches_its_board_slot_as_it_is_counted -->
  <!-- verify: pytest -m requires_gpu sdk/streamlib-python-wheel/tests/test_helper_loss_counts.py -->
- **DECIDED** — No link ever blocks a producer. Producer-blocking is deleted, not merely
  unreachable: no profile resolves to it and the overflow policy it was the second half
  of goes with it. A processor publishing to a slow consumer loses bags at that
  consumer's port, counted as the entry above states; it is never parked. The capability
  was never engineered — the engine never chose the blocking semantics it would have had,
  and a parked
  producer cannot observe shutdown — and keeping it cost the tree two standing
  workarounds. Counted drops land before or with the deletion, so the alternative to
  silent loss exists the moment blocking stops being one.
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
  frame header and never as engine-visible type. [delivery-profile-vocabulary — a
  standing constraint on the endpoints; no code was owed, and nothing in the tree
  carries an encoded stream yet]
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
  is the only such schema. [schema-free-ports — SHIPPED #1813, #1815; the
  `SchemaIdent` grammar itself — processor-class-identity, SHIPPED #1841; narrowed by
  agent-readable-processor-catalog — SHIPPED #2224, #2226]
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
  refuses one. Reconfiguration takes the same object. Processors in the engine tree
  written the old way migrate with the change; consumers lag as §Consumers states.
  [agent-readable-processor-catalog — SHIPPED #2224 for Rust, #2226 for Python]
  <!-- verify: cargo test -p streamlib-engine --test attribute_macro_test the_descriptor_carries_the_config_types_schema_rather_than_its_name -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_config_class.py -->
- **DECIDED** — The Rust half, as built. `ProcessorDescriptor.config_schema` is
  `Option<serde_json::Value>` — the config type's document, `None` only on a descriptor
  built by hand without one — and `ProcessorDescriptorOutput` mirrors it, so
  `/api/registry` serves the same document the MCP catalog does. `#[processor]` emits it
  through `ProcessorConfigJsonSchema`, a blanket trait over `schemars::JsonSchema` whose
  `#[diagnostic::on_unimplemented]` note names the derive and the SDK's re-export at
  `streamlib::sdk::schemars`, so a config type without the derive fails on a message
  naming the fix rather than on a bare trait bound; the synthesized config id and the
  `config_schema` attribute key are gone. `EmptyConfig`, what a processor with no
  `config =` gets, publishes an object with no properties and `additionalProperties:
  false`, and refuses a non-empty map naming the key that had nowhere to go. Every
  in-tree config type derives the schema — the built-ins' configs, their enums and
  `ApiServerConfig` — and a field's `///` doc is its `description`, a serde default its
  `default`, a field with neither `required`. The per-field `ConfigDescriptor` trait,
  its derive, `ConfigField` and `ConfigFieldOutput`, which nothing constructed, are
  deleted. Every catalog document, from either language, is JSON Schema draft 2020-12
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
  stamped as `__streamlib_processor_config_class__` and
  `__streamlib_processor_config_schema__`. The deriver is stdlib-only and emits 2020-12
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
  The wire is unchanged: `rt.add(cls, config={…})` carries a dict, the graph node stores
  its JSON, and `ctx.config` stays that mapping. The six engine-tree fixtures migrated
  with the change, and so did the four extension-wheel processors, as the canary
  §Consumers reserves — owner ruling 2026-09-11, since `packages/` is the one consumer
  tree CI runs; the fourteen example processors still written the old way are filed as
  backlog at their seven examples, #2236–#2242. [agent-readable-processor-catalog —
  SHIPPED #2226]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_config_class.py::test_a_keyword_parameter_is_refused_with_the_fix_named -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_config_class.py::test_the_helper_constructs_the_processor_by_the_config_keyword -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_config_class.py::test_the_document_is_2020_12_with_no_meta_schema_key -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_config_catalog.py -->
  <!-- verify: pytest packages/streamlib-moq/tests/test_processors.py -->
  <!-- verify: pytest packages/streamlib-webrtc/tests/test_processors.py -->
- **DECIDED** — Port rendering in the control plane is name, description, delivery
  profile, direction, and — on an audio input that declared one — its window contract; no
  port carries a type in `graph`, `tap`, or any snapshot. A port that declared nothing
  renders no `audio_window` key at all rather than a null. A `match_device` port renders
  the five values its device settled — machine-dependent because the device format is,
  which is truer than a static lie — and renders the sentinel itself while nothing has
  settled it. [schema-free-ports — SHIPPED #1816; the fifth key —
  audio-port-window-contract, SHIPPED #2032, #2034]
  <!-- verify: sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_a_declared_port_carries_no_type_key_under_any_spelling -->
  <!-- verify: sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_a_port_declaring_no_contract_carries_no_audio_window_key -->
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
  placement. [helper-process-placement-only — SHIPPED #1714]
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
  Five readings the build settled, each binding where an implementer would otherwise choose
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
  holds its pipe. And the ladder is Linux-first: process groups, `waitid` and
  CLOEXEC-at-source compile on both platforms, with macOS closing descriptors one at a time
  where `close_range` is absent, while escalation and SIGHUP on macOS's
  `ctrlc` / `NSApplication` path are not built.
  [shutdown-ladder; local-transport-hardening — SHIPPED #2264, #2266]
  <!-- verify: sdk/streamlib-python-wheel/tests/test_helper_placement.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_helper_placement.py::test_a_processor_interrupted_while_still_setting_up_still_tears_down -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_helper_placement.py::test_a_worker_a_processor_forked_goes_down_with_the_apps_helper -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_three_helpers_slow_to_stop_cost_about_one_ladder -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_a_process_the_app_started_never_holds_the_apps_output_past_its_exit -->
- **DECIDED** — A link onto a helper reads `wired` only once the helper says so. The helper
  answers every `wire_link` it receives with a link-scoped `wired` or `wire_failed` reply,
  `wire_failed` carrying the reason its open failed; the reply rides its own rpc tag, which
  the bridge routes to that link's state and never to the lifecycle reply queue. A link
  carried in the startup envelope needs no reply of its own — `ready` confirms it, and a
  failure there still refuses the processor's start by name. `connect` does not wait for the
  reply: it returns with the link `pending`, and the helper's answer flips it to `wired`, or
  to `error` carrying the helper's own reason, which `graph` renders under `error_reason`
  until the link is disconnected — the first use of `LinkState::Error`. The caller learns
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
  and names the helper's stderr. An absent id is a refusal too, never a silent pass. The
  subprocess protocol version number, its minimum, its validator and its environment
  variable retire with it: one check per invariant, and a hand-bumped integer never caught a
  helper built against a different iceoryx2 patch or a stale wheel on the helper's
  `sys.path`. [local-transport-hardening — SHIPPED #2262]
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
  `python app.py` identifies as `__main__:<Type>` and is a wiring error at `rt.add`,
  with an error naming the fix (move the class to an importable module and import it
  from the entry file — one import line). The entry file itself may still run as
  `__main__`; only processor classes may not live there.
  The `@org/package/Type` identity grammar is deleted along with
  the `@app/local` synthesis; `@processor` declares execution, interval, scheduling
  priority, and description only. Mechanically means at the authoring seam, never from a
  runtime reflection API: Rust captures the type path where the macro expands, because
  `std::any::type_name`'s output format is unspecified across compiler versions and must
  never key a registry; Python joins `__module__` and `__qualname__` with a colon. The
  per-processor isolation tier goes with the grammar — it was derived from the org, so
  with no org it has one reachable answer, and the operator knob that only ever asked
  "is this module `@session`?" collapses with it. The `FullAccessGrant` moat survives
  untouched: it is a compile-time guarantee about who may mint an in-process
  `RuntimeContextFullAccess`, never a placement question.
  [processor-class-identity — SHIPPED #1837, #1839, #1840, #1841; `__main__` clause
  reversed by helper-process-placement-only]
  <!-- verify: cargo test -p streamlib-engine --test processor_class_import_path_test -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_identity.py::test_the_launch_arrangement_never_changes_the_identity -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_identity.py::test_a_processor_declared_in_the_entry_file_is_refused -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-12-processor-class-identity.md -->
- **DECIDED** — A Python processor class registers its descriptor — identity,
  description, ports, config schema — when `@processor` runs, so it is in the processor
  catalog before its first add; the constructor arrives at first add exactly as today.
  Decoration inside a helper process registers nothing, because a helper hosts no
  graph. A class decorated twice under one import path meets the existing
  duplicate-path refusal. As built: after stamping, the decorator calls the
  wheel-internal `register_declared_processor_class`, which reads the class as `rt.add`
  does and registers the descriptor alone through `register_descriptor_only`. It passes
  over two classes — one decorated where `STREAMLIB_ENTRYPOINT` is in the environment,
  which is how a helper knows itself, and one with no import path (declared inside a
  function, or in the entry file), whose refusal stays at `rt.add` with the fix named.
  At first add `ProcessorInstanceFactory::install_constructor_for_registered_descriptor`
  gives the registered descriptor its constructor, refusing a path that already has one
  with the two-classes-one-path text and a path nobody registered by name; the
  unregistered-type resolver keeps its shape, since importing the module runs the
  decorator before the constructor is installed. A second decoration of one import path
  is refused at import naming `importlib.reload`, and the first registration stands. A
  class decorated with no `description=` registers its docstring, or `""`.
  [agent-readable-processor-catalog — SHIPPED #2228]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_declaration_registers.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_config_catalog.py::test_a_class_the_app_imported_and_never_added_is_in_the_catalog -->
- **DECIDED** — An instance's display name is the human-facing label — passed at `add`,
  readable off the returned handle, and the prefix on its log records; it defaults to
  the class's short name and the engine disambiguates duplicates within one graph. It is
  also the processor's part of its address on the runtime mesh (§Networking), so renaming a
  processor re-addresses its ports. Being part of that address is what bounds it: `add`
  refuses a requested display name that is empty, contains `/`, `*`, `$`, `#` or `?`, or
  begins with `@`, naming the character and the fix, in Rust, in `rt.add` and in MCP
  `add_processor` alike. Spaces and unicode stay legal, and a class's short name and the
  engine's ` 2` suffix always pass, so no default display name is ever refused.
  Identity is never derived from it — and neither is the default: a descriptor carries
  the class's short name as its own validated field rather than the engine splitting one
  out of the import path, because splitting re-invents the grammar this change deleted.
  [processor-class-identity — SHIPPED #1838, #1841; the address-chunk refusal —
  runtime-mesh, SHIPPED #2282]
  <!-- verify: cargo test -p streamlib-engine --test display_name_disambiguation_test -->
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::mesh_address_chunk -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_graph_building.py::test_a_duplicate_requested_display_name_is_disambiguated_too -->
- **OPEN** — Additional execution flavors to scale processor count (lightweight /
  green-thread style): intended, do not build until designed; hard constraint — no new
  configuration dials. [execution-model]

## Graphics (RHI / GPU) — IN-FLIGHT (→ macos-platform-floor)

- **DECIDED** — All Vulkan lives in the RHI (`vulkan/rhi/` + `streamlib-consumer-rhi`); one
  kernel abstraction per pipeline kind; consumers go through `GpuContext` only.
- **DECIDED** — The engine's kernel primitives are exposable to Python as configured
  blocks: shader/compute source and binding config passed from Python, compiled and
  executed by the engine on its device — no user-side Vulkan, ever.
  [importable-python-library — SHIPPED #1717 for the align; python-kernel-surface —
  SHIPPED #1773, #1775]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_compute_kernel.py -->
- **DECIDED** — Python reaches every kernel kind Rust authoring reaches: compute,
  graphics and ray-tracing kernels, acceleration structures, and CPU readback. Python
  names and drives; the engine allocates, compiles, binds, and dispatches. No kernel
  kind is Rust-only. Pipeline state and buffer resources inside a kind are a narrower
  claim, and the two a Python processor cannot reach are named rather than left silent:
  vertex and index buffers with indexed draws — no escalate op mints either buffer, and
  no consumer in either language binds one; and storage- and uniform-buffer bindings —
  Rust consumers in the engine tree hold them, and the only by-surface-id resolution the
  escalate path has is texture-shaped, so a Python processor is refused by name. Both
  are undesigned.
  [python-kernel-api; python-kernel-surface — SHIPPED #1773, #1774, #1777;
  kernel-kind-parity-bar — the parity claim narrowed to kernel kinds]
  <!-- verify: cargo test -p streamlib-engine compute_kernel_dispatch -->
  <!-- verify: cargo test -p streamlib-engine graphics_kernel_dispatch -->
  <!-- verify: cargo test -p streamlib-engine ray_tracing_kernel_dispatch -->
  <!-- verify: cargo test -p streamlib-engine cpu_readback_answers_from_gpu_context -->
  <!-- verify: sdk/streamlib-python-wheel/tests/test_graphics_kernel.py::test_a_draw_takes_no_vertex_buffer_no_index_buffer_and_no_depth_target -->
  <!-- verify: sdk/streamlib-python-wheel/tests/test_graphics_kernel.py::test_a_graphics_kernel_carries_no_depth_or_vertex_input_state -->
- **DECIDED** — A kernel's output is an engine-owned texture that Python names by
  surface id and passes downstream in a bag, and that a third-party GPU library in its
  own Python package reaches through a scope: entering blits the texture to a linear
  view (DLPack over DMA-BUF / OPAQUE_FD), leaving blits any write back and orders it on
  the surface's timeline ahead of the engine's next read. The engine owns that
  ordering — no fence or timeline vocabulary reaches Python. Leaving the scope by a
  propagating exception discards the write instead: a half-written view blitted back
  publishes a torn frame that surfaces as corrupt pixels somewhere downstream rather
  than at the `raise`, so the engine keeps the complete frame it already holds and lets
  the exception propagate — one rule for both device-write scopes, the CPU pixel-buffer
  scope included (the surface handle's scope and its pending *device* write —
  distinct from the cast object's `cpu()`, whose coherent-mapped stores publish per
  store; its staged arm over a texture backing follows this same discard rule —
  see the cast-object entry in §Packages), and discarding never suppresses the
  exception. A write-back is always
  an edit of a frame the processor read, never a fresh-frame write: the engine refuses
  a write-back into a staging that has not first read that same frame, because it cannot
  tell a consumer's write from uninitialised memory and one staging spans every frame its
  pool slot publishes. Cross-process texture import is part of the capability, and
  importability is an allocation flavour the engine derives per acquisition, never a
  Python dial: single-plane render-attachment usage takes explicit-modifier DMA-BUF
  where the render-target modifier probes available; a CUDA-mappable format whose usage
  sits inside the OPAQUE_FD set takes OPAQUE_FD where that image pool exists; everything
  else keeps a non-importable allocation. A flavour the device or format cannot take
  falls back rather than failing the acquire, and the later cross-process import refuses
  by naming the flavour.
  [python-kernel-api; python-kernel-surface — SHIPPED #1778, #1779]
  <!-- verify: cargo test -p streamlib-engine the_seam_refuses_to_publish_a_staging_no_frame_was_read_into -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_a_raise_inside_the_device_tensor_scope_discards_the_write -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py::test_a_texture_handle_round_trips_across_the_process_boundary -->
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
  not publish on failure edits outside the scope. Every staging copy blocks:
  `contended` reaches no author, and the unconsumed non-blocking surface — the
  `try_run_cpu_readback_copy` wire op,
  the `contended` response variant, and the engine's `try_`-prefixed staging
  copies — is deleted. The readback staging allocates host-cached from a third
  OPAQUE_FD pool (probed HOST_ACCESS_RANDOM), falling back to the sequential-write
  pool on a device with no cached exportable memory type — slower there, never
  refused. Every OPAQUE_FD checkout binds the exporter's stated memory type index:
  the staging registration puts it on the surface-share wire as texture
  registrations already do, and an importer that cannot bind the stated index is
  refused by name — a conforming OPAQUE_FD import has no fd-properties query to
  derive one from. Python's `acquire_texture` implies `copy_src` and `copy_dst`;
  Rust's descriptor stays explicit; a texture whose usage still cannot take the
  copy refuses the door by name.
  [texture-backed-cpu-reach — SHIPPED #1940, #1941, #1942]
  <!-- verify: cargo test -p streamlib-engine a_device_whose_probed_type_is_not_host_cached_gets_no_host_cached_pool -->
  <!-- verify: cargo test -p streamlib-engine a_staging_registration_states_the_exporters_memory_type_index -->
  <!-- verify: cargo test -p streamlib-engine parse_texture_usages_combines_tokens_and_implies_both_copy_bits -->
  <!-- verify: cargo test -p streamlib-engine the_seam_publishes_a_staged_edit_back_into_the_pooled_backing -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_compute_kernel.py::test_a_texture_backed_surfaces_pixels_reach_the_cpu_with_numpy_alone -->
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
  cross-camera corruption in `CameraSource` as much as in the new sink. Any per-processor
  path that dispatches conversion on its own thread takes `create_color_converter` instead.
  [virtual-camera-sink — SHIPPED #2196]
  <!-- verify: cargo test -p streamlib-engine the_yuyv_pass_writes_every_pixel_of_the_target_range -->
  <!-- verify: cargo test -p streamlib-engine an_owned_color_converter_shares_no_kernel_with_the_cached_one -->
- **DECIDED** — A consumer of a *published* frame barriers it out of whatever layout it
  was observed in into the layout its own descriptor declares, and republishes that layout
  on the registration; only `UNDEFINED` is refused, because only `UNDEFINED` is free to
  discard the picture. Hard-coding the arriving layout is the bug it replaces: a frame
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
  narrower contract is an engine-wide call, not one a built-in makes for itself.
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
  capabilities of `GpuContext`, reached the same way by every caller. The four bridge
  traits and their installation step are deleted: no kernel capability can be absent at
  runtime, and no application glue supplies one.
  [python-kernel-api; python-kernel-surface — SHIPPED #1773, #1774, #1777]
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
- **OPEN** — Everything else, including the two graphics capabilities no language can
  render: depth attachments — Rust constructs a depth-testing pipeline that Python cannot
  name, and no pass in either language renders against one — and MSAA, refused for every
  caller in every language with the pipeline hardcoded to a single sample. Both are
  unbuilt engine capabilities rather than Python-reach gaps; equalising the construction
  surface with no pass to render against would buy nothing.

## Media I/O — camera, display, audio, codecs — IN-FLIGHT (→ cross-runtime-links, macos-platform-floor)

- **DECIDED** — First-party camera, display, and audio are native built-in processors
  in the engine tree, statically linked into the wheel — pre-built named blocks
  instantiated and configured from Python (`rt.add(CameraSource)`), whose per-frame
  paths never enter the interpreter. Lag-by-design ends: built-ins ship inside the
  wheel, current by construction. Since 2026-09-04 this names the shipped set, not a
  rule: a further first-party capability is a built-in only under the criterion in
  §Packages & extension model, and is otherwise an extension wheel.
  [importable-python-library — SHIPPED #1709; extension-model for the scope clause]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_native_builtin_blocks.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_launch.py::test_a_native_block_added_without_config_reaches_a_running_graph -->
- **DECIDED** — A virtual camera is a fifth device class and a built-in under criterion
  (c): `VirtualCameraSink`, in the media built-ins crate, Linux only, as many instances
  as the graph adds — the display's rule. Each instance is one camera that exists only
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
  whatever a caller states. Owner rulings 2026-09-06: widened from loopback-only to two
  doors, then from pre-loaded devices to a device per processor. Proven on the rig for the
  loopback door — two named cameras from one graph, read back as YUYV with matching
  colorimetry, and both gone at a shutdown no reader was holding, which is the removal's
  good case and not a proof against the `EBUSY` path above. The PipeWire door is proven as
  far as registration and negotiation (WirePlumber lists the node beside its V4L2 cameras;
  the offer carries the modifier and its shared-memory sibling) and no further: no consumer
  reachable on the rig negotiates a PipeWire camera at all — `pipewiresrc` fails identically
  for WirePlumber's own V4L2 devices, and Chrome 152 ships the flag off — so which door a
  consumer takes and what stamp it observes is unproven, and closing it needs a machine with
  a working PipeWire camera consumer. [virtual-camera-sink — SHIPPED #2196, #2197, #2198]
  <!-- verify: cargo test -p streamlib-media-builtins virtual_camera_sink -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_virtual_camera_sink.py -->
  <!-- verify: cargo test -p streamlib-engine a_pipewire_camera_node_offers_a_modifier_and_a_shared_memory_sibling -->
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
  primitives third parties get — DMA-BUF / OPAQUE_FD import-export, present target,
  audio clock, color resolution, codec sessions — never against private engine guts;
  the layering wall survives the ABI's deletion as internal discipline.
  [importable-python-library — SHIPPED #1709, #1710]
- **DECIDED** — V4L2 is the only capture backend (platform floor: Linux + NVIDIA).
  Apple capture (AVFoundation) is post-MVP and undesigned; only the TCC permission
  shims exist. [media-io-layering]
- **DECIDED** — Windowing: the engine owns the process's one event pump and mints
  windows on request; a window-owning processor registers with it and keeps every
  window policy decision — title, extent, what a resize means, when to redraw, what
  closing does. winit permits one event loop per process, so the loop is owned once,
  above every processor that wants a window, and N window-owning processors coexist
  in one process: the built-ins crate depends on winit no longer, so the engine holds
  the only construction site there is. Each window's owner renders on its own thread,
  never the pump's, so windows are not serialised behind one render loop — a claim
  about the render loop, not the device, since two windows still share one `VkDevice`
  and its queues. The raw-window-handle seam remains the internal boundary — the
  engine mints the present target from the raw handle and owns every swapchain and
  acquire detail,
  plus the platform main-thread event loop where the OS demands it (in the importable
  arrangement the process main thread belongs to the user's script; `rt.run()` blocks
  with the GIL released while the engine pumps). A processor that cannot get a window
  drains and discards, so upstream still sees a live consumer.
  [importable-python-library — SHIPPED #1707 for the `rt.run()` clause;
  shared-window-event-pump — SHIPPED #1734]
  <!-- verify: cargo test -p streamlib-engine --test window_event_pump_serves_many_windows -->
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
  window. [processor-owned-windows — SHIPPED #1928, #1929, #1930]
  <!-- verify: cargo test -p streamlib-engine --test processor_owned_window_over_the_escalate_wire -->
  <!-- verify: cargo test -p streamlib-engine --test processor_owned_window_shows_named_surfaces -->
  <!-- verify: cargo test -p streamlib-engine --test processor_owned_window_refused_without_a_display_server -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_owned_window.py::test_all_three_ways_of_naming_a_published_surface_reach_the_window -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_owned_window.py::test_a_users_close_leaves_the_pipeline_running_and_the_owner_informed -->
- **DECIDED** — Camera → GPU transport: zero-copy DMA-BUF import when the device
  exports it, transparent CPU-upload fallback otherwise, selected automatically —
  no configuration dial. [media-io-layering]
- **DECIDED** — Python-authored media processors (vendor or user) run in their own
  helper process like every other Python processor and are supported where deadlines
  allow: camera-class sources and block-level audio fit within the helper hop's
  budget; vsync-paced present loops and device audio callbacks stay native, always —
  a deadline the cross-process hop cannot meet, not a GIL argument.
  [importable-python-library; helper-process-placement-only — SHIPPED #1714]
- **DECIDED** — One clock on the data plane: every timestamp a processor stamps, reads,
  or compares — frames, bags, audio ticks, `ctx.time` — is the machine's monotonic clock
  (`CLOCK_MONOTONIC` on Linux, `mach_absolute_time` on Apple), the same epoch the V4L2
  and ALSA driver stamps carry, comparable across every node on a host. No
  process-relative epoch anywhere, and each language exports exactly one name for it.
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
  [one-monotonic-clock — SHIPPED #1725, #1726, #1727, #1728; the pubsub event timestamp no
  listener ever received left the list with the in-process event bus, #2276]
  <!-- verify: cargo test -p streamlib-engine --lib now_lands_in_the_kernel_monotonic_domain -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_clock_and_log.py::test_monotonic_now_ns_reads_the_kernel_monotonic_clock -->
  <!-- verify: cargo run -p xtask -- check-clock-usage -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-13-one-monotonic-clock.md -->
- **DECIDED** — Audio backend: PipeWire-native, reached by runtime dlopen — the
  MIT-licensed PipeWire/SPA headers are vendored, the header-only SPA layer compiles
  into the wheel as a small shim, and every `pw_*` symbol binds at runtime — falling
  back to dlopen'd `libasound`, falling back to a null backend under which audio
  processors run, produce silence, and discard. The chain is probed once per process
  and logged once, no configuration dial and no environment override, and no audio
  library ever appears in the wheel's `DT_NEEDED`. **An arm is chosen by opening, not
  by loading**: a library that resolves but yields no usable connection — `libpipewire`
  present with no daemon answering, the common container case — demotes to the next arm
  exactly as a missing library does, because probing on presence alone would strand
  precisely the machines the chain exists to serve. A caller-named `device_id` is the
  one case that does not demote: it raises at `setup()`, since a wrong device id is a
  wiring error and silently landing on a different device is worse than failing. CPAL
  is gone with it: no audio path links an audio library, interim or otherwise.
  [audio-subsystem; dlopen-audio-backend-and-audio-blocks — SHIPPED #1989, #1990, #1991]
  <!-- verify: cargo test -p streamlib-engine --lib the_chain_is_probed_once_and_hands_back_the_same_backend_every_time -->
  <!-- verify: cargo test -p streamlib-engine --lib the_walk_demotes_past_every_arm_that_declines_in_the_order_it_was_given -->
  <!-- verify: cargo test -p streamlib-engine --lib the_linux_chain_offers_pipewire_then_alsa_before_falling_through_to_null -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_microphone_source.py::test_a_device_that_was_named_and_cannot_be_opened_refuses_at_setup -->
- **DECIDED** — The audio device seam is an engine primitive beside the audio clock,
  not built-in-private code: `AudioDeviceBackend` opening `AudioCaptureStream` and
  `AudioPlaybackStream`, living in `core/context/` with its Linux implementations under
  `linux/`, exactly where the audio clock's two halves already sit. `MicrophoneSource`
  and `SpeakerSink` are written against it and reach no engine guts — the layering wall
  above, applied to a fourth device class. There is no second audio device path: the
  built-ins, the null backend and every test open streams through this one seam. A
  stream carries a liveness report its owner reads, so a publishing or draining thread
  whose device died comes back and says why rather than only telling the log.
  [dlopen-audio-backend-and-audio-blocks — SHIPPED #1989, #2012]
  <!-- verify: cargo test -p streamlib-engine --test silent_null_arm_captures_without_ever_dying -->
  <!-- verify: cargo test -p streamlib-engine --test silent_null_arm_plays_what_it_is_given -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib a_publishing_thread_whose_device_died_comes_back_and_says_why -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib a_drain_thread_whose_device_died_comes_back_and_says_why -->
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
- **DECIDED** — The portability gate is the design's pass/fail, unchanged and
  unweakened: the shipped `_engine.abi3.so` names the same five host libraries after
  audio as before. No name is added to the permitted-host-library list — an audio
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
  explicitly so the stamp cannot arrive on `CLOCK_REALTIME` — and it is published
  through the timestamped write, never the implicit one, whose `MediaClock::now()`
  would stamp the moment of publication rather than the instant of capture. Both the
  bag field and the frame header therefore carry the same device-derived value, in the
  same epoch a video frame's timestamp carries, which is the whole of block-level A/V
  sync: joining audio to camera frames is subtracting two integers.
  [dlopen-audio-backend-and-audio-blocks — SHIPPED #1990, #1991]
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
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_audio_block_cast.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_audio_block_cast.py::test_an_audio_block_takes_no_surface_and_holds_no_claim -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_audio_block_cast.py::test_the_numpy_types_are_spelled_little_endian_at_the_source -->
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
- **DECIDED** — The wheel's own test harness must be able to decode a bag carrying
  bytes, so the defect that stopped it is fixed at the engine layer rather than
  bandaided in audio: the collector `await_bag` is built on decoded bags through a JSON
  value whose visitor implements no `visit_bytes`, so every `bin` payload failed with a
  type error. It decodes through `rmpv` instead, the way the tap path already does. This is
  not audio-specific — any bag carrying bytes hits it, including one a Python processor
  writes today. [dlopen-audio-backend-and-audio-blocks — SHIPPED #1988]
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
  [audio-subsystem; audio-port-window-contract — SHIPPED #2032; the optional count —
  opus-mp4-recording-rung, SHIPPED #2123]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_an_audio_input_declares_its_window_contract -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_an_omitted_hop_defaults_to_the_window_size -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_an_output_port_takes_no_window_contract -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_an_omitted_channel_count_follows_the_source -->
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
  [audio-port-window-contract — SHIPPED #2032, #2033; the `channels` carve-out —
  opus-mp4-recording-rung, SHIPPED #2123]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_a_hop_above_the_window_size_is_refused_naming_both_numbers -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_a_contract_beside_a_skipping_delivery_profile_is_refused_naming_both_knobs -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_a_partial_contract_is_refused_naming_the_missing_fields -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_every_value_but_the_channel_count_is_still_required -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_a_declared_channel_count_of_zero_is_still_refused -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_second_inbound_link_into_a_windowed_port_is_refused_naming_the_port_and_both_links -->
  <!-- verify: cargo test -p streamlib-engine --lib iceoryx2::audio_window::audio_window_stage_tests::a_channel_pair_with_neither_side_at_one_is_refused_naming_both_counts -->
- **DECIDED** — One stage, at the one read seam every reader already shares. It sits in
  `read_raw_bounded`, which an app-process Rust processor reaches through the parent's
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
  [audio-port-window-contract — SHIPPED #2033; the flush count, the ring cap and the live
  refusal — loss-visibility, SHIPPED #2269]
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
  Overwrite counting has now shipped (§Processor model), so the question is answerable where
  it was not before; it is still undecided, and until it is decided the connect is refused as
  the entry above states. [loss-visibility — the precondition met by #2268; the question
  itself untouched]
- **DECIDED** — The resampler is `rubato` — pure Rust, MIT, adding no `DT_NEEDED` entry —
  and the portability gate stays the pass/fail: the shipped `_engine.abi3.so` names the
  same five host libraries with the resampler in as without it. Its three adapter
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
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_the_device_matching_sentinel_is_on_no_public_surface -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_the_device_matching_sentinel_is_refused_at_decoration -->
- **DECIDED** — `MicrophoneSource` and `SpeakerSink` are the audio built-ins, beside
  camera and display: native built-ins in the engine tree, registered with the other
  media built-ins and surfaced to Python as marker classes beside `CameraSource`,
  configured the one way a built-in is configured
  (`rt.add(MicrophoneSource, config={"device_id": "..."})`). Both are `execution =
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
  exists to fix.
  [audio-subsystem; dlopen-audio-backend-and-audio-blocks — SHIPPED #1989, #1992 for
  the two built-ins, their execution mode and the drop-at-the-edge clause;
  audio-port-window-contract — SHIPPED #2034 for the `match_device` clause; conditioning
  and immediate cancel are a later rung]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_microphone_source.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_speaker_sink.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_speaker_sink.py::test_a_microphone_wired_to_a_speaker_runs_and_plays_what_it_captured -->
  <!-- verify: cargo test -p streamlib-media-builtins --test speaker_sink_matches_its_device a_sixteen_kilohertz_source_reaches_whatever_this_machines_speaker_opened_at -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-29-audio-port-window-contract.md -->
- **DECIDED** — Codec blocks are native built-ins beside camera, display and the audio
  pair: `H264Encoder`, `H264Decoder`, `H265Encoder`, `H265Decoder`, `JpegDecoder`,
  `OpusEncoder`, `OpusDecoder`, `Mp4Sink` — instantiated and configured the one way a
  built-in is configured (`rt.add(H264Encoder)`), per-frame paths never entering an
  interpreter, serving Python and Rust apps alike. Video blocks are built on the
  engine's existing Vulkan Video machinery reached through `GpuContext`'s session
  surface; JPEG decode is its own backend (`sdk/vulkan-jpeg`; the nvJPEG backend stays
  parked). AV1 and VP9 remain ported but unexposed until a consumer demands them.
  Encoder sessions mint lazily from the first frame's dimensions; decoder sessions
  auto-size the DPB from the stream's parameter sets. Config shape, rate-control and
  GOP knobs are ticket-level, like every other built-in's config. The four video blocks
  are one encode body and one decode body specialised by a codec identity, not four
  processors — the pair differs in an enumerant, the bag's `codec` string and a name —
  and each built-in is its own port surface, registration and identity. The layering
  wall holds at this fourth device class too: colour conversion into the codec's NV12
  input rides the engine's existing `rgb_to_nv12` converter stage, and no new RHI
  primitive was built for codecs. [codec-blocks — SHIPPED #2083, #2084, #2086 for the
  four video blocks, engine half; python-codec-block-api — SHIPPED #2105 for their
  Python surface; opus-mp4-recording-rung — SHIPPED #2125, #2126 for the Opus pair and
  #2127, #2128 for `Mp4Sink`, which is a sink rather than a codec and holds no session.
  extension-model — the "native built-ins" clause is the record of these seven and not the
  rule for the next codec, which follows the built-in criterion in §Packages & extension
  model; `JpegDecoder` is frozen — neither built nor retired — until its drone consumer
  returns (owner, 2026-09-04)]
  <!-- verify: cargo test -p streamlib-media-builtins --test h264_decoder_completes_the_round_trip -->
  <!-- verify: cargo test -p streamlib-media-builtins --test h265_decoder_completes_the_round_trip -->
  <!-- verify: cargo test -p streamlib-media-builtins --test h264_encoder_publishes_the_bag_convention -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib encoded_packet_to_audio_block_decoder::tests::a_tone_survives_the_round_trip_at_one_two_and_six_channels -->
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_fragmented_file_writer::tests::the_file_opens_with_the_brands_and_one_trak_per_link_named_after_its_producer -->
- **DECIDED** — An encoded frame is an ordinary bag: the bitstream rides inline as a
  msgpack `bin` field beside the producer-written stream metadata the delivery-profile
  decision already specified (sync-point flag, group index, sequence). No pooled-buffer
  or surface-id carriage for encoded bytes unless a measured need appears. The keys are
  the wire contract, the way `AudioBlock`'s six are: `codec` (`"h264"` / `"h265"`, the
  elementary-stream identity), `bitstream` (msgpack `bin`, one Annex-B access unit),
  `is_sync_point`, and `group_index` with `sequence_index` (the producer-scoped ordering
  pair, which over MoQ rides the object payload under `streamlib_bag` only — see
  §Networking),
  `width` and `height` (the coded extent, before crop), and `color` (the H.273 tuple).
  Timestamp rides the frame header like every bag. A bag the decoder cannot read is
  refused by name, never reshaped — the audio wire codec's doctrine — and the ordering
  pair is an encoded-frame key that never reaches a decoded bag: a decoded frame is an
  ordinary `VideoFrame`, so nothing downstream of the decoder joins on it. The
  ring-overwrite loss §Processor model leaves OPEN is stream-corrupting for an encoded
  link until the next sync point; the discard-to-sync-point doctrine makes it
  survivable — a reader enters a stream only at a sync point and discards back to one
  after a `sequence_index` step other than exactly one, counting what it lost — and
  that OPEN stays its own decision, named here so a codec ship is never read as having
  resolved it. [codec-blocks — SHIPPED #2083 for the convention and the sync-point
  gate; #2085 for the ordering pair's reach, which the proposal had assumed joined the
  decoded side]
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
  [codec-blocks — SHIPPED #2086; an engine-layer gap the H.265 arm surfaced, fixed at
  the engine layer for both codecs]
  <!-- verify: cargo test -p streamlib-engine --lib vulkan::video::decode::decoded_picture_display_window -->
  <!-- verify: cargo test -p streamlib-media-builtins --test h265_decoder_completes_the_round_trip -->
- **DECIDED** — Proof precedes surface: the first codec work re-proves
  camera → encode → decode → display at HEAD through the engine-owned PSNR rig
  (`runtime/streamlib-engine/tests/fixtures/`, Y ≥ 35 dB floor), adjudicating the
  #1077 decode regression — and closing out the #756/#335 real-hardware races — before
  any block API lands. The rig rebuilds on the control plane's own observation
  surface — tap the encoded and decoded channels, exchange surface ids for exact
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
  that mode exists to catch. [codec-blocks — SHIPPED #2084 for the rig, #2085 for the
  scoring rebuild, #2085 and #2086 for the two codecs' rig proofs at the floor]
  <!-- verify: cargo build -p streamlib-engine --example codec_roundtrip_rig -->
  <!-- verify: bash runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh -->
- **DECIDED** — The gate scores chroma, not luma alone: `cargo xtask psnr` classifies a
  frame Y ≥ 35 dB pass / 30–35 warn / < 30 fail **and** fails it outright when either
  chroma plane falls under 30 dB — one floor for every reference, no chroma warn band.
  The chroma floor is derived, not chosen: six cold rig runs (three per codec, 108
  samples) put the lowest finite clean chroma figure at `complex_pattern` 32.23 dB,
  reproducing to 0.02 dB run-to-run and 0.13 dB across codecs. That derivation ran
  against a decode path that reconstructed chroma half a luma sample off the siting its
  own bitstream implies; #2100 corrected the siting and the figure rose to 33.52 dB
  (H.264) / 33.42 dB (H.265) — three cold runs per codec of that reference, identical to
  0.00 dB run-to-run, and 0.10 dB across codecs. One whole-set run per codec confirms
  `complex_pattern` still carries the minimum, the next finite chroma reading in the set
  being 48.13 dB. The floor stays 30 dB: the correction widens its margin rather than
  moving it. A fourth injection mode
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
  CI-run; ffmpeg leaves the scoring path entirely. [codec-blocks — SHIPPED #2085 for
  `xtask psnr` and the tap/exchange scoring, #2094 for the chroma floor and
  `swap-chroma`]
  <!-- verify: cargo test -p xtask psnr -->
  <!-- verify: cargo test -p xtask codec_proof_image_measurement -->
- **DECIDED** — The vivid drift lock is per codec, not per rig: the H.265 arm locks
  against `psnr_vivid_baseline_h265.tsv` and H.264 keeps the unsuffixed file it was
  captured under, tolerance ±0.05 and comparison semantics untouched. The numbers could
  not carry over even though the mechanism did — the old baseline was sampled off the
  display's composited output, which is precisely what the rebuilt rig removed from the
  measurement path, so it failed against the new one outright (|0.9792 − 0.9180| =
  0.0612). Re-measured over exact decoded pixels the lock gained headroom: the
  bt601/bt709 green rise now reads 0.0965 off a 0.0029 floor instead of a 0.0575 one.
  Measured, the two codecs agree to 0.0001 on every channel, so one shared file would
  have passed both arms here; the split is headroom for a codec that does reconstruct a
  saturated primary differently, so that it cannot be read as a colour regression.
  [codec-blocks — SHIPPED #2085, #2086]
  <!-- verify: bash runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh -->
- **DECIDED** — The three recorded codec defects are adjudicated and closed on the
  rebuilt rig's evidence, which is what the proof-precedes-surface bar was for. #1077
  (decode unverified at HEAD) closed **obsolete**: the decoder enters at
  `sequence_index=0` with zero frames discarded, and the wire its first hypothesis was
  recorded against no longer exists — `ordered` at depth 16, with parameter sets on
  every IDR rather than only the first, so even a genuinely late subscriber re-enters
  within one GOP. Two latent silent paths in the encoder's header route were hardened
  anyway (a failed header extraction no longer degrades to an empty header, and the
  minted header is checked with the engine's own NAL reader), neither being the named
  cause. #756 (Cam Link `DEVICE_LOST`) closed on 18 clean runs across release and debug,
  1080p60 and 4K30. #335 (H.265 shutdown race) closed **not reproducible**: the pre-RHI-
  coupling teardown that produced it does not exist any more, and the decoder-lag load
  condition that triggered it is independently gone — the decode path's 21–25 fps cap
  became 3.75 ms/frame. If sustained decoder lag ever reappears the scenario is worth
  re-running rather than assumed fixed, and the rig is the runnable.
  [codec-blocks — SHIPPED #2084 closing #1077, #2085 closing #756, #2086 closing #335]
  <!-- verify: cargo test -p streamlib-media-builtins --test h264_decoder_completes_the_round_trip -->
- **DECIDED** — The four video blocks reach Python as marker classes beside
  `CameraSource`, through the five touchpoints a native built-in owns and no sixth — a
  processor extension owns none of them, being an ordinary Python processor class the
  wheel never has to know about (extension-model) — : a
  constructor-less `#[pyclass]` unit struct, an `is()` arm resolving the type to the
  processor's own minted import path, an `add_class` line, a re-export with its
  `__all__` entry, and a stub entry gated by stubtest with no allowlist. Configured the
  one way a built-in is configured — `rt.add(H265Encoder)`,
  `rt.add(H264Encoder, config={"keyframe_interval_seconds": 2})` — and Linux-only at
  the marker, the unsupported-platform arm raising by name rather than resolving a path
  the codec modules do not build there. No engine registration was added: the wheel
  already linked all four and already registered them at import, which is what makes
  this rung five touchpoints and no engine change. The stub docstring is where a
  block's config keys and port names are written down, as it is for every built-in, and
  it states the engine's own behavior rather than an aspiration — the encoder's
  `width`/`height` guardrails that a mismatching frame wins against with a warning, its
  lazy session mint from the first frame, the decoder's eager mint at `setup()`, and
  the `max_width`/`max_height` pair that warns and auto-detects from the first SPS when
  half-specified. What a Python app may wire follows from the engine half and is stated
  so the docstrings can say it: the encoder's `video` input takes any published
  `VideoFrame`, buffer-backed or texture-backed, while the decoder's `video` output is
  an ordinary `VideoFrame` on a pooled RGBA pixel-buffer surface — so a decoded frame
  reaches a Python kernel through a DLPack landing copy and never by bare surface id,
  which is the camera's existing gap carried, not a new one.
  [python-codec-block-api — SHIPPED #2105]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_video_codec_blocks.py::test_the_marker_class_cannot_be_instantiated -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_video_codec_blocks.py::test_the_round_trip_wires_without_an_adapter -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_video_codec_blocks.py::test_display_name_defaults_to_the_type_name -->
- **DECIDED** — `streamlib.EncodedVideoFrame` is the Python cast over the encoded-frame
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
  instant. [python-codec-block-api — SHIPPED #2106; the colour refusal, in both casts,
  #2114]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_encoded_video_frame_cast.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_encoded_video_frame_cast.py::test_an_encoded_video_frame_takes_no_surface_and_holds_no_claim -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_encoded_video_frame_cast.py::test_the_bitstream_crosses_the_wire_as_bytes -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_encoded_video_frame_cast.py::test_the_cast_offers_no_way_back_onto_the_wire -->
- **DECIDED** — The per-block ship bar the entry above states is met for a Python
  surface by agreement, never by a second rig: below the marker the path is
  byte-identical to the one the engine-owned rig scored, so the live proof is a
  Python-authored round trip locking to the same per-codec vivid baseline within the
  same ±0.05. `e2e_fixture_psnr_vivid.sh` carries a `PIPELINE=python` arm (default
  `rust`) differing in its launch argv alone — one timeout, one environment, one
  redirect, and the same tap, `exchange`, scoring and comparison after launch — over an
  engine-owned fixture app of four `rt.add` calls beside `audio_loopback_node.py`,
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
- **DECIDED** — `streamlib.EncodedAudioPacket` is the Python cast, pure Python beside
  `encoded_video_frame.py`, read with `into=EncodedAudioPacket`, every rule of the video
  cast verbatim: the wire keys are the constructor keywords, `bool` is refused where an
  integer is required, unknown keys are read past, the payload is stored under the Rust
  struct's own field name and stays off the repr, and there is no to-bag helper and no
  numpy property. [opus-mp4-recording-rung — SHIPPED #2126]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_encoded_audio_packet_cast.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_encoded_audio_packet_cast.py::test_an_encoded_audio_packet_takes_no_surface_and_holds_no_claim -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_encoded_audio_packet_cast.py::test_the_cast_offers_no_way_back_onto_the_wire -->
- **DECIDED** — `OpusEncoder` is `execution = reactive`, `scheduling = high` like the
  video blocks, input `audio` declaring `delivery_profile = "ordered"` and
  `audio_window(sample_rate = 48_000, dtype = "f32", window_size = 960, hop = 960)` — no
  channel count — so the engine resamples and frames, and `process()` receives one 20 ms
  Opus frame per dispatch in the source's own channels. Framing is the window contract's
  job, never the encoder's, which is why the held `packages/opus` had nothing to carry:
  it refused anything but 48 kHz stereo `f32` in 960-sample frames and told the author to
  add a rechunker. The encoder mints from the first block's `channels`, the video
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
  portability gate stays the pass/fail, naming the same five host libraries with Opus in
  as without it. [codec-blocks; opus-mp4-recording-rung — SHIPPED #2125]
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
  stream and MP4, CMAF, MoQ and WebRTC all model a stream as a track, so the fixed
  video-plus-audio pair every held consumer had is not the shape — and a caption or data
  track then needs only a bag convention, not a sink change. The track's kind is the bag's
  `codec`: `h264`/`h265` a video track, `opus` an audio track, anything else refused by
  name. At `setup()` the sink enumerates its inbound links, refusing by name when there
  are none; it opens `path` (required, created or truncated) and refuses by name a path it
  cannot open, the named-device shape. Truncating is the call: an app is re-run from the
  same `app.py`, wall-clock file naming would be a fourth surface the clock entry bans, and
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
  from the engine's own parser, never a second one for the same bytes. Parameter-set NALs
  are stripped from samples — ISO/IEC 14496-15 forbids in-band sets under `avc1`/`hvc1`,
  and `hvc1` is what Apple hardware plays, which retires the ffmpeg re-tag `/verify-video`
  used to shell to. Every remaining NAL is 4-byte length-prefixed, the walk reusing the
  engine's byte-stream parser rather than a fourth splitter; a sync-point bag is a sync
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
  logs an `Err` from `process()` and carries on. [opus-mp4-recording-rung — SHIPPED #2127]
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
- **DECIDED** — `OpusEncoder`, `OpusDecoder` and `Mp4Sink` reach Python through the five
  touchpoints a native built-in owns and no sixth, and no Linux split — nothing here is
  platform-bound, so they register unconditionally beside the audio built-ins. The stub
  docstrings state the engine's own behavior rather than an aspiration: the encoder's
  window and first-block mint, its two config keys, the decoder's entry and gap rule, the
  sink's track-per-link rule, its `moov` wait, fragment rule and truncate-at-setup.
  [opus-mp4-recording-rung — SHIPPED #2126, #2128]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_opus_blocks.py::test_the_round_trip_wires_without_an_adapter -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_mp4_sink.py::test_two_encoders_wire_into_the_one_input_without_an_adapter -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_mp4_sink.py::test_the_marker_class_cannot_be_instantiated -->
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
  <!-- verify: cargo test -p streamlib-media-builtins --lib mp4_annex_b_access_unit -->
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
- **DECIDED** — The held codec consumers resolve through this align, per §Consumers:
  `packages/{h264,h265,jpeg,opus,mp4}` are mined for their logic (session wiring,
  H.273 ↔ VUI translation, Opus framing) and each deletes in the change that ships its
  block. `examples/vulkan-video-roundtrip`, `examples/vulkan-video-psnr` and
  `examples/jpeg-psnr` delete into the engine-owned proof rig — their job becomes the
  rig's job, and a test owns its fixtures. `examples/h264-opus-validator` deletes
  outright. `examples/camera-audio-recorder` is conversion backlog: the recording
  showcase (camera + microphone → codec blocks → `Mp4Sink`) once the blocks exist.
  [codec-blocks — SHIPPED #2087 for the video half: `packages/h264` and `packages/h265`
  mined and deleted, both vulkan-video examples deleted into the rig; the H.265 VUI
  needed no mining of its own, its translation file being byte-identical to H.264's and
  already mined codec-agnostic. The lesson the sweep paid for: a path-literal search
  proves nothing about a prose citation spelling a package without its directory — the
  residue it missed named the same tree as a bare `h264`.
  opus-mp4-recording-rung — SHIPPED #2129 for the audio and container half:
  `packages/opus` and `packages/mp4` mined and deleted, `examples/h264-opus-validator`
  deleted outright, and `camera-audio-recorder` converted to the showcase. Neither
  mining carried code — the held encoder pushed framing onto an upstream rechunker the
  window contract now does, and `packages/mp4` held no muxer at all, only an ffmpeg
  subprocess and an every-method-TODO Apple tree; the one rule taken is the session
  epoch. `packages/jpeg` and `examples/jpeg-psnr` are the last two held on their own
  rung]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-09-01-codec-roundtrip-reproof.md -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-09-03-opus-mp4-recording-rung.md -->
- **OPEN** — Audio plugins (CLAP / VST3 / LV2): intended, do not build until a
  concrete consumer demands a specific plugin. Direction: CLAP first; the plugin runs
  out-of-process in its own helper over the engine's IPC transport, never in the app
  process; a plugin an app uses is declared project-locally — shipped in or referenced
  by the app's own project, the shader precedent — never discovered from
  machine-global scan paths; the lane costs nothing when unused (no `DT_NEEDED`
  entries, no import-time work). [audio-subsystem]

## Networking — transport, runtime mesh, moq, webrtc — IN-FLIGHT (→ cross-runtime-links, macos-platform-floor)

- **DECIDED** — Cross-language interop happens on the wire between nodes, as
  self-describing bags — never in-graph. [importable-python-library — SHIPPED #1715]
- **DECIDED** — Networking is the next work and the first extension: WebRTC and MoQ ship
  as extension wheels under §Packages & extension model rather than as built-ins — not
  every app needs them, and a capability with a consumer is what proves the extension
  model. The scope is those two, and both are moves of code the tree already holds:
  `runtime/streamlib-moq` (sessions and catalog on `moq-transport`) leaves the runtime
  workspace into the MoQ extension wheel, and the held `packages/{moq,webrtc}` are mined
  into the two wheels' Rust with their processors rewritten as ordinary processor
  extensions. The one expected exception to "leaves the runtime" is a runtime capability
  the moved code turns out to need, which is exposed as engine code — a split of concerns,
  expected to be rare. Zenoh is new work rather than a move and is its own later change;
  the runtime mesh is decided below. MoQ and WebRTC are edge source/sink processors
  ingesting and egressing external streams at a runtime boundary; they are not the
  runtime mesh. The held consumers `packages/{moq,webrtc}` and
  `examples/{moq-roundtrip,webrtc-cloudflare-stream,whep-player}` resolved through this
  change and are gone — mined, replaced or deleted per §Consumers.
  [extension-model; networking-extension-wheels — SHIPPED #2153]
- **DECIDED** — Both wheels sit on the encoded side of the codec blocks and touch no
  raw frame, surface or GPU: `WhipPublisher` and `MoqBroadcastPublisher` consume
  `EncodedVideoFrame` and `EncodedAudioPacket` bags downstream of `H264Encoder` and
  `OpusEncoder`; `WhepPlayer` and `MoqBroadcastSubscriber` emit the same bags upstream of
  `H264Decoder` and `OpusDecoder`. Audio is in scope for both from the first rung. The
  four are ordinary processor extensions — `@processor` classes in the wheel calling the
  wheel's own Rust — each in its own helper, on the tokio runtime the wheel's support
  hook brought up. The MoQ pair was named `MoqPublishTrack` / `MoqSubscribeTrack` here
  until the build: under the `Mp4Sink` shape a publisher carries a broadcast of many
  tracks, so a name saying one track fails the zero-context test.
  [extension-model; renamed at networking-extension-wheels — SHIPPED #2151]
- **DECIDED** — The ordering pair never rides the transport's own identifiers. Both are
  reachable — a subscriber can read `SubgroupReader::group_id` and
  `SubgroupObjectReader::object_id` — but neither can carry the producer's: the publisher
  must open groups with `append`, whose id is the library's own monotonic counter rather
  than the bag's `group_index`, and the received object id is a per-subgroup local counter
  with the wire value discarded before an application sees it. Under `streamlib_bag` the
  pair therefore travels inside the object as ordinary bag keys and the subscriber writes
  it back unchanged; a CMAF fragment carries neither and the subscriber mints its own, so
  downstream of a CMAF hop a gapless `sequence_index` is not evidence of a lossless stream
  and cross-track alignment is not recoverable from it. The pair and the stamp are
  producer-scoped, not end-to-end. A group is cut by a **video** sync point and the cut
  applies to every track at once; the cut is keyed on the video track alone, never on
  whichever bag happens to carry `is_sync_point` — every Opus packet carries it, and the
  library retains only a track's newest subgroup, so cutting on audio would leave one
  packet per group and lose all but the newest. A data track has no producer pair — the
  engine mints none for an arbitrary bag — so the publisher mints one `sequence_index` per
  data track, monotonic for that track's life, carried in the envelope and never written
  into the bag; and a data bag never cuts a group, for audio's reason. A broadcast with no
  video never reaches the video cut and rides two backstops instead:
  `HIGHEST_OBJECTS_IN_ONE_GROUP = 128` objects in one group, and an open group older than
  one second on the publisher's own monotonic clock, cut on the next bag's arrival rather
  than by a timer. A broadcast with video reaches neither. What a late joiner replays is
  therefore the open group — MoQ's behavior and media's, accepted rather than masked,
  bounded to about a second of production while the publisher is writing (owner,
  2026-09-05). The bound is what the age backstop can give without a timer, and it is worth
  stating exactly: a publisher that stops writing holds its last group open until it writes
  again or closes, so a joiner arriving during an idle stretch still replays that group
  however old the idle has made it. An idle close would need a timer, which no processor
  here owns; a downstream that wants only the live edge filters on the stamp it already
  receives.
  [extension-model; the data pair and the age backstop at moq-data-tracks — SHIPPED #2172]
- **DECIDED** — Many tracks follow the `Mp4Sink` shape: a publisher takes one track per
  inbound link and derives its catalog or session media description from them. The
  container names the tracks: under `cmaf` they are `.catalog`, an init track `0.mp4`
  carrying `ftyp` and `moov`, and `{track_id}.m4s` media tracks, because a subscriber not
  asked to fetch a catalog hardcodes exactly those; under `streamlib_bag` each is its
  link's channel name, unless the publisher was given `track_names` — positional in wiring
  order. Both subscribers expose one output per track kind — `encoded_video`,
  `encoded_audio`, and on `MoqBroadcastSubscriber` a third, `data_bags` — never one port per
  track: ports are declared statically
  by decorator, and a decoder downstream wants a port it can name at wiring time. Which
  track feeds which port is config only where the transport cannot say: `MoqBroadcastSubscriber`
  takes `video_track`, `audio_track` and `data_track`, because a MoQ broadcast may carry any
  number of tracks and a subscriber picks, and one naming none of the three is refused;
  `WhepPlayer` takes neither media name, because a WHEP answer names
  the session's media and there is nothing left to choose. Endpoint and credential
  configuration is ticket-level, as for every built-in's config.
  [extension-model; port shape narrowed and the player's track config corrected at
  networking-extension-wheels — SHIPPED #2150, #2151; the data port and name at
  moq-data-tracks — SHIPPED #2172, #2173]
- **DECIDED** — The control plane keeps nothing from the move. Its one use of
  `runtime/streamlib-moq` — a `/api/moq/catalog` route behind a `moq` feature no crate
  enables — read a process-global session registry that, with the publisher in a helper,
  the app process could never see; the route and the feature delete with the crate, and
  a broadcast's catalog is the MoQ wheel's to serve. The "rare exception" did not fire:
  the runtime needed nothing from the moved code that survives the move.
  `runtime/streamlib-moq` leaves the workspace whole once its logic is in the wheel, its
  `deny.toml` entry with it; whether the wheel's Rust is also published as a crate for a
  Rust app waits for a Rust app that wants it. More generally, the coupling was the
  mistake and not the route: the control plane carries no optional capability's routes
  natively, and an extension that needs an endpoint contributes it through `host`, served
  by the one control plane in the app process and seeing only what the app process sees
  (§Control plane & observability). The move owes no catalog route; the first consumer
  that wants one adds it through that door. [extension-model]
- **DECIDED** — The moved processors are typed, and the Python surface gains no raw byte
  port for them: a publisher reads `EncodedVideoFrame` / `EncodedAudioPacket` and hands
  the bitstream to the wheel's Rust; a player or subscriber writes the bag literal
  against the wire contract, filling every required key from the stream itself — the
  extent from the SPS, the ordering pair from its own counters (for MoQ under
  `streamlib_bag`, from the object payload; under `cmaf` minted by the subscriber), a
  sync point from the access unit, the audio parameters from
  the session description — rather than from config. The old processors' opaque envelope
  forwarding does not carry over: what crosses a network on a **media** track is a
  bitstream and the keys a decoder needs, not a serialised link payload. That clause
  scopes to media, which is what it was deciding — on a data track the object *is* the
  bag, whole and nested, because for data the bag is the payload. It is the opposite of
  opaque forwarding either way: the old processors restamped on receive, and a data track
  carries the user's own keys byte-exact under the producer's own stamp.
  [extension-model; narrowed to media at moq-data-tracks — SHIPPED #2172, #2173]
- **DECIDED** — What the move carries and what it leaves: `runtime/streamlib-moq`'s
  catalog shape is mined, not moved, and its session logic was rewritten for draft-16;
  from `packages/webrtc` the RFC 6184
  depacketiser and the WHIP/WHEP signalling logic are mined, and its dead second RTP path
  (`streaming/session.rs`, constructed nowhere) is not; `packages/{moq,webrtc}` have not
  built since the plugin SDK was deleted and their bag types share no key with today's
  wire contract, so that half is a rewrite against mined logic — the conversion doctrine
  as usual. A received stream reaches the bag through the proven manual-source shape: the
  wheel's Rust receives on its own runtime and a processor-owned thread writes, so no
  engine seam is added for it. Two budgets the wheels live inside, ticket-level but named
  here: a helper stops on the shutdown ladder §Processor model states, whose `teardown()`
  budget is five seconds, so a WHIP `DELETE` or a QUIC close must fit inside it; and
  connecting inside `setup()` spends the sixty-second registration budget.
  [extension-model; the ladder — local-transport-hardening, SHIPPED #2264, #2266]
- **DECIDED** — The proof bar is the codec blocks' two halves. CI-run, GPU-free and
  endpoint-free: RTP packetising and depacketising round trips, SDP construction and
  parsing, MoQ catalog and object bytes, and the bag literal a player writes checked
  against the wire contract. Live, rig-only: WHIP publish to Cloudflare Stream and WHEP
  play back from it, and MoQ publish and subscribe through a Cloudflare relay —
  provisioned per account and authenticating, the token riding the CONNECT `:path`;
  there is no credential-free draft-16 relay, and the URL is a credential that must
  never reach a log or an error message — with credentials outside the tree, in the
  fixture-script shape the codec rig set (owner, 2026-09-04). The move re-verifies
  against current library versions rather than the pins the held code carried: `webrtc`,
  `moq-transport`, `quinn` and `rustls` have moved since the freeze, and the patches the
  old MoQ path carried for TLS and for newer draft versions may now be upstream — whoever
  moves it checks first. [extension-model]
- **DECIDED** — `packages/streamlib-webrtc/`: a standalone maturin project — own
  `Cargo.toml` (`[workspace]` root, `[lib] name = "_native"`, `crate-type = ["cdylib"]`,
  `pyo3` on `abi3-py310`, `webrtc 0.14`, `tokio`, `hyper` + `hyper-rustls`, `rustls`,
  `bytes`; no engine crate), own lockfile, `pyproject.toml` depending on `streamlib` by
  version, `python/streamlib_webrtc/` with `_native.pyi` and `py.typed`. `src/` carries the
  mined WHIP and WHEP clients, `h264_rtp.rs` with its tests, and `rtp.rs`'s sample
  conversion; `session.rs` and `RtpTimestampCalculator` were left dead and are not moved.
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
  brought up beside its publisher, so the player carries the same bounded backoff as
  `MoqBroadcastSubscriber` — a fresh session per attempt, since a closed peer connection
  cannot be dialled again. A bag the engine refuses is the one failure not retried: it names
  its port and ends the thread, because reconnecting would spend an endpoint's session
  forever on a bag refused every time. [networking-extension-wheels — SHIPPED #2150]
- **DECIDED** — `packages/streamlib-moq/`: the same standalone shape on `moq-transport`,
  `quinn`, `rustls` and `rustls-native-certs`, with its session and catalog **mined** from
  `runtime/streamlib-moq` rather than moved — the old document attributed tracks to
  processor import paths and has no relation to the draft-ietf-moq-catalogformat-01 JSON a
  player reads, and the process-global `RUNTIME_SESSIONS` registry and `sessions_for_runtime`
  did not come either, since one processor owns one session. The relay URL is config
  carrying the relay's auth token in its path and therefore has no default: a draft-16
  relay is provisioned per account, so no address this wheel could ship would reach one.
  `extension.py:load` brings up the runtime and registers `moq`. The version check
  §Networking asked for was made (2026-09-04): `moq-transport` 0.16.2, the draft-16
  revision, because Cloudflare deploys draft-16 and it carries the acknowledgement and
  namespace machinery draft-14 lacks — owner ruling, superseding the original draft-14
  default. Draft-16 requires authentication, so no credential-free public relay remains.
  [networking-extension-wheels — SHIPPED #2151]
- **DECIDED** — `MoqBroadcastPublisher`: `@processor`, one fan-in input `tracks`, one MoQ
  track per inbound link, the catalog derived from them; config `relay_url` (required),
  `broadcast` (default `streamlib/<runtime_id>`), `container_format`, `track_names` and
  `delivery_deadline_ms`.
  `MoqBroadcastSubscriber`: `@processor(execution = "manual")`, outputs `encoded_video`,
  `encoded_audio` and `data_bags`, config `relay_url`, `broadcast`, `video_track`,
  `audio_track`, `data_track`,
  `container_format`; the processor-owned thread writes each received object as a bag
  literal. Naming the MoQ group from the bag's `group_index` was the original design and did
  not survive contact: `SubgroupsWriter::create` hands back a live writer for a group id at
  or below the latest and then drops every object written to it with no error on either
  side — hence `append` and the library's own counter, and the ordering pair riding the
  object as §Networking's rule above already requires.
  [networking-extension-wheels — SHIPPED #2151]
- **DECIDED** — Two container formats, selected by `container_format` on each processor and
  declared per track in the catalog's own `packaging` field. `"cmaf"` is the default,
  because interop is the point: the broadcast is laid out as `moq-pub` lays one out — a
  `.catalog` track carrying draft-ietf-moq-catalogformat-01 JSON, an init track carrying
  `ftyp` + `moov`, media tracks whose objects are self-contained `moof` + `mdat` fragments —
  so `moq-js` and `moq-sub` can play it. `"streamlib_bag"` is the msgpack envelope, kept
  because CMAF is lossy against the bag contract: the ordering pair becomes container
  timing, `pre_skip` becomes the `dOps` box, colour goes into the VUI, and only the envelope
  can write the producer's pair back unchanged. The wheel builds CMAF on `mp4-atom`, the
  same crate the engine's own fMP4 writer is built on, carrying its own Annex-B conversion
  and sample entries. It is not a port of `Mp4FragmentedFileWriter`, whose growing file,
  shared `moov` and cross-track epoch are file-shaped and wrong here. Owner ruling,
  2026-09-04: MoQ had never been finished, and finished means interoperable.
  `streamlib_bag` is also the only container a data track
  rides, and the only one whose track names the app chooses: a `cmaf` broadcast refuses a
  data bag by name at its first bag, before any hold, and refuses `track_names` and
  `data_track` at `setup()`. A mixed broadcast — CMAF media a `moq-js` reads beside a bag
  data track — needs per-track `packaging` in the catalog and its own `moq-sub` check, and
  is named here as a later rung rather than built.
  [networking-extension-wheels — SHIPPED #2151; the data container at moq-data-tracks —
  SHIPPED #2172, #2173]
- **DECIDED** — `python-wheel.yml` carries an `extension-wheels` job over a matrix of the
  two directories: install the just-built `streamlib` wheel into the venv, `maturin develop`
  the extension, `cargo test` its crate, `mypy.stubtest` over its `_native`, pyright over
  its Python, pytest with `-m "not requires_gpu"`, and the portability gate over its `.so`.
  `release-please-config.json` carries a package entry per wheel (independent versions and
  tags); the release workflow builds and attaches each wheel on its own tag;
  `build_simple_index.py` is multi-project — a set of published names, one PEP 503 directory
  each — with its tests. [networking-extension-wheels — SHIPPED #2152]
- **DECIDED** — The proof, as built. CI-run, GPU-free, endpoint-free, owned by each wheel:
  the RFC 6184 packetise/depacketise round trip (the carried tests plus STAP-A and FU-A
  cases), SDP offer construction and answer parsing, MoQ catalog and object encoding, and
  each player's bag literal checked against the wire contract on the `wired_link` fixture
  pattern. Live, rig-only, under `/verify-live` with a networking arm: WHIP publish of the
  vivid camera and the known signal to Cloudflare Stream and WHEP play-back of the same
  stream, and MoQ publish and subscribe through a Cloudflare draft-16 relay — credentials
  read from the environment, absent ones reported as cannot-run, never as pass. **The CMAF
  arm's interop proof is a live third-party read, not an in-repo fixture comparison**
  (owner, 2026-09-05): `moq-sub`, built from `cloudflare/moq-rs`, subscribes to the same
  broadcast and must parse the catalog, accept the init segment and decode the media. That
  is stronger than matching a captured reference — it is the reference client reading the
  real broadcast — and weaker in one way worth stating: it is rig-only, so CI protects the
  container's shape through `mp4-atom` round trips and the `moq-catalog` parse oracle alone.
  The decode-back is the lock: `WhepPlayer` / `MoqBroadcastSubscriber` → `H264Decoder` → tap
  and exchange → `xtask psnr channel-means` against the per-codec vivid baseline within
  ±0.05 — the network sits inside a path the codec rig already scored, so a mismatch is the
  wheel's. [networking-extension-wheels — SHIPPED #2153]
  <!-- verify: git ls-files packages/streamlib-moq packages/streamlib-webrtc -->
- **DECIDED** — A media bag past its deadline is shed rather than delivered late.
  `MoqBroadcastPublisher` takes `delivery_deadline_ms`; a media bag older than it by its
  own monotonic stamp is never written, and the rest of its group is shed with it, ending
  at the next sync point. A sync point is always written, so a shed never outlives one
  group; every Opus packet is a sync point, so audio is never shed; a data object is never
  shed. `MEDIA_TRACK_PRIORITY` split into `AUDIO_MEDIA_TRACK_PRIORITY = 126` and
  `VIDEO_MEDIA_TRACK_PRIORITY = 127`, read in draft-16 §10.4.2's direction and never
  checked against a relay, which is free to ignore it; a data track rides video's rung as a
  stated placeholder until a consumer asks otherwise. Absent, the publisher is the shipped
  baseline and every bag is written however late. Both of the deadline's readings are
  subtractions against the publisher's own machine's clock, so the stamp-age arm reads only a
  track whose stamps are taken on it: a track fed across the mesh has no age here at any
  offset, is never shed for one, and is said once in the log naming both machines, while its
  uplink backlog is still read from the instant each object reached the transport. The reach of
  that is a mesh link and no further: a stamp a relay restated on a local link reads as local
  here too, which the clock entry below leaves to the common-clock OPEN, and
  `MoqBroadcastSubscriber` feeding `MoqBroadcastPublisher` is that case inside this wheel.
  [moq-data-tracks — SHIPPED #2159; the cross-clock arm — cross-runtime-links, SHIPPED #2340]
- **DECIDED** — The deadline alone cannot see the uplink, because `moq-transport` never
  blocks and never pre-empts: a bag hands off to a forwarder and the writer learns nothing
  of the backlog behind it. So the wheel vendors `moq-transport` 0.16.2 at
  `packages/streamlib-moq/vendor/moq-transport` as a path dependency on the vulkanalia
  precedent — MIT OR Apache-2.0 under its own headers, pinned by
  `cargo xtask check-vendored-trees`, its patches recorded in
  `docs/architecture/vendored-moq-transport.md` — for exactly two of them:
  `SubgroupWriter::abandon` honoured ahead of already-buffered objects and raced by the
  forwarder against every chunk write, resetting the stream with a draft-16 code
  (`DeliveryTimeout` reachable at last); and the forwarder's cursor in shared state, so a
  writer can read how many of its objects have not left the process. On those, the deadline
  fires on the oldest unforwarded object's stamp as well as on the arriving bag's own, and
  a cut abandons the superseded group of every media track whose backlog is past the
  deadline — video and audio, never data, never the open group. The QUIC send window is
  bounded to 512 KiB so the cursor can fall behind at all rather than the stack swallowing
  the backlog silently, and the shed and the abandon are counted per link and said with the
  QUIC path's readings. Vendored rather than patched from git because a path dependency
  reaches the manylinux release build and the draft-16 line is frozen upstream; the patches
  stay ours and are never sent upstream (owner, 2026-09-06).
  [moq-data-tracks — SHIPPED #2179, #2180]
  <!-- verify: cargo xtask check-vendored-trees -->
- **DECIDED** — A MoQ broadcast carries data tracks beside video and audio, under
  `streamlib_bag` only. `MoqBroadcastPublisher` classifies each inbound link by its first
  bag: a bag carrying a `bitstream` key is encoded media and takes the typed path
  unchanged; a bag without one is data, and that link is a data track for the publisher's
  life — a later media bag on it is refused by name, as a codec change on a media link
  already is. `bitstream` is the encoded wire contract's defining key, which both media bag
  types require, so a user data bag that happens to name one is refused as media with a
  message naming the key and the user renames it. The publisher mints a per-track monotonic
  `sequence_index`, builds the object in Python as `{"sequence_index": int,
  "timestamp_ns": int, "bag": <the bag>}` with the user's map nested whole under `bag`
  rather than flattened — nesting
  reserves no name in the user's namespace where flattening would reserve four — encodes it
  with `streamlib.encode_bag_to_msgpack_bytes` and hands the bytes to
  `_native.MoqBroadcastPublishingSession.publish_data_object`, whose Rust writes them as
  the object payload and handles no engine object. `MoqBroadcastSubscriber` reads a
  `ReceivedDataObject` (`track_name`, `payload`) on its `data_bags` output, decodes the
  envelope, refuses one missing any of the three keys by name, and writes `bag` verbatim
  with `timestamp_ns` as the stamp; `sequence_index` never enters the written bag, and a
  jump in it is counted and reported through the Python log at the progress cadence. One
  data track per subscriber — two data tracks are two subscribers, which keeps the bag
  verbatim where a demux key would be pollution. The catalog is unchanged: a data track's
  entry is the entry every `streamlib_bag` track already gets, `codec` the literal
  `streamlib-bag` with every media field empty, because the catalog is written at connect
  before any bag has said what it is. [moq-data-tracks — SHIPPED #2172, #2173]
- **DECIDED** — Track names under `streamlib_bag` are the app's to choose.
  `MoqBroadcastPublisher` takes `track_names`, positional in wiring order — the order
  `runtime.connect` ran, which is the order `cmaf` already numbers `{track_id}.m4s` by. A
  count unequal to the inbound links is refused by name at `setup()`, and so is a repeated
  name — `Tracks::create` overwrites a track of the same name without saying so, orphaning
  its subscribers, so the duplicate is caught before the session can. Absent, a track is
  its link's channel name as before. That name is `{processor_id}/{port}` on a cuid2 minted
  at `add`, which a subscriber in another node cannot know — so before `track_names` the
  only broadcast a second node could name was `cmaf`, and the live fixture ran `cmaf` for
  that reason and said so. It no longer needs to. [moq-data-tracks — SHIPPED #2172]
- **DECIDED** — The wheel's oversize warning is charged against what the link charges: the
  **framed** encoded bag, header included, against the helper-link ceiling — not
  `len(bitstream)`, which under-reported by the bag's other keys and by the frame header.
  The media path's guard is corrected to the same measure. The exact measure is an encode,
  so it is taken only once a cheap prefilter says the bag is near enough to the ceiling for
  the answer to be in doubt. The ceiling itself stays the engine's and unexported; the
  wheel's copy stays a warning that lands at the wrong size on drift, never a failing test.
  [moq-data-tracks — SHIPPED #2172]
- **DECIDED** — The data track's proof. CI-run, GPU-free, endpoint-free, owned by the
  wheel: the envelope round trip on the `wired_link` fixture — a nested bag with a `bytes`
  value crosses the publisher's encode, the subscriber's decode and a real link, and
  arrives `==` with `bytes` still `bytes`; a `cmaf` broadcast refusing a data bag by name
  at its first bag with no hold entered; the `track_names` count mismatch and the `cmaf`
  refusals by name; a publisher classifying a bitstream-less bag as data and a later media
  bag on that link as a refusal; and a video-free publisher fed two data bags stamped more
  than the age backstop apart cutting a group between them, and one fed bags inside it not
  cutting. Live, rig-only, under `/verify-live`'s networking arm: the MoQ round-trip
  fixture runs both containers, and its `streamlib_bag` arm carries video, audio and a
  telemetry data track with `track_names` set through the Cloudflare draft-16 relay — the
  data bags received `==` and stamped as sent, the media decode-back locking PSNR as
  before. The `cmaf` arm and its `moq-sub` read are unchanged.
  [moq-data-tracks — SHIPPED #2174]
  <!-- verify: pytest packages/streamlib-moq/tests/test_data_track_round_trip.py -->
- **DECIDED** — Runtimes on different machines form a runtime mesh over Zenoh, and the mesh
  is engine transport: always on, beside iceoryx2, in the app process. It is not a processor,
  not a built-in, not an extension wheel, and never upstream iceoryx2's tunnel or gateway.
  Every runtime opens one Zenoh session when it starts and closes it inside the shutdown
  budget; helper processes never open one. A link carries the same way wherever its ends
  are: a link whose ends share a runtime rides iceoryx2, a link between runtimes rides Zenoh,
  chosen by the engine from the link's ends, and no processor can tell which. A runtime
  that cannot open its session runs local-only and says so once; the mesh never fails a
  runtime's start. Easy, automatic data exchange between runtimes is runtime capability.
  Five readings the build settled. **The session opens in `Runner::new()`**, after logging
  and before the iceoryx2 node, beside the runtime-id socket refusal — so a refused runtime
  builds no iceoryx2 node and no surface socket, and the duplicate refusal and discovery are
  both provable with no GPU. It costs `Runtime()` about 500 ms more while multicast discovery
  is on, which is Zenoh's own scouting delay: engine-chosen, not authorable, and paid again
  by `dev`'s warm restart. **The session is built from defaults, never from a Zenoh config
  file or a `ZENOH_*` variable** — peer mode, one listener on `udp/[::]:0?rel=1` (QUIC over
  UDP, a stream per priority) unless the runtime names others, multicast scouting on its
  default group, and no Zenoh `namespace` because the mesh name is a key prefix the engine
  writes itself. That is the engine-owned-domain precedent applied to a second transport.
  **Local-only means the open failed**, which in peer mode is a bind failure: the runtime
  warns once naming the reason, runs on, and never retries for its life. A runtime with
  discovery off and no peers is *isolated* rather than local-only, and `graph` says which one
  it is. **The session closes at the end of `stop()`**, token undeclared first so peers see
  the runtime leave at once; an engine the shutdown ladder leaks is closed by process exit,
  whose kernel-closed TCP peers see within milliseconds, and no Zenoh call ever runs on one of
  the engine's current-thread tokio runtimes. And **a helper opens no session**, because it
  never constructs a `Runner` and Zenoh's thread pool starts on first use.
  [runtime-mesh — SHIPPED #2283]
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::mesh::resolved_runtime_mesh_configuration::tests::the_zenoh_configuration_carries_peer_mode_and_exactly_these_endpoints -->
  <!-- verify: cargo test -p streamlib-engine --lib core::json_schema::capability_extension_and_mesh_rendering_tests::a_local_only_runtime_renders_the_reason_its_session_did_not_open -->
  <!-- verify: cargo test -p streamlib-engine --lib core::json_schema::capability_extension_and_mesh_rendering_tests::an_isolated_runtime_renders_an_open_session_with_no_peers_and_no_reason -->
  <!-- verify: cargo test -p streamlib-engine --test runtime_mesh_two_processes -->
- **DECIDED** — A runtime's name and mesh come from its constructor, its environment or the
  CLI, and nowhere else. Five optional values configure it — the runtime name, the mesh name,
  peer endpoints, listen endpoints and whether multicast discovery runs — reachable four ways:
  keyword-only on `Runtime()` and stub-gated; `Runner::new_with_runtime_mesh_configuration`
  in Rust, with `Runner::new()` taking the defaults; `STREAMLIB_RUNTIME_NAME`,
  `STREAMLIB_MESH_NAME`, `STREAMLIB_MESH_PEER_ENDPOINTS`, `STREAMLIB_MESH_LISTEN_ENDPOINTS`
  and `STREAMLIB_MESH_MULTICAST_DISCOVERY` for anything the constructor leaves unset, read in
  the engine so a Rust app and a container get it too; and `streamlib run` / `dev`'s
  `--runtime-name`, `--mesh-name`, `--mesh-peer`, `--mesh-listen` and
  `--no-mesh-multicast-discovery`. An endpoint is a Zenoh locator and a router is named the
  way a peer is; a malformed one — a transport this build lacks, or plain `udp/` — is refused
  at construction by name, the caller's wiring error as a wrong `device_id` is, while an
  endpoint that is merely unreachable never fails the runtime.
  `streamlib run` constructs `Runtime()` before `setup(rt)`, so a CLI-launched app is named
  on its command line or in its environment and never in `app.py` — and most apps need
  neither, the default being stable. Rejected: `setup(rt)` naming the runtime, which makes
  mesh configuration mutable state on a constructed runtime and moves both proofs behind the
  GPU in `start()`; and a `[tool.streamlib]` table in `pyproject.toml`, the first
  streamlib-specific file an app would author, which the zero-ceremony bar rules out. Owner,
  2026-09-14. [runtime-mesh — SHIPPED #2282, #2283]
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::mesh::resolved_runtime_mesh_configuration -->
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::runtime_name -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py::test_a_mesh_peer_this_build_cannot_dial_is_a_usage_error -->
- **DECIDED** — A runtime announces itself on the mesh and discovers other runtimes
  automatically: peer-to-peer discovery is on by default, and explicit peers or a Zenoh
  router serve networks that multicast discovery does not cross. The announcement is a
  liveliness token plus a description queryable, both under
  `streamlib/<mesh name>/@runtime/<runtime name>`. A token carries no payload, so the token
  *key* carries the only things a dead runtime must still answer for — its host identity and
  its pid — while the queryable answers a msgpack document holding `runtime_id`, `host_name`,
  `pid`, `engine_version` and `control_plane_urls`, read from the runtime's state at query
  time so a control plane hosted after construction still shows up. `control_plane_urls` is
  one `http://<address>:<port>` per non-loopback, non-link-local interface address the bind
  covers, an IPv6 literal bracketed, and empty with no control plane. The `@runtime` chunk is
  verbatim in Zenoh's grammar, so no `**` subscription over a mesh's port addresses ever
  matches it, and since a display name may not begin with `@` no address can collide with it.
  Discovery is a liveliness subscriber with history plus one description query per runtime
  that appears, and a runtime that leaves is removed; `graph` reads the peer table lock-free
  and never waits. Multicast discovery is proven in CI, not only on the rig: the two-process
  fixture runs a multicast arm beside its explicit-peer arms, with scouting pinned to
  loopback.
  [runtime-mesh — SHIPPED #2283]
  <!-- verify: cargo test -p streamlib-engine --test runtime_mesh_two_processes two_runtimes_discovering_by_multicast_each_list_the_other -->
  <!-- verify: cargo test -p streamlib-engine --lib core::json_schema::capability_extension_and_mesh_rendering_tests::a_peer_that_has_not_answered_still_deserializes_beside_one_that_has -->
- **DECIDED** — Everything a runtime puts on the mesh lives under a mesh name, `default`
  unless the runtime names another, so runtimes join everything reachable out of the box and
  groups sharing one network separate by naming different meshes. There is no switch that
  turns the mesh off; a runtime is isolated by a mesh name, explicit peers, or discovery
  turned off. Stated as the posture for now, not a permanent default. The mesh name is one
  chunk of the channel-name grammar, `[a-z][a-z0-9_-]*`. What separation by mesh name does
  and does not buy is worth stating exactly: runtimes in different meshes on one network may
  still connect at the transport and exchange nothing, and unrelated Zenoh traffic — ROS 2's
  `rmw_zenoh`, say — may connect the same way. Separation is of what is announced and read,
  never of what dials whom. [runtime-mesh — SHIPPED #2283]
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::mesh::runtime_mesh_name -->
  <!-- verify: cargo test -p streamlib-engine --test runtime_mesh_two_processes -->
- **DECIDED** — A port on the mesh is addressed `<runtime name>/<display name>/<port>`. The
  runtime name belongs to the runtime rather than to its control plane; defaults to
  `<hostname>-<app directory name>-<id>`, the id hashed from the directory's full path so two
  checkouts differ and reruns match; is never auto-suffixed; and is unique within a mesh: a
  runtime whose name is already live on the mesh refuses to start by name, except over one on
  the same host whose process is gone. The display name — already unique within a graph — is
  the processor's part of the address, so renaming re-addresses it; identity stays the class
  import path. Processor ids and cuid2 channel names never appear on the mesh.
  Three readings the build settled. **The grammar is one legal Zenoh key chunk** — non-empty,
  no `/`, `*`, `$`, `#` or `?`, not beginning with `@` — and otherwise free text like a
  display name, validated against the real key-expression rules; an explicit name that breaks
  it is refused at construction naming the character. **The default is
  `<hostname>-<app directory name>-<id>`** with every forbidden character replaced by `-`,
  the id four base-36 characters of an FNV-1a hash over the app directory's full path — the
  virtual camera's own recipe — resolved from `STREAMLIB_APP_DIRECTORY`, else the wheel's
  captured entry directory for a hand-run `python app.py`, else the working directory, which
  is what a Rust app gets. Where never-auto-suffixed bites is stated rather than discovered: a
  second run from one directory is refused until given `--runtime-name`, and moving the
  directory renames the runtime, as it relabels its unnamed virtual cameras. **The duplicate
  check is a query before the token is declared** — discovery is on, so `open` has already
  waited out the scouting delay, and the query then waits at most an engine-chosen bound for
  connected peers to answer. Any token refuses `Runtime()`, naming the runtime name and the
  holder's host and pid (both on the token key) and offering both fixes — stop it, or start
  under another name — unless the holder's host identity is this host's and its pid is gone.
  Host identity on Linux is the kernel boot id plus the pid-namespace inode, so a container on
  the same kernel is never mistaken for its host; macOS has no host identity and therefore no
  exception, so a duplicate there is refused until the old token leaves. `runtime_id` stays
  per-run — logs, the registry file, iceoryx2 names, the description — and is never an
  address.
  Stated residual: two runtimes that start inside one discovery window, or that meet when a
  partition heals, are not refused. Both keep running, each says so once naming the other's
  host, and `graph` lists both; which one a remote link reaches is `cross-runtime-links`'s to
  settle. [runtime-mesh — SHIPPED #2282, #2284]
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::runtime_name -->
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::mesh::duplicate_runtime_name_on_the_mesh -->
  <!-- verify: cargo test -p streamlib-engine --test runtime_mesh_two_processes -->
- **DECIDED** — A bag's top-level `surface_id` crosses the mesh transparently, for now: the
  sending runtime resolves it locally and sends the frame's pixels with what the receiver
  needs to rebuild them, and the receiving runtime writes the pixels into a freshly minted
  local surface and hands the bag on carrying that local `surface_id`. No surface id, lease,
  lifetime state or write-back crosses. The `surface_id` key is a stand-in the general
  mechanism replaces in a later release.
  As built: the message is `[pixel description][bag][pixel bytes]`, and the attachment carries
  the description's length — zero for a bag naming no surface, which therefore still crosses
  verbatim. The sender's copy door is `SurfaceExportStaging` at host-visible residency, held
  under a check-out claim that spans the copy alone, so a slow network never pins the
  producer's pool slot. The description's format, extent and byte length are read from the
  backing, never from the bag, which names no format at all. Each refusal is counted and said
  once per port or per source by its own name: a recycled frame, a multi-plane format, a pool
  at its cap, and a source offering more than the four format-and-extent pairs one may mint
  pools of (pools are never freed). The copy-out door is Linux-only, because
  `SurfaceExportStaging` is; a non-Linux sender says once per port that its surface bags do
  not cross. [runtime-mesh — SHIPPED #2290]
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::mesh::a_bags_top_level_surface_id -->
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::mesh::a_frames_pixels_on_the_mesh -->
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::mesh::a_frames_pixels_written_into_a_local_surface -->
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::mesh::a_frames_pixels_read_out_for_the_mesh -->
- **DECIDED** — The mesh carries no authentication or access control in this work; security
  is its own later pass, and the auth posture OPEN under §Control plane & observability owns
  it. What shipped holds the line: `zenoh` is built with `default-features = false` and
  exactly `transport_tcp` and `transport_udp`, so the QUIC-over-UDP link runs unencrypted on
  a self-signed key Zenoh makes itself — nothing to provision — and `transport_quic`, which
  needs a provisioned key and certificate, waits for that security pass.
  [runtime-mesh — SHIPPED #2283]
  <!-- verify: grep -n "transport_udp" runtime/streamlib-engine/Cargo.toml -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py::test_the_native_extension_links_nothing_the_host_may_not_supply -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_third_party_notices.py -->
- **DECIDED** — Any runtime on the mesh may create a remote link: a receiver pulling another
  runtime's output into its own input, a sender pushing its output into another runtime's
  input, or a third runtime wiring two others. The runtime that owns the input end applies the
  link through the same `connect` operation and the same refusals a local link meets, and the
  requesting runtime receives that outcome. The request travels over the mesh, so neither end
  needs a control plane, and `graph` on the input's runtime shows which runtime created the
  link. Until the security pass, any runtime on the mesh may wire. [runtime-mesh]
- **DECIDED** — A remote link naming a runtime that is not on the mesh waits and wires when
  that runtime appears; a runtime that is present but offers no such processor or port refuses
  the link by name, listing what it does offer; and a link whose remote runtime leaves returns
  to waiting and restarts its loss count when the runtime returns. A sending runtime does no
  network work and copies no frame for a port until a remote link to that port exists.
  [runtime-mesh]
- **DECIDED** — Stamps cross the mesh unchanged — the frame header's and every stamp in the
  bag — carrying the identity of the clock that produced them, and a stamp is never compared
  against one from another clock. The one monotonic clock of §Media I/O is therefore one
  clock per machine. [runtime-mesh]
- **DECIDED** — No link ever blocks a producer across the mesh either: a send the network
  cannot take is dropped rather than waited for, and every bag lost between two runtimes is
  counted on its remote link in `graph`, beside the drops its ports already count. Notify
  services, the runtime event bus, request-response and blackboard never cross the mesh.
  [runtime-mesh]
- **DECIDED** — `graph` carries the runtime's mesh peers, and `streamlib nodes` lists mesh
  peers beside the nodes in the local registry. A runtime that hosts no control plane still
  joins the mesh and carries remote links; it is not drivable remotely.
  `graph` gains a fourth top-level key, `mesh`, always present, holding `mesh_name`,
  `runtime_name`, `session` (`open` or `local_only`), `local_only_reason` only when the
  session did not open, and `peers` sorted by name. A peer carries `runtime_name` always —
  it is on the liveliness token — and `runtime_id`, `host_name`, `engine_version` and
  `control_plane_urls` each only once the description answers, so every one of the four is
  optional on the peer type and both shapes are covered by the key-list test, the strict
  fixture and the schema. No peer carries a last-seen time: a wall-clock one would be a
  fourth surface the clock entry bans, and a monotonic one means nothing to another machine.
  `streamlib nodes` puts `RUNTIME_NAME` first and `--node` takes a runtime name or a runtime
  id; below the registry table it prints a mesh-peers table of
  `RUNTIME_NAME HOST CONTROL_PLANE_URLS ENGINE_VERSION`, whose rows come from a short-lived
  observe-only session reached through a stub-gated `_engine` function and taking
  `--mesh-name`, `--mesh-peer` and `--no-mesh-multicast-discovery`. A peer that is already a
  registry row is not repeated, and the cost is about a second more per `nodes` call — the
  scouting delay plus the query bound. `nodes` stays a registry surface rather than a tool,
  so the CLI is still a pure JSON-RPC client for every tool there is.
  [runtime-mesh — SHIPPED #2283, #2285]
  <!-- verify: cargo test -p streamlib-engine --lib core::json_schema::capability_extension_and_mesh_rendering_tests -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py::test_a_runtime_on_the_mesh_is_listed_once_with_what_it_says_it_is -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py::test_an_empty_mesh_says_so_and_names_the_mesh -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py::test_a_verb_targets_a_node_by_its_runtime_name -->
- **OPEN** — A common clock across machines: intended, do not build until designed. Direction:
  runtimes on a mesh negotiate a shared network time (PTP, NTP or similar) so stamps from
  different machines become comparable; until then the per-clock rule above stands.
  [runtime-mesh]

## Language SDKs & parity — SHIPPED
<!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py -->

- **DECIDED** — Python is the sole focus runtime: the importable PyO3 wheel is the
  primary authoring surface. The Deno SDK and the subprocess-polyglot machinery are
  deleted with the module system they are built on. TypeScript authoring is paused,
  not rejected — a future TypeScript SDK follows this same importable-library model
  (a native module a TypeScript app imports; Deno itself optional), aimed at the
  hobbyist / video-creator audience when it is scheduled.
  [importable-python-library — SHIPPED #1707, #1708; importable-python-library-ripout
  — SHIPPED #1715 for the deletion clause]
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
  joined and every anchored thread state released before `rt.run()` returns, with an
  `atexit`/context-manager guarantee on the exception path. Proven against a real
  `python app.py` harness, the arrangement the spike never ran.
  [importable-python-library — SHIPPED #1707]
- **DECIDED** — `rt.run()` owns SIGINT, SIGTERM and SIGHUP through the whole teardown,
  engine drop included, and escalates on repeat: the first interrupt stops the graph
  gracefully; the second forces it — every helper's ladder skips to terminating its process
  group, and a native processor thread still inside its callback is abandoned; the third
  kills every helper's process group and exits with status 130 at once. A native processor
  thread that ignores shutdown past its budget is abandoned rather than joined: the engine
  stays alive beneath it, and `run()` raises naming the processor. An engine-chosen watchdog
  of about fifteen seconds ends a teardown hung anywhere else. The entry above's "all engine
  threads joined" therefore reads "joined, or abandoned and named". The `run()` docstring
  states the same.
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
  [shutdown-ladder; local-transport-hardening — SHIPPED #2266]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_ctrl_c_exits_cleanly -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_sigint_is_handed_back_to_cpython -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_a_second_ctrl_c_forces_the_shutdown_past_a_long_teardown -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_a_third_ctrl_c_kills_every_helper_process_group_and_exits_130 -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_sighup_tears_the_graph_down_gracefully -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_a_runtime_held_by_a_live_thread_is_torn_down_at_exit -->

## Distribution & versioning — IN-FLIGHT (→ macos-platform-floor)
<!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py -->

- **DECIDED** — Two artifacts, one version, released together: the streamlib wheel
  (Python API + CLI + engine) and the `streamlib` crate for Rust apps. Initial
  release channel is this repo's releases served through a static PEP 503 simple
  index (`pip install streamlib --index-url …` — one stable incantation) — PyPI
  publication waits for the project rename; the artifact is identical either way.
  Positioning is "realtime engine, Python authoring" — the Rust engine is named as
  material; never marketed as "a Python library" even though the shape is one.
  [importable-python-library — SHIPPED #1691, #1692, #1694, #1711]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli.py::test_the_scaffold_pins_streamlib_to_its_own_index -->
- **DECIDED** — Wheel portability model: system libraries (Vulkan loader, window
  system, libcuda) are dlopen'd at runtime, never linked — the wgpu/opencv-python
  manylinux shape; "baked in" means our Rust is compiled in, not that system deps
  are static. abi3 across a small range of GIL-enabled CPython builds only
  (free-threaded builds wait for the stable ABI to exist for them). "Our code" includes
  vendored C/C++ we compile and link statically, not only our Rust: the wheel carries a
  C++ GLSL shader compiler so a kernel author needs no system shader toolchain. The
  wheel's adapter closure excludes skia. Helper processes import the wheel itself — one
  native artifact, no separate helper cdylib.
  [importable-python-library — SHIPPED #1691, #1692; the vendored GLSL compiler with
  python-kernel-surface #1775]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py::test_the_native_extension_links_nothing_the_host_may_not_supply -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_wheel_portability.py::test_the_glsl_compiler_is_linked_statically -->

## Control plane & observability — IN-FLIGHT (→ cross-runtime-links)

- **DECIDED** — The control plane carries no optional capability's routes natively. A
  capability extension that needs an endpoint contributes it through the `host` door
  (§Packages & extension model), served by the one control plane in the app process
  under the same `RuntimeOperations`-shaped discipline — a handler sees what the app
  process sees, the graph and what the extension registered, and no helper's private
  state. The `moq` feature, `/api/moq/catalog`, the `runtime_id` plumbing they carried and
  their test stubs — the one coupling of this kind — are deleted with the networking move,
  and `graph` gained the `extensions` key in their place: what loaded, one entry per
  capability with its name, version and distribution. The door's spelling is the first
  extension's to bring when it needs one, and neither of the first two wheels needed it —
  a broadcast's catalog is the MoQ wheel's to serve.
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
  them a tool. The ripout's "the control plane never mutates the graph — code is the
  source of truth, the edit loop is `dev`" clause is reversed (owner, 2026-09-06:
  removing live mutation was an overstep; a dynamic graph an agent cannot add to is
  not worth having). A mutation compiles inside the call, so the caller learns whether
  its change took rather than reading a `Running` node with nothing flowing. A Python
  class is named by its import path — its descriptor registered when its decorator ran,
  its constructor at this first add exactly as `rt.add` supplies it; the app process
  imports the class, the processor runs in its own helper process — and a native
  built-in by the path `graph` reports for one;
  a link wired onto a running processor reaches it, and every channel is sized for a
  destination that connects later. `exchange` stays an observation verb because a read
  that costs the node a bounded copy is still a read. MCP is
  served by the node's control plane at `POST /mcp`, mounted with the node and sharing
  its lifecycle; it has exactly one transport, and no CLI verb, stdio server, or bridge
  process stands between a host and that endpoint — an MCP host is configured with a
  running node's URL. Beside its tools the node serves two resources, each rendered at
  the moment it is read: `streamlib://processor-catalog`, every processor type it can
  add with its description, derived config schema and ports — `/api/registry`'s
  document — and `streamlib://graph`, the `graph` tool's. It also serves four prompts,
  recipes rendered against the live graph and the catalog —
  `insert_processor_between_linked_processors`, `fan_output_to_another_consumer`,
  `show_channel_on_virtual_camera` and `look_at_what_a_channel_carries` — whose every
  step is a call to a served tool: a prompt is text, never a mutation path, so the tool
  set stays the whole of the control vocabulary.
  [importable-python-library, mcp-served-with-the-node — SHIPPED #1712;
  control-plane-surface-pixel-exchange — SHIPPED #1972, #1974 for the vocabulary
  sentence; live graph mutation restored by owner ruling 2026-09-06; resources and
  prompts — SHIPPED #2232, the catalog they serve from agent-readable-processor-catalog;
  local-transport-hardening — SHIPPED #2263, #2265 made the late-joiner sizing clause true
  in the tree and gave a helper's link the `pending` state the instructions now explain]
  <!-- verify: sdk/streamlib-python-wheel/tests/test_cli.py::test_the_wheel_serves_no_mcp_verb -->
  <!-- verify: cargo test -p streamlib-api-server tools_list_advertises_exactly_the_control_vocabulary -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_live_graph_mutation.py -->
  <!-- verify: cargo test -p streamlib-api-server resources_list_names_the_processor_catalog_and_the_live_graph -->
  <!-- verify: cargo test -p streamlib-api-server every_step_of_every_prompt_calls_a_tool_the_node_serves -->
  <!-- verify: cargo test -p streamlib-engine --lib core::compiler::compiler_ops::open_iceoryx2_service_op::tests::a_newest_and_an_ordered_consumer_share_one_running_output_port_each_at_its_own_depth -->
- **DECIDED** — `dev` and `run` bind the control plane identically: all interfaces
  (`0.0.0.0`) by default, narrowed per invocation by `--host`. There is no dev-only
  exposure posture — a node another host can reach is bound wide by definition, so
  reachability is not the lever that scopes exposure. [control-plane-bind-posture]
- **DECIDED** — The api-server is engine-side infrastructure and relocates into the
  `runtime/` tree: it is a host — statically linked, never dlopen'd. Its new host is
  the wheel (and the `streamlib` crate for Rust apps); the relocation is a sequencing
  prerequisite of the rip-out. [control-plane-one-surface]
- **DECIDED** — The CLI ships inside the wheel and slims to `new` / `dev` / `run` (a
  thin runner over the same engine the wheel exposes) plus the observation verbs
  (`nodes` / `graph` / `tap` / `logs` / `exchange`) and one machine-setup verb,
  `enable-virtual-camera`, which installs the loopback permission the virtual camera's
  loopback door needs behind the desktop's password prompt and touches no node;
  build-orchestration, packaging, provisioning, and codegen verbs are deleted. `exchange` takes a surface id, or a
  channel: the channel form composes tap → decode → exchange client-side in one warm
  process — one connection, the exchange fired the moment the bag lands, `--count` and
  every-Nth sampling as client flags. It is the cold-spawn latency fix and the
  throttling surface in one, and it adds nothing to the engine: the CLI stays a pure
  JSON-RPC client composing the same two operations any consumer composes. The
  standalone streamlib-runtime binary retires. Python embeds the engine in-process via
  the wheel; the control plane exists to observe and drive *running* nodes, not to
  embed.
  [importable-python-library — SHIPPED #1683, #1711; importable-python-library-ripout
  — SHIPPED #1715; control-plane-surface-pixel-exchange — SHIPPED #1975 for the
  `exchange` verb; virtual-camera-sink — SHIPPED #2196 for the setup verb]
  <!-- verify: sdk/streamlib-python-wheel/tests/test_cli.py::test_this_wheel_is_the_only_streamlib_cli -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py::test_the_channel_form_taps_then_exchanges_each_sampled_id -->
- **DECIDED** — One engine-resolved runtime directory holds everything a live runtime puts
  on disk that means nothing once its processes are gone: the node registry, the
  surface-sharing socket and the iceoryx2 domain. On Linux it is
  `$XDG_RUNTIME_DIR/streamlib/` when that variable is set and non-empty, and otherwise —
  empty or unset, and on macOS always — `/tmp/streamlib-<uid>/`, created owner-only and
  checked as a real directory this uid owns with no group or other bits. The check runs once
  as the runtime starts, before its first node, socket or registry write, and a failure
  refuses the start by name; every user takes the resolved directory. No StreamLib variable
  overrides it — a container or CI job sets `XDG_RUNTIME_DIR` — so a runtime starts anywhere
  with nothing set and the old Linux refusal is gone, and the wheel's Python registry reader
  resolves identically. What a runtime *keeps* — logs, caches — stays under the project's
  `.streamlib/`. Node discovery is the per-user on-disk registry inside that directory: one
  JSON file per live node, written only by control-plane-hosting runtimes, pruned only when
  both liveness signals (control round-trip, process check) fail. The entry carries the
  runtime's own `runtime_name` beside its `runtime_id`, taken from the runtime rather than
  from the control plane it hosts, at a bumped schema version; the control plane no longer
  carries a name of its own, and the adjective-noun generator that minted one nothing ever
  read is gone. Owner, 2026-09-14.
  [control-plane-one-surface; the directory — local-transport-hardening, SHIPPED #2261;
  `runtime_name` on the entry — runtime-mesh, SHIPPED #2282]
  <!-- verify: cargo test -p streamlib-engine --lib core::runtime::streamlib_runtime_directory -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_runtime_directory.py -->
- **DECIDED** — Observability: the JSONL log schema is a durable contract; tap forwards
  bags verbatim, trading completeness for guaranteed non-interference; graph and health
  inspection ride the same control plane. [control-plane-one-surface]
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
  [control-plane-surface-pixel-exchange — SHIPPED #1972]
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
  to kill. [control-plane-surface-pixel-exchange — SHIPPED #1972]
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
  outwaiting it is an error. [control-plane-surface-pixel-exchange — SHIPPED #1972]
  <!-- verify: cargo test -p streamlib-engine --lib a_retired_frame_id_is_refused_at_the_exchange_naming_the_recycling -->
  <!-- verify: cargo test -p streamlib-api-server tools_call_exchange_on_a_recycled_frame_is_a_tool_error_naming_the_recycling -->
- **DECIDED** — The engine converts, in the RHI, or the caller gets nothing viewable: a
  camera frame is NV12 or YUYV and converting it is the RHI's existing job, while
  readback is an always-present `GpuContext` capability. No pixel conversion happens
  outside the RHI and no second converter is built. The operation reaches the engine
  through `RuntimeOperations` and nothing else — the api-server's HTTP task deliberately
  holds only `Arc<dyn RuntimeOperations>`, the trait gains one operation, and `Runner`
  implements it over the shipped doors: the surface store's checkout for a pooled
  pixel-buffer backing, the host-visible export staging for a texture backing, the same
  doors the cast object's `cpu()` rides. No new surface-resolution path exists, and the
  caller needs no Vulkan device, no surface socket and no runtime link.
  [control-plane-surface-pixel-exchange — SHIPPED #1972]
  <!-- verify: cargo test -p streamlib-engine --lib a_pooled_rgba_frame_exchanges_for_the_pixels_the_bag_published -->
  <!-- verify: cargo test -p streamlib-engine --lib a_texture_backed_frame_exchanges_for_the_pixels_its_producer_rendered -->
- **DECIDED** — Two spellings of one operation: MCP tool and REST route serve the same
  `exchange` with the same arguments, differing only in result shape. REST serves the
  exact frame as a binary `image/png` body — lossless, full resolution, no base64
  inflation, remote-capable: the evidence and PSNR path, and what the CLI writes into a
  caller-named directory. The MCP tool returns an image content block, downscaled by
  default to a declared long-edge cap (~1568 px, the resolution ceiling vision models
  actually use), with the result stating the true extent and the exact-bytes route — the
  agent's in-session view, always under the per-image payload ceiling. The downscale
  rides the RHI's existing blit path, never a second scaler, and raw unconverted planes
  stay deferred until something needs them. The REST spelling joins the bearer-gated set
  beside the tap WebSocket and MCP inherits the gate the whole dispatch already has:
  whatever the auth entry below decides later, it decides for this verb the same as the
  rest. [control-plane-surface-pixel-exchange — SHIPPED #1972, #1974]
  <!-- verify: cargo test -p streamlib-api-server the_exchange_route_answers_the_operation_bytes_verbatim_as_an_image -->
  <!-- verify: cargo test -p streamlib-api-server tools_call_exchange_states_the_true_extent_the_id_and_the_exact_bytes_route -->
  <!-- verify: cargo test -p streamlib-api-server the_exchange_route_rejects_a_missing_token_with_401_when_auth_on -->
- **DECIDED** — The observer effect is the problem being removed, and its absence is the
  proof. Reading a channel no longer requires terminating it in a window, so a mid-graph
  channel is observable in the topology that ships. Window capture survives only where
  the window is genuinely the subject — the present and swapchain path.
  [control-plane-surface-pixel-exchange — SHIPPED #1972, #1976]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-26-control-plane-surface-pixel-exchange.md -->
- **OPEN** — Auth and remote-access posture: how a node authenticates and authorizes
  control-plane callers, and what it exposes to a mesh. Scoping exposure down is decided
  here and only here — it is a question of who may call, never of what the node listens
  on, so no narrower bind default is set ahead of it. [control-plane-bind-posture]

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

## Packages & extension model — IN-FLIGHT (→ importable-python-library, cast-object-tensor-protocol, texture-backed-cpu-reach)

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
  vtable surface, no load handshake, no build fingerprints. The extension paths are
  Python packages and Rust source crates only.
  [importable-python-library; importable-python-library-ripout — SHIPPED #1715]
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
  address-space-local pointer is not a handle. [importable-python-library]
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
  `-abi` halves die with the plugin ABI. [importable-python-library;
  surface-id-lifetime-contract — SHIPPED #1868 for the source clause]
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
  The wheel ships the protocol as one public composable piece any cast type composes,
  over the unchanged claim seam — `VideoFrame` is itself built from it, which is the
  proof it holds no privileged position over any library or user cast type. The bare
  protocol binds a type that claims exactly one surface: a type claiming several gets
  no bare `__dlpack__` — the ambiguity is refused by name — and reaches each surface
  through that surface's own protocol object. `cpu()` yields a numpy array writable
  exactly when the frame can take a write-back.
  Wheel-layer grammar only over the shipped staging, export and escalate
  primitives — no engine change. [cast-object-tensor-protocol;
  texture-backed-cpu-reach — the staged cpu() arm]

## Processor model & scheduling — IN-FLIGHT (→ importable-python-library)

- **DECIDED** — A link is pure plumbing: output port → input port, carrying a bag
  (self-describing msgpack named map). The engine has no type layer: ports carry no
  type declaration, connect never inspects or compares types and never warns, no read
  path examines a tag, and the frame header carries no schema ident. Consuming is a
  cast at read time; a mismatch surfaces as a decode failure at the consuming
  processor. [schema-free-ports — SHIPPED #1814]
  <!-- verify: cargo test -p streamlib-ipc-types frame_header_size_matches_constant -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-11-schema-free-ports.md -->
- **DECIDED** — A port declares three things and nothing else: name, description, and
  — on an input — delivery profile. Type information belongs to the authoring language
  and never reaches the engine: in Python the port method's return annotation is the
  declaration, read by humans and type checkers only, with `ctx.inputs.read(port)`
  yielding the bag as a mapping and `read(port, into=T)` the opt-in strictness dial
  (a TypedDict casts for free, a dataclass or pydantic model constructs and validates,
  raising at read); in Rust the read target's `Deserialize` impl is the validation,
  always on, with no free-cast mode. [schema-free-ports — SHIPPED #1816, #1812]
  <!-- verify: sdk/streamlib-python-wheel/tests/test_read_into_target.py -->
- **DECIDED** — Channel policy (delivery profile, ring depth, overflow) is declared
  port-locally at the consuming input port. Every input port declares its delivery
  profile explicitly — there is no default and nothing left to infer one from, so an
  input port without one is a wiring error. [schema-free-ports — SHIPPED #1811]
  <!-- verify: cargo test -p streamlib-engine missing_declaration_is_a_wiring_error_naming_the_port -->
  <!-- verify: sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_an_input_port_without_a_delivery_profile_is_refused -->
- **DECIDED** — There is no schema layer: no JTD, no schema registry, no embedded
  schemas, no codegen and no generated type classes, and no schema identity grammar
  anywhere in the engine or the authoring surfaces. [schema-free-ports — SHIPPED
  #1813, #1815; the `SchemaIdent` grammar itself — processor-class-identity, SHIPPED
  #1841]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-11-schema-free-ports.md -->
- **DECIDED** — Port rendering in the control plane is name, description, delivery
  profile, and direction; no port carries a type in `graph`, `tap`, or any snapshot.
  [schema-free-ports — SHIPPED #1816]
  <!-- verify: sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_a_declared_port_carries_no_type_key_under_any_spelling -->
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
  <!-- verify: sdk/streamlib-python-wheel/tests/test_helper_placement.py -->
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
- **DECIDED** — An instance's display name is the human-facing label — passed at `add`,
  readable off the returned handle, and the prefix on its log records; it defaults to
  the class's short name and the engine disambiguates duplicates within one graph.
  Identity is never derived from it — and neither is the default: a descriptor carries
  the class's short name as its own validated field rather than the engine splitting one
  out of the import path, because splitting re-invents the grammar this change deleted.
  [processor-class-identity — SHIPPED #1838, #1841]
  <!-- verify: cargo test -p streamlib-engine --test display_name_disambiguation_test -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_graph_building.py::test_a_duplicate_requested_display_name_is_disambiguated_too -->
- **OPEN** — Additional execution flavors to scale processor count (lightweight /
  green-thread style): intended, do not build until designed; hard constraint — no new
  configuration dials. [execution-model]

## Graphics (RHI / GPU) — IN-FLIGHT (→ texture-backed-cpu-reach)

- **DECIDED** — All Vulkan lives in the RHI (`vulkan/rhi/` + `streamlib-consumer-rhi`); one
  kernel abstraction per pipeline kind; consumers go through `GpuContext` only.
- **DECIDED** — The engine's kernel primitives are exposable to Python as configured
  blocks: shader/compute source and binding config passed from Python, compiled and
  executed by the engine on its device — no user-side Vulkan, ever.
  [importable-python-library]
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
  included, which is what makes its write-back legal — and a writable staged array
  publishes at the block edge, ordered ahead of the engine's next read; leaving by
  a propagating exception discards the edit without suppressing the exception. The
  door's one contract across both backings: a raise leaves the frame the engine
  already held or a complete edit of fewer pixels, never a torn frame — which of
  the two is the backing's own, and code that must not publish on failure edits
  outside the scope. Every staging copy blocks: `contended` reaches no author, and
  the unconsumed non-blocking surface — the `try_run_cpu_readback_copy` wire op,
  the `contended` response variant, and the engine's `try_`-prefixed staging
  copies — is deleted. The readback staging allocates host-cached from a third
  OPAQUE_FD pool (probed HOST_ACCESS_RANDOM), falling back to the sequential-write
  pool on a device with no cached exportable memory type — slower there, never
  refused. Python's `acquire_texture` implies `copy_src` and `copy_dst`; Rust's
  descriptor stays explicit; a texture whose usage still cannot take the copy
  refuses the door by name. [texture-backed-cpu-reach]
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

## Media I/O — camera, display, audio — IN-FLIGHT (→ importable-python-library, processor-owned-windows)

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
- **DECIDED** — Windowing: the engine owns the process's one event pump and mints
  windows on request; a window-owning processor registers with it and keeps every
  window policy decision — title, extent, what a resize means, when to redraw, what
  closing does. winit permits one event loop per process, so the loop is owned once,
  above every processor that wants a window, and N window-owning processors coexist
  in one process. Each window's owner renders on its own thread, never the pump's, so
  windows are not serialised behind one render loop. The
  raw-window-handle seam remains the internal boundary — the engine mints
  the present target from the raw handle and owns every swapchain and acquire detail,
  plus the platform main-thread event loop where the OS demands it (in the importable
  arrangement the process main thread belongs to the user's script; `rt.run()` blocks
  with the GIL released while the engine pumps). A processor that cannot get a window
  drains and discards, so upstream still sees a live consumer.
  [importable-python-library; shared-window-event-pump]
  <!-- verify: cargo test -p streamlib-engine --test window_event_pump_serves_many_windows -->
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
  crosses it. A window is requested in `setup()`, where the typestate is Full, and
  released at teardown or with its processor — never minted mid-`process()`. The
  per-frame verb accepts anything that names a published surface: the cast object
  (whose claim guarantees the id un-recycled), a kernel-output handle, or a bare
  surface id. The pump's two events reach the owner as coalesced state polled off
  the window object, never a callback across the hop; an owner that reads neither
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
  window. [processor-owned-windows]
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
  a media timestamp; a fifth surface is a plan change, not a judgement call. The list is
  mechanically enforced, with no per-line pragma and no opt-out attribute: the permitted
  surfaces are a closed set in the gate, so a fifth is a source change that surfaces in
  review rather than a line quietly appended, and an entry whose file stops reading a
  wall clock is a licence the gate makes you hand back. The allowlist is per-file, which
  is why a data-plane file never joins it — a machine-global unique name comes from the
  engine's unique-name primitive, never from reading a clock.
  [one-monotonic-clock — SHIPPED #1725, #1726, #1727, #1728]
  <!-- verify: cargo test -p streamlib-engine --lib now_lands_in_the_kernel_monotonic_domain -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_clock_and_log.py::test_monotonic_now_ns_reads_the_kernel_monotonic_clock -->
  <!-- verify: cargo run -p xtask -- check-clock-usage -->
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-13-one-monotonic-clock.md -->
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
  hobbyist / video-creator audience when it is scheduled.
  [importable-python-library; importable-python-library-ripout — SHIPPED #1715 for the
  deletion clause]
  <!-- verify: bash .claude/scripts/ship-change-removed-gate.sh docs/plan/changes/archive/2026-08-10-importable-python-library-ripout.md -->
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
  (free-threaded builds wait for the stable ABI to exist for them). "Our code" includes
  vendored C/C++ we compile and link statically, not only our Rust: the wheel carries a
  C++ GLSL shader compiler so a kernel author needs no system shader toolchain. The
  wheel's adapter closure excludes skia. Helper processes import the wheel itself — one
  native artifact, no separate helper cdylib. [importable-python-library]

## Control plane & observability — IN-FLIGHT (→ importable-python-library, control-plane-bind-posture)

- **DECIDED** — One control plane: the api-server's HTTP + WebSocket + MCP surface,
  hosted in-process by any runtime that enables it. The MCP tool set is the canonical
  control vocabulary; the CLI is a pure JSON-RPC client of it — agents and humans use
  the same verbs; REST/WS routes serve the same operations for programmatic clients.
  Post-pivot the vocabulary is observation-shaped: graph, tap, logs, health, nodes.
  The live-mutation verbs (submit / replace / connect / remove) and their MCP tools
  are removed — code is the source of truth; the edit loop is `dev`, not live
  mutation. MCP is served by the node's control plane at `POST /mcp`, mounted with
  the node and sharing its lifecycle; it has exactly one transport, and no CLI verb,
  stdio server, or bridge process stands between a host and that endpoint — an MCP
  host is configured with a running node's URL.
  [importable-python-library; mcp-served-with-the-node — SHIPPED #1712]
  <!-- verify: sdk/streamlib-python-wheel/tests/test_cli.py::test_the_wheel_serves_no_mcp_verb -->
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
  (`nodes` / `graph` / `tap` / `logs`); build-orchestration, packaging,
  provisioning, and codegen verbs are deleted. The standalone streamlib-runtime
  binary retires. Python embeds the engine in-process via the wheel; the control
  plane exists to observe and drive *running* nodes, not to embed.
  [importable-python-library; importable-python-library-ripout — SHIPPED #1715]
  <!-- verify: sdk/streamlib-python-wheel/tests/test_cli.py::test_this_wheel_is_the_only_streamlib_cli -->
- **DECIDED** — Node discovery is a per-user on-disk registry — one JSON file per live
  node in the OS's standard per-user runtime directory — written only by
  control-plane-hosting runtimes, pruned only when both liveness signals (control
  round-trip, process check) fail. [control-plane-one-surface]
- **DECIDED** — Observability: the JSONL log schema is a durable contract; tap forwards
  bags verbatim, trading completeness for guaranteed non-interference; graph and health
  inspection ride the same control plane. [control-plane-one-surface]
- **OPEN** — Auth and remote-access posture: how a node authenticates and authorizes
  control-plane callers, and what it exposes to a mesh. Scoping exposure down is decided
  here and only here — it is a question of who may call, never of what the node listens
  on, so no narrower bind default is set ahead of it. [control-plane-bind-posture]

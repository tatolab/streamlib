# StreamLib Architecture Plan

The single source of architectural decisions. Sessions implement this plan; they do not make
architecture. A decision missing here stops work and comes back to the owner — it is never
inferred from existing code, consumers, or history. This document and the diagrams under
`diagrams/` (Mermaid `.mmd`, the committed source — Excalidraw files are generated views,
never round-tripped back) move together: every DECIDED entry is represented in the diagram.

Legend: **DECIDED** — build exactly this. **OPEN** — do not build; needs an owner decision.

## Product (the MVP sentence) — SHIPPED
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
  <!-- verify: cargo test -p streamlib example_dir_has_no_ceremony_files -->

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
  address-space-local pointer is not a handle.
  [importable-python-library — SHIPPED #1710, #1756, #1757]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_device_exchange.py -->
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
  texture-backed-cpu-reach — SHIPPED #1942 for the staged cpu() arm]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_claimed_surface_pixel_access.py::test_the_bare_object_hands_back_the_surfaces_own_capsule -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_claimed_surface_pixel_access.py::test_a_two_surface_type_is_refused_every_bare_door_naming_the_surfaces -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_claimed_surface_pixel_access.py::test_a_frame_that_cannot_take_a_write_back_arrives_read_only -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_video_frame_claim.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_compute_kernel.py::test_a_raise_inside_the_staged_cpu_door_discards_the_edit -->

## Processor model & scheduling — IN-FLIGHT (→ delivery-profile-vocabulary)

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
- **DECIDED** — The delivery profile is the whole of channel policy: one word, declared
  port-locally at the consuming input port. Every input port declares its delivery profile explicitly — there is no default
  and nothing left to infer one from, so an input port without one is a wiring error.
  Ring depth and overflow policy are engine-chosen and are not authorable: no port
  declares a depth, a leak policy, or a queue element, and there is no second surface
  that tunes one. [schema-free-ports — SHIPPED #1811; delivery-profile-vocabulary]
  <!-- verify: cargo test -p streamlib-engine missing_declaration_is_a_wiring_error_naming_the_port -->
  <!-- verify: sdk/streamlib-python-wheel/tests/test_processor_declaration.py::test_an_input_port_without_a_delivery_profile_is_refused -->
- **DECIDED** — The delivery profile names a read policy and nothing else. There are
  exactly two: `newest` — the consumer drains to the most recent bag, older ones are
  passed over — and `ordered` — the consumer receives bags in publication order. Both
  drop under sustained pressure. Neither promises delivery, because on a link whose head
  is a device that will not wait, no port-local declaration can: backpressure only
  relocates the loss to the device edge. `lossless` is retired — the word promised what
  the runtime does not do. [delivery-profile-vocabulary]
- **DECIDED** — No loss is silent. A bag dropped at a port is counted by the port that
  dropped it and is readable over the control plane in `graph`, alongside the processor's
  other metrics. Drops are counted per link, never as one blended total, so a future
  reflection of a link's count to its producer stays possible without recounting. A drop
  is a normal, reportable event on a realtime link, never an error and never invisible —
  a run that lost most of its bags must not read as a healthy one.
  [delivery-profile-vocabulary]
- **DECIDED** — No link ever blocks a producer. Producer-blocking is deleted, not merely
  unreachable: no profile resolves to it and the overflow policy it was the second half
  of goes with it. A processor publishing to a slow consumer loses bags at that
  consumer's port, where the loss is counted; it is never parked. The capability was never engineered
  — the engine never chose the blocking semantics it would have had, and a parked
  producer cannot observe shutdown — and keeping it cost the tree two standing
  workarounds. Counted drops land before or with the deletion, so the alternative to
  silent loss exists the moment blocking stops being one.
  [delivery-profile-vocabulary]
- **DECIDED** — Loss-handling knowledge lives at the link's endpoints, never in the
  engine. The engine's whole role is to count a drop at the port that dropped it and
  surface it; it never inspects a payload, never knows a bag holds a reference frame, and
  never acquires a drop rule that depends on content. A producer that can make loss
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

## Graphics (RHI / GPU) — IN-FLIGHT

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

## Media I/O — camera, display, audio — IN-FLIGHT

- **DECIDED** — First-party camera, display, and audio are native built-in processors
  in the engine tree, statically linked into the wheel — pre-built named blocks
  instantiated and configured from Python (`rt.add(CameraSource)`), whose per-frame
  paths never enter the interpreter. Lag-by-design ends: built-ins ship inside the
  wheel, current by construction. [importable-python-library — SHIPPED #1709]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_native_builtin_blocks.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_launch.py::test_a_native_block_added_without_config_reaches_a_running_graph -->
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
- **DECIDED** — An audio input port may declare a window contract — rate, channels,
  dtype, window size, hop — beside its delivery profile (audio declares `ordered`):
  the engine resamples, mixes down, and frames natively so `process()` receives
  exact-size timestamped blocks matching the declaration. Resampling is an always-on
  engine stage, never a user processor. Feature extraction (mel, MFCC) is not engine
  surface: the contract ends at windowed raw samples. [audio-subsystem]
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
  source counts what it dropped — never silent.
  [audio-subsystem; dlopen-audio-backend-and-audio-blocks — SHIPPED #1989, #1992 for
  the two built-ins, their execution mode and the drop-at-the-edge clause; conditioning
  and immediate cancel are a later rung]
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_microphone_source.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_speaker_sink.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_speaker_sink.py::test_a_microphone_wired_to_a_speaker_runs_and_plays_what_it_captured -->
- **OPEN** — Audio plugins (CLAP / VST3 / LV2): intended, do not build until a
  concrete consumer demands a specific plugin. Direction: CLAP first; the plugin runs
  out-of-process in its own helper over the engine's IPC transport, never in the app
  process; a plugin an app uses is declared project-locally — shipped in or referenced
  by the app's own project, the shader precedent — never discovered from
  machine-global scan paths; the lane costs nothing when unused (no `DT_NEEDED`
  entries, no import-time work). [audio-subsystem]

## Networking — transport, moq, webrtc

- **DECIDED** — Cross-language interop happens on the wire between nodes, as
  self-describing bags — never in-graph. [importable-python-library — SHIPPED #1715]
- **OPEN** — Everything else (transport choice, moq, webrtc, mesh discovery).

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
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_ctrl_c_exits_cleanly -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_interpreter_lifecycle.py::test_sigint_is_handed_back_to_cpython -->

## Distribution & versioning — SHIPPED
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

## Control plane & observability — IN-FLIGHT

- **DECIDED** — One control plane: the api-server's HTTP + WebSocket + MCP surface,
  hosted in-process by any runtime that enables it. The MCP tool set is the canonical
  control vocabulary; the CLI is a pure JSON-RPC client of it — agents and humans use
  the same verbs; REST/WS routes serve the same operations for programmatic clients.
  Post-pivot the vocabulary is observation-shaped, and the served MCP tool set is
  exactly `graph`, `tap`, `logs`, `exchange` and `shutdown` — `health` is a REST route
  and `nodes` a registry surface, neither of them a tool. `exchange` joins as an
  observation verb without loosening the pivot's rule: the control plane never mutates
  the graph — the live-mutation verbs (submit / replace / connect / remove) and their
  MCP tools are removed, code is the source of truth, the edit loop is `dev`, not live
  mutation — because a read that costs the node a bounded copy is still a read. MCP is
  served by the node's control plane at `POST /mcp`, mounted with the node and sharing
  its lifecycle; it has exactly one transport, and no CLI verb, stdio server, or bridge
  process stands between a host and that endpoint — an MCP host is configured with a
  running node's URL.
  [importable-python-library, mcp-served-with-the-node — SHIPPED #1712;
  control-plane-surface-pixel-exchange — SHIPPED #1972, #1974 for the vocabulary
  sentence]
  <!-- verify: sdk/streamlib-python-wheel/tests/test_cli.py::test_the_wheel_serves_no_mcp_verb -->
  <!-- verify: cargo test -p streamlib-api-server tools_list_advertises_exactly_the_observation_vocabulary -->
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
  (`nodes` / `graph` / `tap` / `logs` / `exchange`); build-orchestration, packaging,
  provisioning, and codegen verbs are deleted. `exchange` takes a surface id, or a
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
  `exchange` verb]
  <!-- verify: sdk/streamlib-python-wheel/tests/test_cli.py::test_this_wheel_is_the_only_streamlib_cli -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py -->
  <!-- verify: pytest sdk/streamlib-python-wheel/tests/test_cli_observation_verbs.py::test_the_channel_form_taps_then_exchanges_each_sampled_id -->
- **DECIDED** — Node discovery is a per-user on-disk registry — one JSON file per live
  node in the OS's standard per-user runtime directory — written only by
  control-plane-hosting runtimes, pruned only when both liveness signals (control
  round-trip, process check) fail. [control-plane-one-surface]
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

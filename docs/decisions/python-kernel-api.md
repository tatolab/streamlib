# The Python kernel API

Rationale for the `[python-kernel-api]` entries in `docs/plan/ARCHITECTURE.md` §Graphics,
decided by the owner 2026-08-07. Discharges the "Shape only; the Python-facing API design is
its own session" clause that §Graphics had carried since the pivot.

## Trigger

Read this before designing any Python-facing GPU surface, before adding a kernel capability
to one language and not the other, before proposing that Python hand StreamLib pre-compiled
SPIR-V, or when someone asks why the four GPU bridge traits were deleted.

## Decision

1. **Parity is the bar.**
   > ~~Parity is the bar. Python reaches every GPU capability Rust authoring reaches~~
   > — Superseded 2026-08-17 by owner ruling. The bar is every kernel *kind*, which is
   > what the enumeration in this same sentence already names. Pipeline state and buffer
   > resources inside a kind are a narrower claim, and the ones Python cannot reach are
   > named in the plan rather than promised here. The rest of (1) stands unnarrowed:
   > Python still names and drives every kind, and being "a proxy to Rust-powered GPU
   > work — not a lesser scripting surface beside it" claims a relationship, not a
   > surface area.

   Python reaches every kernel kind Rust authoring reaches —
   compute, graphics and ray-tracing kernels, acceleration structures, CPU readback. Python
   names and drives; the engine allocates, compiles, binds, and dispatches. Being a proxy to
   Rust-powered GPU work — not a lesser scripting surface beside it — is the differentiator
   the library is built on.
2. **A kernel is an object**: constructed in `setup()` where the capability typestate is
   Full, dispatched per frame in `process()`. Bindings are passed at dispatch, by name.
3. **Kernel outputs are engine-owned textures**, named by surface id, passed downstream in a
   bag, and reached by a third-party GPU package through a scope: entering blits the texture
   to a linear DLPack view over DMA-BUF / OPAQUE_FD, leaving blits any write back and orders
   it on the surface's timeline ahead of the engine's next read. The engine owns the
   ordering; Python never sees a fence.
4. **GLSL is the source contract**, compiled by the engine at kernel construction and cached
   by source hash; SPIR-V remains an escape hatch. The wheel carries a C++ compiler
   (shaderc / glslang).
5. **The four bridge traits are deleted.** Compute, graphics, ray tracing and CPU readback
   are always-present capabilities of `GpuContext`.
6. **Dispatch is synchronous**, with batching for multi-pass work.
7. **Rust converges on the same spelling** — bindings at dispatch, nothing persisting on the
   kernel — as its own change, sequenced after the Python surface.

## Rejected alternatives

**Texture reach, not names-only.** The cheaper option was to let Python *name* a kernel's
output surface and pass it downstream without ever mapping it — enough for camera → kernel →
display, and it needs no cross-process texture import. It was rejected because it would have
contradicted a decision already on the books: §Packages holds that the engine's handle-shaped
primitive surface is "surfaced to the Python ecosystem as DLPack and the CUDA Array
Interface". A skia-class or torch-class package that manipulates GPU resources needs the
texture itself, not a token for one. The wheel refuses this today — `acquire_texture` raises
*"device textures are not reachable from a Python processor"* — which is an implementation
gap, not a decision.

**A scope, not read-only reach and not an explicit publish.** Third-party *read* access alone
would have avoided the ordering question entirely, but it rules out the torch-writes-the-frame
pattern and walks back the parity the first decision states. An explicit `publish()` was
rejected because a forgotten call discards GPU work silently. The scope reuses the round trip
the wheel already implements (`publish_device_write_back_to_surface`, ordered on the surface's
timeline) and spells it as the `with` block Python readers expect. The blit in both directions
is not a wart to hide: §Packages already commits to it, because DLPack expresses strided
linear memory and engine textures are tiled. Saying a third party writes "the same texture"
without naming the round trip would have left an implementer unable to tell which of two
mechanisms was decided.

**Kernel objects over per-frame calls.** The wire is already register-once-dispatch-many,
keyed by SHA-256 of the shader. An imperative `ctx.gpu.dispatch_compute(source, …)` per frame
would work — a cache makes it fast — but the expensive step and the cheap step would look
identical in the source, and correctness would rest on a cache the reader cannot see. Objects
put registration in a constructor and dispatch in a method, which is the same resource-handle
pattern game engines settled on for the same reason. A declarative form on `@processor` was
rejected separately: it cannot express a kernel whose shape depends on runtime configuration,
and it adds a declaration surface the pivot has been removing everywhere else.

**GLSL in the wheel over SPIR-V bytes.** Nothing in the tree compiles a shader at runtime —
no `shaderc`, no `naga`, not even transitively — and every `glslc` invocation lives in a
`build.rs`. SPIR-V-only would therefore put a C++ shader toolchain and a build step in front
of anyone wanting to write a filter, in a library whose stated bar is that a user pip-installs
it and edits a Python file. Compiling in a shader compiler does not strain §Distribution: that
section's rule is that *system* libraries are dlopen'd — a compiler we vendor and link
statically is ours, not the system's.

**A C++ compiler over a pure-Rust one.** naga would have kept the wheel's compiled-in code
entirely Rust and left §Distribution's wording untouched, which is the tidier outcome. It was
rejected because its GLSL front end does not cover the ray-tracing *pipeline stages* — raygen,
miss, closest-hit, any-hit, intersection — that a ray-tracing kernel is built from; whatever
inline ray-query support it carries addresses a different shape, a query issued from within a
compute shader. Ray-tracing kernels are a decided Python capability, so a GLSL contract that
silently excluded one kernel kind would not be the contract stated, and an author would
discover the hole only on writing a raygen shader. Front-end coverage is a checkable claim
that moves with releases; if it ever covers the pipeline stages, this rejection is worth
revisiting on its own terms. The
cost accepted is a statically linked C++ toolchain inside an abi3 manylinux wheel, which
widens "our Rust is compiled in" to "our code is compiled in" — vendored C/C++ included.

The compiler shipped is `shaderc` 0.10.1 taken `build-from-source`, which vendors shaderc,
glslang, SPIRV-Tools and SPIRV-Headers in the crate and links `libshaderc_combined.a`. That
feature is not a preference: it forbids the build script's system-library probe, so no build
host's stray `libshaderc.so` can end up as a runtime dependency. `glslang` (SnowflakePowered)
was the lighter option — a `cc`-only build, no cmake — and was passed over because its safe
wrapper exposes no entry point and its diagnostics are rawer, and a kernel author reads those
diagnostics.

**The licences, recorded because a reader will grep and find the scary one.** shaderc and
SPIRV-Tools are Apache-2.0, glslang proper is 3-Clause BSD, SPIRV-Headers is MIT — all
notice-only, none copyleft. `glslang_tab.cpp` is Bison-generated and carries the GNU GPL
header, *with* the Bison special exception ("you may create a larger work that contains part
or all of the Bison parser skeleton and distribute that work under terms of your choice, so
long as that work isn't itself a parser generator"). StreamLib is not a parser generator, so
no GPL obligation reaches it — the ordinary case for anything shipping a Bison parser. What
the notice-only licences do owe is attribution on binary distribution, an obligation the
wheel already carried for its Rust closure and does not yet discharge anywhere.

**Collapsing the bridges over installing them.** The four traits exist to route between an
app-process arm and a cdylib arm. The cdylib arm dies with the plugin ABI, helper children
always take the app-process arm, and `importable-python-library-ripout.md` already records
that the branch collapses onto the path it was using. Every non-test implementation in the
tree lives in a `polyglot-*` example on the deletion list — the only others are mocks inside
the escalate handler's own test module — and each keeps a UUID → Texture map built once at
startup that never grows — so preserving the seam would have meant the wheel re-creating
the application-setup-glue pattern the pivot exists to remove. Keeping the traits as a
third-party extension point is not available either: an external implementor would have to
depend on the `streamlib` crate, which §Packages forbids.

**Synchronous with batching, not async.** Isolation is the axis the placement model optimises,
so a helper blocking its own thread costs nothing another processor can observe, and a call
that returns with its writes guaranteed visible is one a reader can reason about. Async
dispatch would put a fence and timeline vocabulary into the Python surface for throughput the
MVP does not need. Batching is not an optimisation here but a parity requirement: Rust nests
several dispatches in one command buffer via `RhiCommandRecorder::record_dispatch`, and
per-dispatch blocking alone would have made a two-pass blur cost twice the stalls in Python
and left a Rust-only capability standing, contrary to (1).

**Rust converges.** Rust already has kernel objects; what it lacks is safety in how bindings
attach. `set_storage_image(0, src)` mutates state that persists until overwritten, so a
missed rebind silently reuses last frame's texture, and because the setters take `&self`
behind a mutex, two threads dispatching one kernel can interleave their bindings.
`VulkanToneMapper::prepare()` exists to paper over exactly this. Two divergent kernel
spellings in one engine is the parallel-system shape the doctrine forbids, so the direction is
decided now; the refactor is sequenced separately because folding an RHI-wide change into the
Python surface ticket would make the diff unreviewable.

## What the existing demos were, and were not

Worth recording, because their names mislead and they were the only evidence of a working
Python kernel path. `examples/polyglot-vulkan-compute` is a **pure generator** — mandelbrot,
one `imageStore`, and the incoming frame explicitly ignored. `examples/polyglot-cpu-readback-blur`
**has no shader at all**; its blur runs on the CPU over mapped staging via numpy. Neither
performed a GPU read-input-write-output pass, because the compute wire could not express one:
`run_compute_kernel` carried a single `surface_uuid` bound as a storage image at slot 0,
output-only. Graphics and ray tracing already carried general binding arrays, and the host
`VulkanComputeKernel` already supported arbitrary slots and kinds — the restriction was
wire-level only. Lifting compute to an N-binding array is what makes a Python filter possible
at all.

## Consequences

- Cross-process texture import must be built; `acquire_texture` and `import_dma_buf` stop
  raising.
- The texture scope's blit-out / blit-back round trip extends the existing device-export
  staging path from pixel buffers to tiled textures, and the engine orders the write-back
  against its own next read without surfacing a timeline to Python.
- The wheel gains a statically linked C++ shader compiler, and §Distribution's portability
  entry widens from "our Rust is compiled in" to cover vendored C/C++.
- `vulkan_compute_kernel.rs` and its graphics / ray-tracing equivalents lose their stateful
  numeric-slot setters; every consumer moves, including the codec paths using the raw-view
  escape hatches. `VulkanToneMapper::prepare()` retires as a shape.
- The escalate compute op grows a binding array; the v1 single-output convention retires.
- `GpuContext::set_*_bridge` and the four bridge modules under `core/context/` are deleted.

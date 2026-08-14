# Change: python-kernel-surface

Implements the six `[python-kernel-api]` DECIDED entries in §Graphics that concern the
Python-facing surface. ADR: `docs/decisions/python-kernel-api.md` (owner, 2026-08-07).

Scale tier: change artifact **+ ADR** — it touches the RHI and the escalate wire format; the
ADR is already written by the align. Recon verified at HEAD `4c08100c` by three read-only
sweeps (RHI + bridges, escalate wire, wheel Python surface).

**Out of scope by the plan's own sequencing:** the Rust bindings-at-dispatch convergence
(§Graphics' last `[python-kernel-api]` entry). The stateful numeric-slot setters on
`VulkanComputeKernel` (`:347-510`), `VulkanGraphicsKernel` (`:350-508`) and
`VulkanRayTracingKernel` (`:390-493`) therefore survive this change and the escalate handler
calls them; they retire with `VulkanToneMapper::prepare()` (`vulkan_tone_mapper.rs:58-69`) in
that change. Sequenced coexistence, not a parallel system.

## Sequencing

Land **after** the ripout deletes `sdk/streamlib-python`, `sdk/streamlib-deno` and the
`polyglot-*` examples: those two SDKs carry **checked-in** generated escalate types a wire
change would otherwise have to move, and the four bridge traits' only non-test
implementations are four `polyglot-*` example hosts already on the deletion list.

## Behavior after this change

A Python processor constructs a kernel in `setup()` and dispatches it in `process()`:

```python
def setup(self, ctx):
    self.blur = ctx.gpu_full_access.create_compute_kernel(source=BLUR_GLSL)

def process(self, ctx):
    frame = ctx.inputs.read("video_in")
    with ctx.gpu_limited_access.resolve_surface(frame["surface_id"]) as source:
        width, height = source.width, source.height
        output = ctx.gpu_limited_access.acquire_texture(width, height, "rgba16f", ["storage"])
        self.blur.dispatch(
            bindings={"source_image": source, "output_image": output},
            push_constants=self.push_constants,
            group_count=(width // 16, height // 16, 1),
        )
        ctx.outputs.write("video_out", {"surface_id": output.surface_id})
```

Bindings are a name→surface mapping passed at dispatch and never persisted on the kernel.
The names are the shader's own — the engine reads them from SPIR-V reflection at
construction. Compute reads one surface and writes another, at parity with graphics and ray
tracing; the v1 single-output slot is gone. `dispatch()` returns when the GPU work has
retired and the writes are visible. No handle string, fence, timeline or slot number reaches
Python.

Multi-pass work batches:

```python
with ctx.gpu_full_access.kernel_dispatch_batch() as batch:
    batch.dispatch(self.blur_horizontal, bindings={...}, group_count=(...))
    batch.dispatch(self.blur_vertical, bindings={...}, group_count=(...))
```

One command buffer, a full barrier between consecutive dispatches, one submit and one fence
wait on leaving the scope. This mirrors the Rust pair exactly — `kernel.dispatch()` alone
versus `RhiCommandRecorder::record_dispatch` inside `begin()`/`submit_and_wait()` — which is
why dispatch has two entry points in both languages.

A third-party GPU package reaches a kernel's output through a scope:

```python
with output.as_device_tensor() as tensor:      # blit out to a linear DLPack view
    torch.from_dlpack(tensor).mul_(2.0)        # write in place
# leaving normally blits back and orders it ahead of the engine's next read
```

Every kernel capability is always present. `create_compute_kernel`, `create_graphics_kernel`,
`create_ray_tracing_kernel`, the acceleration-structure builders and CPU readback answer from
`GpuContext` for every caller; there is no installation step and no runtime-absent case.

## The two questions the delta was asked to answer

**Named-binding error cases** — resolved by the plan, not owner calls. Every one raises
before any GPU work is submitted, and the message names the shader's declared bindings:

- **unknown** (a name the shader does not declare) — error at dispatch.
- **missing** (declared, not supplied) — error at dispatch. There is no implicit default and
  no carried-over value; the kernel holds no binding state to fall back on.
- **duplicate** — unrepresentable in a Python mapping; a wire binding array carrying one name
  twice is an error at dispatch, checked engine-side so the wheel is not the only guard.
- **kind mismatch** (a `storage_image` name given a buffer surface) — error at dispatch.
- **stage mismatch** (graphics / ray tracing: a name declared for a stage this kernel has no
  module for) — error at **construction**, where the multi-stage declaration is built.
- **name-stripped SPIR-V** on the escape hatch — error at construction. Bindings are by name
  in one spelling for both languages, so a blob whose `OpName` decorations were stripped
  cannot be bound at all; a numeric fallback would be the second spelling the plan forbids.
  The engine keeps debug names in what it compiles itself.

**A write when an exception is raised inside the texture scope** — see below.

## The texture scope's exception path — DISCARD (owner, 2026-08-07)

**Leaving the scope normally blits the write back. Leaving it by a propagating exception
discards it**, and the engine's texture keeps the kernel output it already held.

A raised exception means the third-party write did not finish, and blitting a half-written
linear view back publishes a torn frame — one that surfaces as corrupt pixels somewhere
downstream instead of at the `raise`. Discarding leaves a complete, valid frame in place and
lets the exception propagate to `process()`, where the engine's normal error path handles it.
This does not reopen the ADR's rejection of an explicit `publish()`: that rejection was about
a *forgotten* call discarding work silently, and an exception is not silent.

**One rule, both scopes.** The CPU pixel-buffer scope commits on the exception path today —
`GpuSurfaceHandle.__exit__` calls `close()`, which publishes any pending device write first
(`python_processor_context.rs:268-306`) however the block was left. It moves to the same
rule here rather than leaving two scopes with two behaviours; the MODIFIED bullet below
carries it.

## Factual gaps resolved by reading (not owner decisions)

- **Binding names are already available.** `rspirv-reflect` 0.9 (already a dependency) hands
  `DescriptorInfo.name` to `derive_bindings_from_spirv`, which discards it
  (`core/rhi/compute_kernel.rs:128`); the only read of `.name` in the tree is an error-message
  interpolation (`vulkan_compute_kernel.rs:1816`). Named bindings need no new reflection
  library — the data is already in hand and thrown away.
- **The UUID→texture map already has an engine home** — `GpuContext::texture_cache` +
  `resolve_texture_registration_by_surface_id` (`gpu_context.rs:418`, `:785`). Only the
  kernel cache the example bridges kept is genuinely new.
- **The parent half of texture checkout already exists.** `assign_texture_handle_id`
  registers a texture for cross-process import with produce/consume timelines
  (`subprocess_escalate.rs:839-895`), and `ConsumerVulkanTexture::import_render_target_dma_buf`
  / `::from_opaque_fd` are complete. Missing is the wheel-side arm — the helper's importer
  refuses every non-`dma_buf` handle and ends at a buffer
  (`python_helper_process_pixel_exchange.rs:621-698`). Write-back into a texture needs no new
  RHI primitive either: `RhiCommandRecorder::record_copy_buffer_to_image` exists
  (`vulkan_command_recorder.rs:444`) and the staging path never calls it.
- **"Cached by source hash" was never implemented in the engine.** The only SHA-256 kernel
  cache in the tree is `examples/polyglot-vulkan-compute/src/main.rs:132`; the engine's own
  SHA-256 over SPIR-V is the *driver* pipeline-cache filename, which stays.

## REMOVED

Bare patterns — the ship gate greps each line verbatim as a fixed string.

- REMOVED: ComputeKernelBridge
- REMOVED: GraphicsKernelBridge
- REMOVED: RayTracingKernelBridge
- REMOVED: CpuReadbackBridge
  The four trait modules under `runtime/streamlib-engine/src/core/context/` whole, their
  `mod.rs` declarations and re-exports (`:7`, `:9`, `:16`, `:19`, `:34-58`), and the
  decl/value structs they carry (`GraphicsBindingDecl`, `GraphicsBindingValue`,
  `GraphicsKernelRegisterDecl`, `GraphicsKernelRunDraw`, `RayTracingBindingDecl`,
  `RayTracingBindingValue`) — the escalate handler converts wire types to `GpuContext`
  arguments directly. `CpuReadbackCopyDirection` survives and needs a home (crate layout
  final at implementation).
- REMOVED: set_compute_kernel_bridge
- REMOVED: set_graphics_kernel_bridge
- REMOVED: set_ray_tracing_kernel_bridge
- REMOVED: set_cpu_readback_bridge
  The four installers and getters (`gpu_context.rs:2188-2262`), the four
  `Arc<Mutex<Option<…>>>` fields (`:461`, `:469`, `:477`, `:488`) and their constructor inits
  (`:511-517`, `:540-546`), and the four `GpuContextFullAccess` getter mirrors with their
  cdylib panic guards (`:5653`, `:5671`, `:5689`, `:5707`).
- REMOVED: registered on GpuContext
  The nine bridge-absent `ok_or_else` paths in `subprocess_escalate.rs` (`:1176`, `:1254`,
  `:1323`, `:1405`, `:1606`, `:1709`, `:1814`, `:1939`, `:2025`) — unrepresentable once the
  capability is always present.
- REMOVED: single-output convention
  The v1 compute convention and its prose (`escalate_request.yaml:283-289`,
  `compute_kernel_bridge.rs:55-62`).
- REMOVED: device textures are not reachable from a Python processor
  Both refusals (`python_processor_context.rs:562-574`, `:672-684`) and their stub docstrings
  (`_engine.pyi:343`, `:365`).
- REMOVED: importing a foreign DMA-BUF is not reachable from a Python processor yet
  The refusal (`python_processor_context.rs:745-759`) and its stub entry (`_engine.pyi:381`).
- REMOVED: device export is read-only
  The write-back refusal (`device_export_staging.rs:571-576`) and the `writable: false`
  texture arm (`:265`). Re-anchored by surface-id-lifetime-contract (#1865), which reworded
  the refusal and routed dual-backed pool surfaces through the same gate — so the gate is no
  longer texture-only, and what retires here is its texture half. Whether this bullet still
  wants the whole refusal is that change's call, not this one's.

## MODIFIED

- MODIFIED: `run_compute_kernel`'s `surface_uuid` (`escalate_request.yaml:277`) becomes a
  `bindings` array of `{name: string, kind: enum, target_id: string}`; `run_graphics_draw`
  (`:610-628`) and `run_ray_tracing_kernel` (`:1015-1038`) swap `binding: uint32` for
  `name: string`; the register-time declarations (`:352-377`, `:952-978`) do the same, and
  `register_compute_kernel` grows the declaration array it lacks today. `color_target_uuids`
  (`:646-654`) is untouched, exactly-one-entry rule included — it is not a descriptor binding
  and renaming the binding array must not sweep it up.
- MODIFIED: the reflected binding name stops being discarded engine-side.
  `ComputeBindingSpec` (`core/rhi/compute_kernel.rs:38`), the graphics equivalent and
  `RayTracingBindingSpec` (`core/rhi/ray_tracing_kernel.rs:148`) carry the name alongside
  the binding number, and all three reflection paths keep it —
  `derive_bindings_from_spirv` (`compute_kernel.rs:108`),
  `derive_bindings_from_spirv_multistage` (`graphics_kernel.rs:565`) and the ray-tracing
  stage validator. The numeric binding survives as what the descriptor set is actually built
  from; the name is what dispatch resolves against. Nothing propagates through a plugin-ABI
  path — §Packages deleted that surface.
- MODIFIED: the three register ops accept GLSL `source` + `stage` + `entry_point`; `spv_hex`
  survives as the escape hatch. Compilation happens in the engine at kernel construction.
- MODIFIED: `PROTOCOL_VERSION` bumps to 2 (`sdk/streamlib-python-wheel/python/streamlib/_helper.py:53`,
  advertised at `:589` and checked at `:713-716`). The escalate ops change shape, and a
  stale helper must fail its ready handshake with the existing clear error rather than
  mis-parse a changed op. No compatibility branch is added — pre-1.0, and parent and helper
  ship in one wheel, so the only skew this can catch is a stale process or a broken install.
- MODIFIED: `GpuContext` gains the kernel compilation cache the example bridges kept. The key
  covers everything that changes the output — source bytes, stage, entry point, target
  environment, compiler version — never source alone.
- MODIFIED: CPU readback becomes a `GpuContext` method. The engine has no implementation
  today (the example host supplied it); it is built on `record_copy_image_to_buffer` /
  `record_buffer_barrier`, the shape `refill_device_export_staging` already implements.
- MODIFIED: the device-export staging path extends from buffer-backed to texture-backed
  write-back (`device_export_staging.rs:507-545`) via `record_copy_buffer_to_image` plus the
  layout barriers the read direction already records (`:440-469`).
- MODIFIED: `export_pixel_shape_for_texture` (`device_export_staging.rs:64-78`) accepts the
  float formats a kernel output actually uses — it refuses `Rgba16Float` and `Rgba32Float`
  today, which would make the scope unreachable for the common HDR compute output.
- MODIFIED: the helper's importer (`python_helper_process_pixel_exchange.rs:585-698`) gains a
  texture arm over `ConsumerVulkanTexture`, and `acquire_texture` / `import_dma_buf` send
  their escalate ops instead of raising. `import_dma_buf` needs a wire carrying an inbound fd
  child→parent, which does not exist today.
- MODIFIED: both device-write scopes discard on the exception path.
  `publish_pending_device_write` (`python_processor_context.rs:187-205`) gains an
  exception-path arm, and `GpuSurfaceHandle.__exit__` (`:290-306`) stops publishing when the
  block was left by a raise — it still closes. `__exit__` keeps returning `False`; discarding
  the write never suppresses the exception.
- MODIFIED: the wheel links a C++ GLSL compiler (shaderc / glslang), vendored and static —
  §Distribution's portability entry already covers this, so no plan edit is owed there.
- MODIFIED: §Graphics' "cached by source hash" restated as the guarantee it meant, and
  GLOSSARY's **Kernel** cut to the term plus its _Avoid_ list — both made in this session's
  marker window.

## ADDED

- ADDED: `ComputeKernel`, `GraphicsKernel`, `RayTracingKernel` pyclasses with `create_*_kernel`
  on the Full-access capability and `dispatch(bindings=…, push_constants=…, group_count=…)`;
  `KernelDispatchBatch` with `kernel_dispatch_batch()` and `batch.dispatch(kernel, …)`. Every
  one gets its `_engine.pyi` entry in the same PR — the stub is part of done.
- ADDED: `GpuSurfaceHandle.as_device_tensor()` — the blit-out / blit-back scope, house style
  (`__enter__` returns self, `__exit__` annotated `-> Literal[False]`).
- ADDED: a read-one-write-another compute conformance test — a GLSL kernel sampling an input
  surface and writing a second one, proving the lifted binding array end to end.
- ADDED: one test per named-binding error case above, each asserting the message names the
  shader's declared bindings — at the layer the case is representable at. Unknown, missing
  and kind-mismatch are Python-level dispatch tests. **Duplicate is not expressible in a
  Python mapping** and is tested against the escalate binding array directly. Stage-mismatch
  and name-stripped SPIR-V are construction-time, so they assert on `create_*_kernel`, not on
  dispatch.
- ADDED: a discard-on-exception test for **both** scopes — a write raising mid-block leaves
  the engine's surface holding its pre-scope content, the exception propagates unsuppressed,
  and the surface is still usable on the next frame.
- ADDED: a batching test proving N dispatches cost one submission and one fence wait, and an
  identical-kernel-recreation test asserting the second `create_*_kernel` for the same key on
  the same `GpuContext` is a cache hit — counted compiler invocations, never elapsed time.
  Re-creation is free of *compilation*; it may still allocate handles.

## Notes (not tickets)

- The two `polyglot-*` demos are already dead at HEAD — they import `streamlib.adapters.*`
  and call `Runtime::add_processor`, neither of which the current SDK exposes. Reference
  material for what the wire could express, never a target to keep building.
- Every GPU escalate op is Linux-only today and stays so; the platform floor is unchanged.

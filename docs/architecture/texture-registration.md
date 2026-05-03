# `TextureRegistration` — engine-wide per-surface lifecycle state

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).
> Reflects code state as of 2026-05-02 (PR #632, issue #616).
> Cross-process section + scope clarification revised 2026-05-02
> (issue #634) based on Vulkan spec, Khronos guidance, and
> sandboxed-host precedent research.

## What this is

`TextureRegistration` is the **single canonical engine-wide record for
per-surface lifecycle state**, keyed by `surface_id` in
`GpuContext::texture_cache`. Each registration carries the texture
plus the typed mutable fields that producers and consumers both need
to read/write across the surface_id handoff:

- `texture: StreamTexture` — the resource itself.
- `current_layout: AtomicI32` (Linux only — stores
  `streamlib_consumer_rhi::VulkanLayout`) — the last-known Vulkan
  image layout. Producers update on transitions; consumers read for
  barrier source layouts.

Future additive fields go on this record (see [What goes in / what
stays out](#what-goes-in--what-stays-out) below). The shape mirrors
the per-surface state pattern surface adapters already use
(`streamlib-adapter-vulkan::SurfaceState`,
`streamlib-adapter-cuda::SurfaceState`,
`streamlib-adapter-cpu-readback::SurfaceState`) — `TextureRegistration`
is the same shape lifted from adapter-scope to engine-wide scope.

## Why it exists

Before `TextureRegistration` (pre-#632), `GpuContext::texture_cache`
was `HashMap<String, StreamTexture>` — a thin lookup with no
lifecycle metadata. Per-surface state lived in two disjoint places:

- Adapter-scoped `Registry<SurfaceState>` per adapter — visible only
  to adapter code.
- Implicit conventions encoded in producer/consumer code — invisible
  to anyone reading just the engine.

The implicit-convention path broke when a new producer (the OpenGL
adapter, in #484) shipped an output texture whose Vulkan layout
didn't match the convention display assumed (`SHADER_READ_ONLY_OPTIMAL`
from camera). Display's descriptor binding then claimed a layout that
didn't match reality. NVIDIA tolerated the mismatch; AMD/Intel
behavior was unverified; Vulkan validation layers warned. See #616
for the full diagnosis.

The engine-model fix (per
[CLAUDE.md "Engine-wide bugs get fixed at the engine
layer"](../../CLAUDE.md#core-operating-principles--read-first)) was to
make the handoff contract **explicit and typed at the engine
layer**, not patched at the consumer that surfaced the symptom. That's
what `TextureRegistration` is.

## Scope: this record vs. adapter-internal `SurfaceState<P>`

`TextureRegistration` (engine-wide, in `GpuContext::texture_cache`)
and the adapter crates' `SurfaceState<P>` records (in
`streamlib-adapter-vulkan`, `-opengl`, `-cuda`, `-cpu-readback`)
are at **different scopes by deliberate design**, not redundant
maps. They look superficially similar (both keyed by `surface_id`,
both can carry a `current_layout` field) but hold different state
for different consumers:

| Scope | Record | What it carries | Who reads it |
|---|---|---|---|
| Engine-wide | `TextureRegistration` | Same-process handoff state for disjoint producers/consumers (camera → display, OpenGL adapter → display): texture handle, last-known layout. | In-tree pipeline code via `resolve_videoframe_registration`. |
| Adapter-internal | `SurfaceState<P>` | Adapter's acquire/release state machine: `read_holders`, `write_held`, timeline values, framework-specific handles (e.g. EGL image, GL texture id, CUDA external memory mapping). | Only the adapter's own `acquire_*` / `release_*` paths. |

This mirrors Unreal Engine 5's deliberate scope split between
`FRDGSubresourceState` (per-pass handoff state read by the next
consumer) and `FPooledRenderTarget` (allocator-internal pool /
refcount state) — two records, two scopes, zero conflation. To the
best of our current knowledge the same shape applies in Granite
(typed identity record on `Vulkan::Image` plus per-pass transient
state in the render graph) and The-Forge (persistent props on
`Texture`, transitional state on call-site `TextureBarrier`).
Verify against current code at pickup if revisiting.

The Anti-pattern #1 rule below (no parallel `HashMap<surface_id, …>`)
applies **within scope**: don't create a second engine-wide map
alongside `texture_cache`, and don't create a second adapter-
internal map alongside an adapter's `Registry`. Different-scope
records that share a key but hold disjoint field sets and serve
disjoint consumers are **not** the failure mode the rule exists
to prevent.

## How it works

```
producer:                  consumer:
  register_texture_with_      resolve_videoframe_
    layout(id, tex, L)          registration(frame)
       │                            │
       ▼                            ▼
  ┌─────────────────────────────────────────┐
  │ GpuContext::texture_cache               │
  │   HashMap<surface_id, Arc<TexReg>>      │
  │     ├── texture: StreamTexture          │
  │     └── current_layout: AtomicI32       │
  └─────────────────────────────────────────┘
       ▲                            │
       │                            ▼
  update_layout(L′) after     barrier(old=current_layout,
    a producer transition       new=target_layout);
                              update_layout(target_layout);
```

The `Arc<TextureRegistration>` is shared across all holders in-process
— no IPC, no schema changes. `current_layout` reads use
`Ordering::Acquire`; writes use `Ordering::Release`. Multi-consumer
races are tolerated (see [Race model](#race-model)).

## What goes in / what stays out

### In: state both producers and consumers care about for handoff

A field belongs in `TextureRegistration` when **all three** are true:

1. The field's value is set by the producer and read by the
   consumer (or vice-versa) — the handoff is the contract this
   record encodes.
2. The field has a stable identity tied to the surface_id (lives
   from registration through unregistration) — it's not transient
   per-frame data.
3. Both producers and consumers run on the same in-process address
   space, OR the cross-process IPC layer round-trips the field
   (see [Cross-process limitation](#cross-process-limitation)).

Examples that fit:

- ✓ `current_layout` — present today.
- ✓ `last_writer_id` — debugging "where did this surface come from?";
  set on registration, read by anyone tracing the pipeline.
- ✓ `last_written_frame_index: AtomicU64` — staleness detection;
  producer increments per write, consumer compares to its expected
  frame.
- ✓ `format`, `width`, `height` — could be hoisted from `StreamTexture`
  for cheaper validation; arguably already covered by `texture`.
- ✓ Exportable timeline-semaphore handle — for consumers that need to
  GPU-wait without a side-channel.

### Out: state that doesn't fit any one of the criteria

- ✗ **Adapter-internal state** (e.g., the OpenGL adapter's cached
  `EGLImage` + GL texture id, or the cuda adapter's `cudaExternalMemory`
  handle). Stays in the adapter's own `SurfaceState`. Engine doesn't
  need to see it; other consumers don't either.
- ✗ **Per-frame transient state** (the timestamp of *this* frame, the
  encoder's `is_keyframe` bit, etc.). These belong on the `Videoframe`
  IPC message, not on the surface registration. The registration
  outlives any single frame.
- ✗ **RDG-style declared-usage hints** ("I'll read this as a SAMPLED_READ
  in pass N+1"). That's a different layer entirely — see
  [Boundary with RDG](#boundary-with-rdg-631) below. Adding declared
  usage here without the graph compiler to interpret it is just dead
  metadata.
- ✗ **Cross-process-only state without a same-process consumer.** Use
  the surface-share daemon's own state or extend its IPC schema. The
  `texture_cache` is for in-process consumers reaching textures via
  `resolve_videoframe_registration` Path 1.

When you find yourself wanting to add a field that doesn't fit cleanly,
**stop and ask**:

- "Is this really per-surface, or is it per-frame?" → if per-frame, use
  `Videoframe` IPC fields.
- "Does the engine need to see this, or only the adapter?" → if only
  the adapter, keep it in `SurfaceState`.
- "Am I tracking declared usage?" → if so, that's RDG territory (#631).

## Producer rules

1. **Declare your post-publish layout at registration time.**
   ```rust
   gpu.register_texture_with_layout(
       &surface_id,
       texture,
       VulkanLayout::SHADER_READ_ONLY_OPTIMAL,  // or whatever you actually leave it in
   );
   ```
   The "post-publish" layout is the layout the texture is in **at the
   moment any consumer dereferences the surface_id**. For producers
   that write IPC frames, this is the layout immediately after the
   transition that precedes the IPC write.

2. **Use the back-compat shim only when the layout is genuinely
   `UNDEFINED`.**
   ```rust
   gpu.register_texture(&id, texture);  // defaults to UNDEFINED
   ```
   This is correct for a freshly-allocated texture that no one has
   touched (e.g. `acquire_output_texture`). It is **not** a "I don't
   know, just pick something" escape hatch. If you actually do know
   the layout, declare it. If you genuinely don't, that's a code smell
   — the texture should have a knowable post-allocation state.

3. **Update on mid-pipeline transitions.**
   ```rust
   // After issuing a barrier that moves the texture to a new layout:
   reg.update_layout(VulkanLayout::TRANSFER_SRC_OPTIMAL);
   ```
   Producers that transition the texture multiple times during a frame
   only need to update the field after the *last* transition before the
   IPC publish — intermediate layouts aren't observed by consumers.

4. **Don't lie.** A registration that claims `SHADER_READ_ONLY_OPTIMAL`
   when the Vulkan tracker is in `GENERAL` re-creates the exact bug
   `TextureRegistration` exists to fix. If your producer doesn't
   transition the Vulkan tracker (e.g., the OpenGL adapter, which
   writes via GL without issuing Vulkan barriers), declare what's
   actually true: `UNDEFINED` initially, then whatever the last
   consumer's barrier left it in (read it back from the registration
   if the producer needs to reason about it).

## Consumer rules

1. **Resolve the registration, not just the texture.**
   ```rust
   let reg = gpu.resolve_videoframe_registration(&frame)?;
   let texture = reg.texture();
   let current = reg.current_layout();
   ```
   `resolve_videoframe_texture` is back-compat for callers that don't
   need the metadata — but if you're issuing a barrier, use the
   registration form.

2. **Barrier from `current_layout` to your target.**
   ```rust
   if current != VulkanLayout::SHADER_READ_ONLY_OPTIMAL {
       device.cmd_pipeline_barrier2(cmd, &dep_info(/* old=current, new=target */));
       reg.update_layout(VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
   }
   ```
   Skip the barrier when source equals target — Vulkan permits no-op
   transitions but they emit validation warnings on some drivers.

3. **Update after the barrier records.** The
   `update_layout` call should be the source-of-record for the next
   reader; race tolerance is documented but not magic. If you skip the
   update after a real transition, the next consumer will issue a
   barrier from the wrong source layout.

4. **Don't cache `current_layout` across frames.** A future frame may
   have a different producer, or the layout may have been changed by
   another consumer. Read it fresh per frame.

## Anti-patterns

These are the failure modes the engine-model rule exists to prevent.
Each was either tried and rejected, or is the foreseeable workaround
that future agents would attempt without this doc.

1. **Parallel `HashMap<surface_id, FooState>` *within scope*.** If
   you find yourself wanting to track new engine-wide per-surface
   metadata in a separate HashMap alongside `texture_cache`, stop.
   Extend `TextureRegistration`. The whole point of the engine-wide
   cache is that there's *one* keyed registry at engine scope;
   multiple parallel engine-wide ones recreate the implicit-
   convention problem one layer up. (See
   [Scope](#scope-this-record-vs-adapter-internal-surfacestatep)
   above — adapter-internal `SurfaceState<P>` lives at a different
   scope and does not violate this rule.)

2. **Descriptor-side claims that don't match registration.** Display's
   pre-#632 bug was claiming `SHADER_READ_ONLY_OPTIMAL` in a
   `VkDescriptorImageInfo::imageLayout` field while the actual texture
   was in `GENERAL`. The fix is to barrier first, not to claim
   something else and hope the driver tolerates it. Never use the
   descriptor's `imageLayout` field as a workaround for an unknown
   source layout — barrier the texture into the layout you're going
   to claim.

3. **Side-channel "I know better" code paths.** If a consumer thinks
   it knows the source layout better than the registration claims,
   the answer is to fix the producer's declaration, not to ignore the
   registration and barrier from a hardcoded layout. The registration
   is the source of truth; if it's wrong, fix it at the source.

4. **Reaching through private fields to bypass the API.** No
   `_lib._texture_cache._inner` reach-through code paths. If you need
   the registration's internals, add a public method on
   `TextureRegistration` itself.

5. **"Just unconditionally barrier from `GENERAL`"** as a way to dodge
   tracking. This was considered for #616 (Option A in the design
   discussion) and rejected: it trades one validation warning class
   for another (camera-display now warns instead of AvatarCharacter).
   The right answer is typed tracking.

6. **Adding declared-usage hints onto `TextureRegistration`.** "I'll
   need this as SAMPLED_READ next" is RDG-shape information; without
   the graph compiler to read it, it's dead metadata that producers
   and consumers can both lie about. See next section.

## Boundary with RDG (#631)

`TextureRegistration` is **typed state that consumers manually read +
typed transitions consumers manually issue**. RDG-style automatic
barrier inference would be **consumers declaratively name their
access type and the engine derives transitions** from a graph of
read/write edges. They are different layers:

| Layer | What consumers do | What the engine does |
|---|---|---|
| Direct RHI (today) | Issue explicit `cmd_pipeline_barrier2` calls | Nothing |
| `TextureRegistration` (this doc) | Read `current_layout`, issue barrier, update | Track typed state per surface_id |
| RDG (#631 — future) | Declare access type via pass parameters | Build graph, derive barriers, schedule async-compute, alias memory |

If you find yourself wanting consumers to declare "I want this as a
SAMPLED_READ" *without* writing a barrier, that's an RDG-shape need.
Don't bolt declared usage onto `TextureRegistration` — escalate to
#631. The engine doesn't have a graph layer to interpret the
declaration, so the field would be dead metadata that masks rather
than solves the problem.

The substrate `TextureRegistration` provides (typed per-surface state
keyed by stable id) is exactly what an eventual RDG layer would build
on top of — but RDG is a new layer above this one, not a replacement.

## Race model

`current_layout` is `AtomicI32` with `Acquire` reads and `Release`
writes. The Arc itself is `Send + Sync`. Consequences:

- **Single producer + single consumer** (the dominant pattern today —
  one camera writes, one display reads): clean, layout claims always
  match GPU reality after the consumer's barrier records.
- **Single producer + multiple consumers in the same frame** (e.g.
  display + encoder both consuming a camera ring texture): the second
  consumer's `current_layout` read may see the first consumer's
  updated value, in which case it issues a no-op barrier; or it may
  race and both consumers issue barriers from the same source layout,
  in which case the queue mutex serializes the submissions and the
  GPU work is correct. **Documented as race-tolerant**, not race-free.
- **Multiple producers in the same frame**: not a supported pattern.
  `register_texture_with_layout` overwrites the registration; if a
  producer needs to share a surface with another producer it has to
  coordinate out-of-band.

The race model is good enough today because streamlib pipelines are
predominantly single-consumer-per-frame. If a future pipeline genuinely
needs multi-consumer coordination beyond what queue serialization
provides, escalate to RDG (#631) — that's the layer where coordination
is the engine's job, not the consumer's.

## Cross-process limitation

`TextureRegistration` solves the same-process handoff. **It does not
yet propagate to cross-process / cross-language consumers.**

Today, when a subprocess producer registers a texture via
`surface-share`, the host consumer's `resolve_videoframe_registration`
hits Path 2 (cross-process import) which synthesizes a fresh
`Arc<TextureRegistration>` with `current_layout = UNDEFINED`. The
host consumer barriers from UNDEFINED → its target, which is correct
but conservative (Vulkan spec permits content discard on
UNDEFINED → X, though NVIDIA preserves in practice).

The wire-format gap is mechanical, not architectural. Two extension
shapes are possible (and probably both belong long-term):

1. **Per-surface schema extension (`surface-share` IPC).** Add
   `current_layout` to the `register` / `check_in` / `lookup`
   messages. Subprocess producer declares layout at registration; host
   consumer reads it. Handles "static layout, never changes" — matches
   today's same-process producer pattern.
2. **Per-frame schema extension (`Videoframe`).** Add `texture_layout`
   (optional) to the `Videoframe` IPC message. Producers can vary
   layout per frame. Bigger lift — `Videoframe` is a polyglot schema,
   so all three runtimes (Rust + Python + Deno) ship together per
   [`.claude/workflows/polyglot.md`](../../.claude/workflows/polyglot.md).

The "right" shape is probably both — registration carries a default,
`Videoframe` overrides per-frame. Tracked as #633.

> ~~There's also a quieter constraint: cdylibs depend on
> `streamlib-consumer-rhi`, NOT the full `streamlib`, so they can't
> construct `TextureRegistration` directly (it lives in
> `streamlib::core::context`). To give subprocess producers the
> same typed contract, the registration record itself probably
> needs to live in `consumer-rhi`. Separate ticket.~~ —
> **Superseded 2026-05-02** (issue #634, closed without code
> change). The speculation that subprocess producers need to
> construct `TextureRegistration` themselves doesn't survive the
> spec evidence: layouts across DMA-BUF imports into a second
> `VkDevice` are independent state machines by Vulkan
> construction, not stale mirrors of the host's record. The
> architecturally correct cross-process work is the IPC schema
> lift (#633), not a type relocation. See [Why no sandbox-side
> mirror](#why-no-sandbox-side-mirror) below.

### Why no sandbox-side mirror

Three independent lines of evidence converged in the #634
research and pointed the same way:

1. **Vulkan spec.** `VkImageCreateInfo::initialLayout` must be
   `UNDEFINED` or `PREINITIALIZED`. There is no "import this
   in layout L" form. Every freshly-created `VkImage` in the
   consumer process — even one bound to imported memory — starts
   at its declared `initialLayout`. The consumer's layout state
   is its own state machine, **independent of the producer's by
   spec construction**, not a stale view of it.

   Khronos's `VK_EXT_external_memory_acquire_unmodified`
   proposal states this directly: *"The solution should not
   require the implementation to internally track the
   `VkImageLayout` of external images, as such tracking can be
   complex to implement and cause performance overhead."*
   Cross-process layout is communicated by **application
   protocol** (release/acquire barriers via
   `VK_QUEUE_FAMILY_EXTERNAL`, or in our case the IPC schema),
   not by shared mutable state.

2. **Sandboxed-host architecture precedent.** Every closely-
   analogous system mirrors only **immutable descriptor metadata**
   (size, format, usage) on the sandbox side; mutable lifecycle
   state stays server-side. To the best of our current knowledge:

   - Dawn Wire client `Texture.h` caches descriptor shape only;
     no `current_layout`, no barrier scope.
   - Chromium SharedImage hands renderers an opaque 16-byte
     `gpu::Mailbox` with **zero embedded metadata**; the backing
     lives in the GPU process.
   - wgpu-core's `TextureTracker` / `TextureUsageScope` live in
     the hub (server); user-facing `wgpu` holds only
     `Arc<Texture>` resource handles.
   - WebGPU spec §3.4 explicitly splits "content timeline"
     (client-visible, immutable descriptor) from "device
     timeline" (server-side, mutable, accessed asynchronously
     via `GPUError`).

   Verify the patterns against current upstream source if
   revisiting — these are pinned to the snapshot evaluated in
   the #634 research.

3. **In-tree evidence.** No subprocess code constructs
   `TextureRegistration` today. Cdylibs use
   `HostSurfaceRegistration<P>` (an adapter-scope record at the
   adapter crate, generic over `DevicePrivilege`) plus the
   `VulkanLayout` enum (already in `streamlib-consumer-rhi`).
   The "subprocess needs typed-contract for layout" concern is
   already covered at the adapter scope, where it belongs per
   the [Scope](#scope-this-record-vs-adapter-internal-surfacestatep)
   section.

The architecturally correct cross-process work is the **IPC
schema lift (#633)**: the producer's *published layout* travels
in the surface-share / `Videoframe` schema as a typed protocol
field; the host consumer reads it once at acquire time and
barriers from there. No mirror, no shared mutable record across
the boundary.

**Until #633 lands, cross-process consumers should keep
barriering defensively from `UNDEFINED`** — don't paper over the
gap consumer-side.

## Tests

When a new field lands on `TextureRegistration`:

1. **Unit test in `texture_registration.rs::tests`**: exercise
   round-trip (set → read), concurrent updates from N threads (no
   torn values), and any field-specific invariants.
2. **Unit test in `gpu_context.rs::tests`**: exercise the resolve
   path — register with the new field, resolve via
   `resolve_videoframe_registration`, assert visibility.
3. **Mentally revert the field's update logic** — does the test still
   pass? If yes, the test is feel-good and doesn't lock the contract.
   Strengthen it: a test that doesn't fail when the impl is reverted
   isn't locking anything.
4. **E2E with `VK_LOADER_LAYERS_ENABLE=*validation*`** for any field
   that affects GPU behavior. The unit tests lock the data structure;
   validation-layer E2E locks the actual Vulkan-side correctness.

## Reference

- **Implementation**: `libs/streamlib/src/core/context/texture_registration.rs`,
  `GpuContext::register_texture_with_layout` /
  `GpuContext::resolve_videoframe_registration` in
  `libs/streamlib/src/core/context/gpu_context.rs`.
- **First consumer**: `LinuxDisplayProcessor::render_frame` in
  `libs/streamlib/src/linux/processors/display.rs`.
- **First adapter-output producer**: `register_opengl_output_surface`
  in `examples/camera-python-display/src/linux.rs`.
- **First in-tree producer**: `LinuxCameraProcessor` in
  `libs/streamlib/src/linux/processors/camera.rs`.
- **Adapter-scope sibling**: `streamlib-adapter-vulkan::SurfaceState`
  in `libs/streamlib-adapter-vulkan/src/state.rs:48` (and the
  same-shape cuda + cpu-readback adapter state structs). These are
  at adapter scope, **not** parallel maps to `texture_cache` — see
  the [Scope](#scope-this-record-vs-adapter-internal-surfacestatep)
  section.
- **PR**: #632.
- **Issue**: #616.
- **Closed without code change**: #634 (lift `TextureRegistration`
  into `streamlib-consumer-rhi`) — see [Why no sandbox-side
  mirror](#why-no-sandbox-side-mirror) for the spec + precedent
  evidence.
- **Cross-process follow-up**: #633 (IPC schema lift for
  producer-published layout).
- **Future research**: #631 (RDG / automatic barrier inference).
- **External references** (consulted during the #634 research):
  - [Khronos `VK_EXT_external_memory_acquire_unmodified` proposal](https://docs.vulkan.org/features/latest/features/proposals/VK_EXT_external_memory_acquire_unmodified.html)
  - [Vulkan synchronization & queue-transfer chapter](https://docs.vulkan.org/spec/latest/chapters/synchronization.html)
  - [Dawn wire client Texture.h](https://dawn.googlesource.com/dawn/+/refs/heads/main/src/dawn/wire/client/Texture.h)
  - [Chromium SharedImageBacking](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/gpu/command_buffer/service/shared_image/shared_image_backing.h)
  - [wgpu-core track/texture.rs](https://github.com/gfx-rs/wgpu/blob/trunk/wgpu-core/src/track/texture.rs)
  - [WebGPU spec §3.4 (Programming Model)](https://www.w3.org/TR/webgpu/)
  - [UE5 `FRDGSubresourceState`](https://dev.epicgames.com/documentation/en-us/unreal-engine/API/Runtime/RenderCore/FRDGSubresourceState/IsUsedBy)

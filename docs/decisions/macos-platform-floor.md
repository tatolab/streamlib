# macOS as a second platform floor: one Vulkan RHI, Apple SDKs at the edges

Rationale for the `[macos-platform-floor]` entries in `docs/plan/ARCHITECTURE.md` §Product,
§Graphics, §Media I/O, §Processor model, §Distribution and §Networking, decided 2026-09-19.

## Trigger

Read this before adding a per-platform GPU backend, before reaching for an Apple framework where a
Vulkan path exists, before proposing an `.app` bundle or a launchd service for anything, and before
assuming a Linux kernel interface has a one-to-one Apple peer. Also read it before widening the
macOS floor to Intel, or before treating macOS as a best-effort target.

## Decision

> Apple Silicon is a **supported** platform floor beside Linux + NVIDIA — not a developer-machine
> convenience. §Product's sentence names both, CI gates macOS on every PR, the macOS wheel is in the
> release closure, and a macOS-only regression blocks a release exactly as a Linux one does.

> **Vulkan is the one RHI on every supported platform.** MoltenVK is the macOS driver, reached
> through the same `HostVulkanDevice`. There is no second backend and no per-platform RHI. The
> pre-pivot `src/metal/` tree and the `backend-metal` feature are deleted.

> **Apple frameworks appear only at the edges Vulkan cannot reach**, each meeting Vulkan at a defined
> handoff: AVFoundation for capture (IOSurface → ~~`VkImage`~~ storage buffer),
> AppKit/`CAMetalLayer` for present (→ `VkSurfaceKHR`), IOSurface plus Mach ports for cross-process
> frames, `MTLSharedEvent` for the cross-process timeline, `mach_absolute_time` for the clock.

> ~~AVFoundation for capture (IOSurface → `VkImage`)~~ — Superseded 2026-09-21 by the owner, on
> #2359's measurement. MoltenVK refuses every CoreVideo 4:2:0 surface as a multi-planar `VkImage`
> through v1.4.2 and on `main`: its import check compares the surface's top-level element — one
> byte in 1×1, which is how CoreVideo describes it — against the whole six-byte 2×2 block, and a
> single-plane import reaches only the luma plane. The frame's memory is imported as a storage
> buffer through `VK_EXT_external_memory_host` instead — measured on MoltenVK 1.4.2, which the wheel
> therefore carries at 1.4.1 or later — and read by the NV12 kernel a V4L2 DMA-BUF import feeds.

> **The artifact is the wheel, identical to Linux** — `aarch64-apple-darwin` only, no Intel, no
> Rosetta. No `.app` bundle, no launchd service, no installer, under any justification. macOS
> security prompts are part of the experience; needing a *bundle* to obtain them is not.

## Why

The evidence is `docs/research/2026-09-19-macos-camera-display-revival.md`, measured on an M1 Max.

**Vulkan-everywhere is the cheap path, not the compromise.** MoltenVK creates a device accepting
every feature the engine pushes unconditionally. `streamlib-consumer-rhi` compiles on macOS with
zero code changes once its `cfg` gates widen. The engine's 65 compile errors under a forced Vulkan
backend contain **no** MoltenVK capability gap — they are an unapplied edition-2024 migration, stale
`core/rhi` facades pointing at the Metal tree, host-gated shader compilation, and `cfg(linux)` on
Vulkan code with nothing Linux in it. Against that, a revived Metal backend is 2 488 LOC standing in
for 89 332 LOC of Vulkan RHI, and every kernel, buffer, timeline and present primitive would be
written twice forever.

**The Apple edges are real, and each was proven rather than assumed.** ~~IOSurface imports as a
`VkImage` single-plane and biplanar~~ — superseded 2026-09-21 by #2359's measurement: a single-plane
surface imports as a `VkImage`, but a CoreVideo 4:2:0 surface — every camera frame — imports only
its luma plane as one, so camera frames import as a storage buffer (see the annotated handoff line
above); `CAMetalLayer` yields a working swapchain; a spawned Python
child edits a 1080p GPU frame through a numpy view over a Mach-passed IOSurface at 698 fps, verified
pixel-exact; a Vulkan timeline semaphore crosses to that child at 120 µs per round trip.

**The transport is raw Mach, not XPC, and that is a finding rather than a preference.** An XPC
endpoint cannot be serialised to bytes, so it can only travel over an XPC channel that already
exists — circular for a child that does not yet exist. `XPC_CONNECTION_MACH_SERVICE_LISTENER` and the
macOS 14+ `xpc_listener_create` both refuse a dynamically registered name. Raw Mach reaches a spawned
child service-lessly, and hands back the public audit trailer and dead-name notifications as well.

**Scope is set by the install promise.** A supported floor is what makes capture, present,
cross-process frames and the wheel part of the milestone; a developer-machine floor would have
stopped at a window opening. The owner's ruling — *"I want people to be able to build and run it,
not just develop on it"* — is what the nine tickets trace to.

## Rejected alternatives

- **Revive the Metal RHI as a second backend.** A parallel abstraction is the default-wrong move
  under §Structure, and the measurements remove its justification: MoltenVK clears the bar. Its
  *interop* knowledge — the AVFoundation serialisation constraints recorded in
  `metal/rhi/texture_cache.rs` and `pixel_buffer_pool.rs` — is mined before the tree is deleted.
- **Ship an `.app` bundle.** It would solve two real problems: the terminal-permission edge case
  where a host has never prompted, and hosting a launchd-backed XPC service. Rejected by the owner:
  the distribution promise is `pip install` parity with Linux, and a bundle breaks it. The
  permission edge case is therefore accepted with a good diagnostic instead of a fallback, and the
  shared-event transfer takes the public-coder route with host-side ordering behind it.
- **XPC as the cross-process transport.** Not available service-lessly to a spawned child; see Why.
- **Intel macOS or a universal2 wheel.** Rosetta offers no usable Metal path, the Linux wheel is
  x86_64-only anyway, and every measurement is Apple Silicon. A second architecture doubles the
  macOS CI cost for hardware the evidence does not cover.
- **Pixels over the wire to Python instead of a handle.** Would sidestep cross-process GPU sharing
  entirely, and contradicts §Language SDKs' DECIDED contract that frames travel as handles and
  surface ids. Not taken; the handle path was proven instead.
- **Restructuring the helper spawn to `posix_spawn` inside this change.** The capability-secure
  rendezvous needs it, and `POSIX_SPAWN_CLOEXEC_DEFAULT` would be stronger than today's descriptor
  sweep — but `python_helper_process_spawn_host.rs` is one shared file, so the change alters the
  spawn path on **both** platforms. It becomes its own change with its own Linux proof rather than a
  rider on this one.

## Consequences

- The Metal tree, the `backend-metal` feature and four dead Apple files are deleted; eight shared
  files have their `cfg` expressions simplified, which is a no-op on Linux because `backend-metal`
  was never in `default`. Two of the eight are the SDK's (`streamlib-sdk`'s manifest forwards the
  feature and its `sdk::engine` module reads it); two of the four Apple files are Metal-RHI call
  sites that cannot outlive the tree.
- Capture stops being V4L2-only. A video device backend seam is created — modelled on
  `AudioDeviceBackend` and extending it rather than paralleling it — and the V4L2 implementation
  moves behind it unchanged. This is the largest single piece of engine work in the milestone.
- **CoreVideo's `_pixelFormatDictionaryInit` is not thread-safe**, so `CVMetalTextureCacheCreate`
  and `CVPixelBufferPoolCreate` crash when either races `AVCaptureDeviceInput` initialisation. The
  deleted Metal tree serialised both by dispatching them to the main thread. Whatever the Apple
  capture arm allocates through CoreVideo inherits that constraint; recorded here because the tree
  that recorded it is gone and no capture code exists yet to carry it.
- The engine's one event pump gains an Apple arm that runs on the process's first thread, which is
  the thread `rt.run()` blocks.
- A cross-process GPU wait is never unbounded: an unsatisfied wait past roughly five seconds loses
  the Metal device unrecoverably, so a stalled helper must never be able to take the engine's device
  down with it.
- The wheel carries the Vulkan loader and MoltenVK on macOS, because a stock Mac has no Vulkan
  driver. No notarisation and no signing identity are needed — nothing pip delivers is quarantined —
  but any post-link rewrite of a shipped binary is re-signed in the same step.
- CI gains a macOS lane. `xtask lint-logging` must gain a macOS `cfg` pass or the Apple tree stays
  unlinted, which is how it rotted unnoticed for seven months.
- **One change is not additive.** The camera carrying the device's capture instant rather than the
  publication instant alters Linux behaviour, and is gated on a Linux regression check before it
  merges.
- The mesh gains an Apple host identity, reversing the clause that macOS has none. Zenoh's
  transport already works there; the gap was the identity layer above it, which turned the
  duplicate-name refusal into a dev-loop papercut — scouting is on by default, so a crashed
  `streamlib dev` could not restart under its own name.
- KosmicKrisp — MIT, fully Vulkan 1.3 conformant, Apple Silicon only — becomes the migration target
  worth evaluating once a macOS 26 floor is acceptable. Bundling the Vulkan *loader* rather than
  linking MoltenVK directly is what keeps that path open.

## Capability parity — decided 2026-09-22

Read this before giving a helper process its own Metal code, before adding a staging copy on
macOS because Linux has one, and before deciding a Linux-only capability "has no macOS
equivalent". The floor above reaches the MVP sentence; `docs/plan/changes/macos-capability-
parity.md` carries everything else the Linux floor offers, and the owner's ruling is the
trigger: *"streamlib on osx requires the full capabilities and feature set"* — the first delta's
"Not in scope" block was a session's reading of the MVP sentence, not the intent.

> **The helper's importer stays Vulkan.** `streamlib-consumer-rhi` gains a MoltenVK arm — an
> IOSurface imported as a `VkBuffer` or `VkImage`, a Metal shared event imported as a
> `VkSemaphore` — and Metal appears only as exported handles at the boundary: the shared-event
> port, the `MTLBuffer` behind an imported buffer for the DLPack capsule. A Metal-direct shim in
> the wheel was rejected as a second system beside the one that exists, even though it would
> have saved the per-helper device bring-up (~0.5 s cold, 26–70 ms warm), which is inside the
> startup budget the plan already tests.

> **Unified memory removes the staging, not the ordering.** An IOSurface-backed texture is
> linear and host-visible, so on macOS the CPU door and the device tensor over a texture are
> the surface itself, ordered on its shared-event timeline. The device tensor is a `kDLMetal`
> capsule over a no-copy `MTLBuffer` on the frame's own IOSurface — measured on torch 2.14 MPS
> and MLX 0.32.2 with write-through — so the CUDA peer on macOS is zero copies where Linux is
> one blit. What this narrows is stated in the change: the texture door publishes per store, as
> the pixel-buffer door already does everywhere.

> **Absent tiers are typed and closed.** Ray tracing (no `VK_KHR_ray_tracing_pipeline` under
> MoltenVK), `VirtualCameraSink` (a Camera Extension needs a bundle; the DAL plug-in stopped
> loading in macOS 14.1), the CUDA Array Interface, the fd-shaped raw handles (peer:
> `export_iosurface`), and the OpenGL adapter (its seam is EGL and DMA-BUF; OpenGL is deprecated
> on macOS; a native consumer takes the Vulkan adapter). Each refuses by name before a frame
> flows, and the same Python test suite proves parity on both CI lanes with a skip marker that
> may name only this list.

Rejected alongside: a CGL-and-IOSurface arm of the OpenGL adapter (Zink or any GL on macOS
would still need one, making it the Vulkan adapter behind a translation layer); one milestone
for floor and parity together (the floor delta is over the line cap and is a shippable
increment on its own — parity is its own milestone).

# macos-capability-parity

The second half of the Apple Silicon floor. `macos-platform-floor` reaches the MVP sentence —
camera, window, one scaffolded Python effect. This delta carries every other capability the
Linux floor offers to macOS: the helper process's whole GPU surface (textures, kernels, device
tensors, raw handles, windows), the clocks and wakeups, parent-death teardown, audio, video
codecs, the read-out doors, the in-process adapters, and the proof that keeps the two floors
honest. Owner, 2026-09-22: *"streamlib on osx requires the full capabilities and feature set"* —
the "Not in scope" block of the first delta was a session's reading of the MVP sentence, not the
owner's intent. The architecture does not change: one Vulkan RHI through MoltenVK, Apple
frameworks only at the edges Vulkan cannot reach.

**Scale gate — this delta plus a section on the floor ADR.** It touches the RHI (the consumer
RHI's second driver, a third importability flavour), the IPC wire (timeline and texture sidecars
on the Mach channel), the processor model (helper lifetime, wakeups), and the Python public
contract (the device tensor's macOS peer, an IOSurface raw handle). `docs/decisions/macos-
platform-floor.md` gains a "Capability parity" section rather than a second ADR: the rationale
extends the floor's, and the rejected alternatives are the same family.

**Precondition.** Every entry this delta touches is DECIDED or was MODIFIED by
`macos-platform-floor`, which stays active beside this one; both fold at `/ship-change`. The
OPEN entries in the sections touched are untouched: zero-copy per-frame foreign consumption
(`ARCHITECTURE.md:246-250`), the vertex/index and storage-buffer bindings from Python
(`:965-971`), depth attachments and MSAA (`:1142`), the extension in-process carve-out (`:194`),
extra execution flavors (`:947`).

**Verified against the tree and the hardware, 2026-09-22.** Evidence beyond the 2026-09-19 memo
was measured on the M1 Max in three probes left under `/tmp/iosurf-nocopy/` and
`/tmp/torchprobe/`; every "measured" below is from there or from the memo.

- Every GPU capability a Python processor has on Linux answers one refusal on macOS — *"its
  helper process was started without a surface-share channel"* (`python_processor_context/mod.rs:
  95-108`) — because the spawn host passes the surface channel only on Linux
  (`python_helper_process_spawn_host.rs:626-629`) and the helper's exchange client has no
  macOS arm (`python_helper_process_pixel_exchange.rs` as measured then, since split into
  `python_helper_process_pixel_exchange/`: ~180 Linux gates, zero fallback arms).
- The wheel crate is excluded from the macOS CI lane (`.github/workflows/test.yml:786-797`).
  The lib compiles on macOS with two one-line fixes; only the test target's `libc::pipe2`
  (`:1317`) does not. `stubtest` never runs on macOS, so three `#[pymethods]` the stub names
  are absent from the macOS binary unnoticed (`export_dma_buf`, `export_opaque_fd`,
  `import_dma_buf`).
- `GpuContext`'s kernel and buffer surface is `cfg(linux)` with nothing Linux in it —
  `create_compute_kernel` (`gpu_context.rs:1921`), `create_graphics_kernel` (`:2221`),
  `acquire_uniform_buffer` (`:1805`) and the rest of the 14-method list — while the Vulkan
  types beneath them already compile on macOS. The fifteen Linux-only escalate ops
  (each family's `not_linux.rs` under `subprocess_escalate/`) refuse only because of that.
- `ConsumerVulkanDevice::new` hard-requires the fd and DMA-BUF extensions unconditionally
  (`consumer_vulkan_device.rs:199-215`); the crate builds on macOS and fails at runtime.
- An IOSurface-backed `VkImage` is an allocation flavour, bound at `vkCreateImage`
  (`MVKImage.mm:1289-1292`); it is forced `MTLStorageModeShared` (`:1099`) and takes storage and
  render-target usage (measured). MoltenVK's own export-create path sets the banned
  `kIOSurfaceIsGlobal` (`:1067`), so the engine always allocates the surface itself.
- DLPack `kDLMetal` is live: torch 2.14 MPS and MLX 0.32.2 both import a hand-built `kDLMetal`
  capsule over a no-copy `MTLBuffer` wrapping an IOSurface, and a write through either lands in
  the surface (measured). The `MTLBuffer` is exportable from an imported `VkBuffer` through
  `vkExportMetalObjectsEXT`.
- The audio seam is platform-neutral with no Apple arm (`audio_device_backend.rs:270-273`
  returns no backends); `MicrophoneSource` and `SpeakerSink` run on the silent null backend.
  `apple/audio_clock.rs` is a timer, not a device.
- No kqueue anywhere in the tree. macOS processors run the non-reactive sleep loop
  (`thread_runner.rs:318`); `MonotonicTimer` refuses (`python_monotonic_timer.rs:55-60`); the
  parent-death signal is absent. The wheel's clock on Darwin is `CLOCK_MONOTONIC`, which keeps
  counting through sleep; the engine's `MediaClock` is `mach_absolute_time`, which does not.
- A CoreMediaIO Camera Extension needs a bundled, entitled, notarised app; the DAL plug-in
  stopped loading in macOS 14.1. No macOS virtual camera exists under the bundle ruling.

---

## §Product — the MVP sentence

- MODIFIED: the two floors are one product surface. A Python processor written against the
  wheel's public surface runs on both floors, and the places where it cannot are a short closed
  list that refuses by name before a frame flows. The scaffold and the examples use only the
  portable surface.
- ADDED: **the portability guarantee is mechanical, never prose.** One `_engine.pyi`, gated by
  `stubtest` against both binaries in CI, so no class or method exists on one floor and not the
  other. Every capability absent on a floor refuses by name at `rt.add()` or `setup()`, naming
  the platform and the reason, never mid-frame. The same Python test suite runs on both CI
  lanes; a platform skip marker may name only the closed list below, so parity is proven by the
  same tests green on both rather than asserted. The closed list, after this delta: ray-tracing
  kernels (no `VK_KHR_ray_tracing_pipeline` under MoltenVK), `VirtualCameraSink`, the CUDA Array
  Interface, and the two fd-shaped raw handles — each with a named peer or a named reason.

## §Packages & extension model

- MODIFIED: the interop contract's tensor side names both drivers. A graph frame's natural side
  is the device on both floors: `kDLCUDA` over an OPAQUE_FD staging on Linux, **`kDLMetal` over
  a no-copy `MTLBuffer` on the frame's own IOSurface on macOS** — zero copies and no staging,
  because unified memory makes the surface's bytes the device's bytes. `torch.from_dlpack`
  yields `cuda` there and `mps` here; `mx.from_dlpack` consumes the same capsule. Floors stated
  where the capsule is minted: ~~torch ≥ 2.12~~ torch ≥ 2.10 by source, measured on 2.14 *(the
  2.12 had no measurement behind it; `portable-gpu-interop`)*, MLX ≥ 0.32. The CUDA
  Array Interface is Linux by nature and is not offered on macOS.
- MODIFIED: raw-handle export gains the IOSurface flavour. `export_iosurface` on the Full
  surface returns a typed object carrying a Mach send right to the allocation's IOSurface plus
  the same allocation-stable shape `export_opaque_fd` carries; it is gated, owned, and bounded
  exactly as the fd flavours are. `export_dma_buf` and `export_opaque_fd` exist on macOS and
  refuse by name pointing at it; `export_iosurface` exists on Linux and refuses by name pointing
  back. A raw handle is platform-shaped by nature; choosing one is visible in the code.

## §Processor model & scheduling

- ADDED: **the helper's importer is the consumer RHI on both floors, and it stays Vulkan.**
  `streamlib-consumer-rhi` gains its MoltenVK arm: a per-platform required-extension list,
  `ConsumerVulkanBuffer` and a `ConsumerVulkanTexture` minted from an IOSurface through
  `VkImportMetalIOSurfaceInfoEXT`, and `ConsumerVulkanTimelineSemaphore` minted from a Metal
  shared event through `VkImportMetalSharedEventInfoEXT` at `initialValue = 0`. Metal appears
  only as exported handles at the boundary — the shared-event port, the `MTLBuffer` behind an
  imported buffer for the tensor capsule — never as an API the helper writes against. Owner,
  2026-09-22, over a Metal-direct importer: that would be a second system beside the consumer
  RHI. Cost carried, measured: a MoltenVK device is ~0.5 s cold and 26–70 ms warm per helper,
  inside the helper startup budget the plan already tests.
- ADDED: the Mach channel carries the sidecars the Unix socket carries. A texture registration
  crosses with its image recipe, its layout cell and its `produce_done` / `consume_done` ports; a
  lookup or check-out returns them beside the IOSurface port; `update_layout` exists. The
  four-port headroom `MAX_SURFACE_SHARE_MACH_MESSAGE_PORTS` reserved is what this fills.
- MODIFIED: the ladder's Apple clause narrows once more, reversing "the parent-death signal has
  no Apple equivalent and stays unbuilt." A helper watches its parent's pid through kqueue
  `EVFILT_PROC` with `NOTE_EXIT`, belt-and-braces with the surface-share port's no-senders
  notification, and tears itself down when either fires; a `SIGKILL`ed app leaves no helper.
  The signal ladder gains macOS test coverage, which is zero today.
- ADDED: reactive and continuous execution pace the same on both floors. The thread runner's
  descriptor-driven reactive wait gains a kqueue arm beside epoll, and the shutdown wake rides
  it, retiring the sleep loop on macOS. `MonotonicTimer` is built on kqueue `EVFILT_TIMER` with
  `NOTE_MACHTIME`, drift-free on absolute deadlines, behind the unchanged Python surface.
- MODIFIED: the one-clock rule reaches the wheel. The wheel's `monotonic_now_ns` on Darwin
  moves from `CLOCK_MONOTONIC` to `mach_absolute_time` so a helper's stamps, its timer deadlines
  and the engine's `MediaClock` share one domain — today they drift by every sleep. Linux is
  unchanged.

## §Graphics — RHI / GPU

- MODIFIED: importability has three flavours. IOSurface joins DMA-BUF and OPAQUE_FD in
  `TextureCrossProcessImportability`, derived per acquisition and never a dial: on macOS a
  texture that must cross — a Python `acquire_texture`, a kernel output published downstream — is
  allocated on a private IOSurface the engine creates, imported at `vkCreateImage`, and bound to
  a device-local memory type the image's `memoryTypeBits` admit — never a host-visible one, which
  eagerly allocates a private `MTLBuffer` per image (measured: 859 µs and 8 MB each); on MoltenVK
  and Apple Silicon that resolves to type 0, chosen by query, never assumed. `OPTIMAL` tiling,
  strides read from the surface. Everything else keeps a
  non-importable allocation. A flavour the driver refuses falls back and the later import
  refuses by naming the flavour, as on Linux.
- MODIFIED: **the staged door has a direct arm.** On macOS the Vulkan image stays declared
  `OPTIMAL`, but its storage is the IOSurface's own linear rows — an IOSurface-backed Metal
  texture is linear by construction, and MoltenVK treats the tiling as metadata. `cpu()` reaches
  those rows through the surface's host mapping and the device tensor through a no-copy
  `MTLBuffer` over the same pages, so both read and write the surface itself, ~~ordered on the
  surface's shared-event timeline — the engine's next read waits, bounded, on the helper's
  write-done value~~ ordered ahead of the engine's next read *(superseded 2026-09-24 by #2404,
  owner decision A: the door retires the write before it closes — the device tensor's exit
  drains torch's MPS queue; an MLX write is `mx.eval`ed inside the scope — so it is complete
  before the id can be published; a pooled frame has no timeline on macOS and no helper-signalled
  write-done value exists)* — with no export staging and no readback copy. The six
  staging escalate ops are not needed on macOS and refuse by name saying so. Owner, 2026-09-22,
  over a Linux-identical staging: what this narrows is stated — the texture door's edit is
  published per store, as the pixel-buffer door's already is everywhere; the engine never reads a
  torn frame, a second concurrent holder could observe an edit mid-flight.
- MODIFIED: "Python reaches every kernel kind" is per floor, and the tiers are typed. Compute
  and graphics kernels from Python run on macOS — `GpuContext`'s kernel and buffer surface and
  the escalate ops are un-gated to both floors. Ray tracing and Vulkan Video are absent tiers on
  MoltenVK: `supports_ray_tracing_pipeline` and the capability snapshot exist on macOS and
  answer, and every constructor refuses with the typed error at `setup()`, as the floor promised
  and the tree does not yet do. A GLSL construct MoltenVK cannot serve refuses at
  `create_*_kernel`, naming the driver.

## §Media I/O — camera, display, audio, codecs

- MODIFIED: the audio device backend seam gains its Apple arm: CoreAudio through the AUHAL
  audio unit, capture and playback streams stamped with the device's own timing, enumeration
  owned by the backend, the once-per-process probe listing it. `MicrophoneSource` and
  `SpeakerSink` go live on macOS unchanged above the seam. Microphone permission is requested,
  never merely queried, with the same responsible-application diagnostic the camera has.
- MODIFIED: video codec blocks are built on a codec backend seam, not on Vulkan Video by name.
  The seam has two arms — Vulkan Video on Linux, VideoToolbox on Apple — chosen per process
  with no dial; `H264Encoder`, `H264Decoder`, `H265Encoder`, `H265Decoder` are platform-free
  above it and the marker classes stop being Linux-only. VideoToolbox speaks AVCC with
  parameter sets in the format description; the Annex-B boundary is converted at the seam's
  edge so the published `EncodedVideoFrame` is identical on both floors.
- MODIFIED: `Mp4Sink` records on every platform. Its Annex-B helpers are pure byte walking and
  move out from under the Vulkan Video tree; nothing in the muxer is Linux-bound.
- MODIFIED: `VirtualCameraSink` has no Apple port, stated rather than deferred. A macOS virtual
  camera is a Camera Extension inside a bundle, which the floor rules out under any
  justification. The block refuses by name at `rt.add()`, and `streamlib enable-virtual-camera`
  refuses before it prints anything.
- MODIFIED: processor-owned windows are a Python capability on both floors. The wheel's window
  arm, with its HDR sidecar, rides the present loop `macos-platform-floor` shipped.

## §Networking — transport, runtime mesh

- MODIFIED: the copy-out door is no longer Linux-only. A frame's pixels read out for the mesh
  on macOS through the surface's IOSurface, ~~ordered on its timeline~~ ordered ahead of the
  read by publication *(superseded 2026-09-24 by #2404: no helper-signalled value exists to wait
  on)*, so a non-Linux sender no longer says its surface bags do not cross.

## §Control plane & observability

- MODIFIED: the surface exchange answers on macOS. `streamlib tap`'s surface-id-for-pixels door
  reads the IOSurface the same way, which is what makes the repo's own live verification usable
  on the Mac.

## §Distribution & versioning

- MODIFIED: the wheel is built, tested and linted on the macOS lane, not excluded from it. The
  test-target `pipe2` goes portable; `check-no-inheritable-descriptor` gains its
  portable-CLOEXEC allowance; `stubtest` and the Python suite run there. Where the runner
  exposes Metal, the GPU half of the suite runs in CI on macOS, which Linux can only do on the
  rig.
- MODIFIED: the in-process adapters are per floor and say so. `streamlib-adapter-vulkan`,
  `streamlib-adapter-cpu-readback` and `streamlib-adapter-skia` build on MoltenVK and are in
  the macOS lane. `streamlib-adapter-opengl` is a named absent tier on macOS — its seam is EGL
  and DMA-BUF, OpenGL is deprecated there, and a native macOS consumer takes the Vulkan
  adapter. `streamlib-adapter-cuda` is absent by nature; its DLPack module stays the
  workspace's one home for the ABI on both floors.

## Removals

- REMOVED: runtime/streamlib-engine/src/apple/texture.rs
  Metal-era residue: an unused `create_metal_texture` the floor's removal list missed, no live
  callers.
- REMOVED: MonotonicTimer is Linux-only
  The refusal string; the timer runs on both floors.
- REMOVED: the device-tensor scope is a Linux capability
  The refusal string; the scope runs on both floors.
- REMOVED: a processor-owned window is not reachable from this platform
  The refusal string; windows are a Python capability on both floors.
- REMOVED: is only available on Linux
  The fifteen escalate refusals. Compute and graphics run; the staging ops and ray tracing
  refuse with messages that name the reason, not the platform.
- REMOVED: every other target lands on the null backend
  The audio probe's non-Linux arm comment; the Apple arm exists.

---

## Decisions — resolved by the owner, 2026-09-22

**1 — The helper's importer.** A: `streamlib-consumer-rhi` gains a MoltenVK arm; Metal only as
exported handles. B: a Metal-direct IOSurface shim in the wheel, no Vulkan device in the helper,
saving the per-helper device bring-up. **RESOLVED — A.** B is a parallel system beside the one
that exists; the bring-up cost is measured and inside the budget the plan tests.

**2 — The texture door on macOS.** A: direct — read and write the IOSurface in place, ~~ordered on
the shared-event timeline~~ ordered ahead of the engine's next read *(superseded 2026-09-24 by
#2404; see §Graphics)*, no staging. B: Linux-identical — an export staging plus one GPU copy
per write, keeping "no torn frame at the block edge" for every holder. **RESOLVED — A.** What
it narrows is recorded in §Graphics.

**3 — Raw-handle export on macOS.** A: an `export_iosurface` flavour beside the two fd flavours.
B: the fd methods exist-and-refuse, no Apple raw handle. **RESOLVED — A.**

**4 — The in-process adapters.** A: vulkan, cpu-readback and skia build; OpenGL and CUDA are
named absent tiers. B: a CGL-and-IOSurface arm of the OpenGL adapter. **RESOLVED — A.** Zink
or any other GL on macOS would still need an IOSurface interop arm, which is the Vulkan adapter
with a translation layer in front of it.

**5 — Where the work lives.** A: one change, one milestone, the floor's description rewritten to
parity. B: this second delta and a second milestone, *Full feature parity on Apple Silicon*,
with *Camera → display on Apple Silicon* finishing as the floor it is six tickets into.
**RESOLVED — B.** The first delta is over the line cap, and the floor is a shippable increment
on its own.

## Not in scope

KosmicKrisp — a macOS 26 floor is a change of its own; everything here is written against the
loader, so the ICD swaps later at no cost. An Asahi Linux floor — a Linux target with a
conformant Mesa driver, needing no macOS work; noted, not planned. `packages/screen-capture`'s
disposition, decided under §Consumers' rules. JAX-metal. An `.app` bundle, under any
justification.

## Tickets

Derived 2026-09-22; blockers first. Tickets 1 and 2 sit in milestone 52 (the floor); 3–17 in milestone 53, *Full feature parity on Apple Silicon*.

1. #2400 — The wheel builds, tests and lints on macOS in CI — *floor milestone*, blocks everything.
2. #2361 — A Python processor edits a pooled frame on macOS — reshaped; *floor milestone*.
3. #2401 — The timeline crosses to a helper as a Metal shared event — blocked by 2.
4. #2402 — A texture crosses to a helper on macOS — blocked by 3.
5. #2403 — Kernels from Python run on macOS — blocked by 4.
6. #2404 — Device tensors on macOS, for torch-MPS and MLX — blocked by 4.
7. #2405 — An IOSurface raw handle from Python — blocked by 4.
8. #2406 — A published frame's pixels read out on macOS — blocked by 3.
9. #2407 — Processor-owned windows from Python on macOS — blocked by 2.
10. #2408 — `MonotonicTimer` on macOS, and the wheel's clock on the plan's domain — blocked by 1.
11. #2409 — Reactive wakeups on macOS pace like Linux — independent.
12. #2410 — A helper never outlives its engine on macOS — blocked by 2.
13. #2411 — CoreAudio behind the audio device seam — independent.
14. #2412 — A video codec backend seam, with Vulkan Video moved behind it — independent.
15. #2413 — VideoToolbox behind the codec seam — blocked by 14.
16. #2414 — `Mp4Sink` on every platform — independent.
17. #2415 — The in-process adapters build on macOS — independent.

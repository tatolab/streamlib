# macos-platform-floor

A second platform floor: Apple Silicon beside Linux + NVIDIA, reaching the same MVP sentence —
`pip install`, `streamlib new`, `streamlib dev`, camera live in a window, the scaffolded Python
effect included. Vulkan stays the one RHI; MoltenVK is the macOS driver. Apple frameworks appear
only at the edges Vulkan cannot reach: AVFoundation for capture, `CAMetalLayer` for present,
IOSurface plus Mach ports for cross-process frames. The evidence is
`docs/research/2026-09-19-macos-camera-display-revival.md`, measured on an M1 Max; every claim below
that says "measured" is from there.

**Scale gate — this skill, plus an ADR.** This touches the RHI (a second driver under the one
Vulkan backend), the cross-process transport (fd passing becomes Mach port passing), the processor
model (the helper spawn mechanism), and the wheel's public install contract. A new
`docs/decisions/macos-platform-floor.md` is owed rather than a section on an existing ADR — no
current ADR is about a platform floor. It was held back until the decisions below were settled — an
ADR records the shape chosen over its alternatives — and now lands with this change, all five blocks
resolved.

**Precondition.** Every entry this delta touches is DECIDED: the MVP sentence
(`ARCHITECTURE.md:14-21`), the capture-backend floor (`:1229-1231`), windowing and the one event
pump (`:1232-1246`), camera→GPU transport (`:1279-1281`), the RHI's one-Vulkan rule (`:941-943`),
helper placement (`:1283-1288`), the shutdown ladder's Linux-first clause (`:820-826`), wheel
portability (`:2836-2849`), the runtime directory's macOS arm (`:2940-2944`), and the mesh's
host-identity and duplicate-name clause (`:2683-2688`). The OPEN entries
in the sections touched are untouched by this delta: §Media I/O's `VirtualCameraSink` behaviours,
the windowed-port-on-a-smaller-channel question and audio plugins; §Graphics' layout-cell
serialisation and its "everything else"; §Processor model's drop-count reflection, port bag
reporting and extra execution flavors; and §Networking's common-clock-across-machines entry,
which this delta does not touch.

**Verified against the tree and the hardware, 2026-09-19.**

- The engine does not compile for `aarch64-apple-darwin`: 44 errors on default features, 65 with
  the Vulkan backend forced on. None is a MoltenVK capability gap — 21 are the unapplied
  edition-2024 `unsafe extern` migration across `apple/`, 23 are `core/rhi` facades dispatching to
  `src/metal/`, 3 are shaders `build.rs:26-27` compiles under a host-gated `cfg`, and the rest are
  `cfg(target_os = "linux")` on Vulkan code with nothing Linux in it.
- MoltenVK 1.4 creates a device accepting every feature the engine pushes unconditionally
  (`vulkan_device.rs:1133-1203`). It reports the device `apiVersion` clamped to whatever the
  instance requested, so the engine's `make_version(1, 4, 0)` (`:528`) is what makes the promoted
  1.3 entry points resolve — a probe of `apiVersion` would be misled.
- `runtime/streamlib-consumer-rhi` compiles on macOS with zero code changes once its `cfg` gates
  widen; only `MAX_DMA_BUF_PLANES` is genuinely Linux-bound.
- There is no video device backend trait. V4L2 is inline in `camera_source.rs` — enumeration
  (`:82`), open (`:174`), negotiation (`:360`), the DQBUF/QBUF loop (`:880-950`). Audio has the
  seam this needs: `AudioDeviceBackend` + `AudioCaptureStream` + a once-per-process probe
  (`core/context/audio_device_backend.rs:215-305,378-395`), with `MicrophoneSource` platform-free
  above it.
- `core/window_event_pump.rs:275-289` opts into `with_any_thread(true)` on X11 and Wayland; the
  file already names the Apple main-thread implementation as the seam it leaves (`:19-22`).
- The vendored fork already carries `VK_EXT_metal_surface`, `VK_EXT_metal_objects` and
  portability-subset bindings, and maps `RawWindowHandle::AppKit` to a Metal surface
  (`vendor/tatolab-vulkanalia/src/window.rs:48-51,175`). The engine hardcodes a Linux instance
  extension list instead of calling `get_required_instance_extensions`.
- Handle exchange is per pool slot, not per frame: `register_buffer` is called from the pool
  pre-allocation loop (`gpu_context.rs:448`, `:573`). iceoryx2 stays the data plane and runs on
  macOS unmodified.
- `surface_store.rs` holds a macOS XPC client with no server behind it, and `apple/xpc_ffi.rs`
  declares a listener that is never called.
- All twelve CI jobs are `ubuntu-latest`. `xtask lint_logging.rs:770,1156-1260` evaluates `cfg` as
  if the target were Linux and skips `apple/` and `metal/` — which is how this rotted unseen.
- Proven on the hardware: IOSurface imports as a `VkImage` single-plane and biplanar (superseded
  for camera frames by #2359: a CoreVideo 4:2:0 surface imports only its luma plane as an image, and
  imports whole as a storage buffer — see §Media I/O);
  `CAMetalLayer` yields a working swapchain; a spawned Python child edits a 1080p GPU frame through
  a numpy view over a Mach-passed IOSurface at 698 fps, verified pixel-exact; a timeline semaphore
  crosses to that child over public API with no launchd service, at 120 µs per round trip.

---

## §Product — the MVP sentence

- MODIFIED: the sentence names a second floor. A Python developer on **Linux with an NVIDIA GPU or
  on Apple Silicon** pip-installs streamlib and reaches the same minute-to-camera bar, scaffolded
  Python effect included. The zero-ceremony clauses are unchanged and apply to both. macOS security
  prompts are part of the experience and do not breach zero ceremony; needing a bundle to obtain
  them would.

## §Graphics — RHI / GPU

- MODIFIED: "All Vulkan lives in the RHI" gains its platform clause. Vulkan is the one RHI on every
  supported platform; MoltenVK is the macOS driver and is reached through the same
  `HostVulkanDevice`. There is no second backend and no per-platform RHI.
- ADDED: the instance requests portability enumeration and the `ENUMERATE_PORTABILITY_KHR` flag,
  and the device requests `VK_KHR_portability_subset`, wherever the driver advertises them. The
  requested instance API version is the engine's floor for promoted entry points and is never
  inferred from a device query.
- ADDED: capabilities MoltenVK does not implement are absent tiers, not failures. Ray tracing and
  Vulkan Video refuse at construction with the typed error they already raise; the camera→display
  path touches neither. Where a driver advertises no `MAILBOX`, a non-vsync request takes FIFO and
  says so once rather than silently.

## §Media I/O — camera, display, audio, codecs

- MODIFIED: capture is no longer V4L2-only. The floor becomes a video device backend seam with two
  arms — V4L2 on Linux, AVFoundation on Apple — chosen by a once-per-process probe that is logged
  once, with no configuration dial and no environment override.
- ADDED: the seam itself, modelled on the audio one and extending it rather than paralleling it: a
  backend trait, a capture-stream trait with a hand-off callback, enumeration owned by the backend,
  and a named `device_id` that misses refused at `setup()` by name. `CameraSource` becomes
  platform-agnostic above it; the V4L2 implementation moves under `linux/` unchanged. The published
  contract does not move: a `VideoFrame` bag on port `video` whose `surface_id` names a pooled
  buffer, its timestamp in the machine monotonic epoch, colour as the H.273 four-tuple.
- ADDED: Apple capture hands back an IOSurface-backed `CVPixelBuffer`, ~~imported as a `VkImage`
  through `VK_EXT_metal_objects`~~ whose memory is imported as a storage buffer through
  `VK_EXT_external_memory_host` and read by the same NV12 kernel a V4L2 DMA-BUF import feeds, so
  colour stays the engine's on both platforms. Camera→GPU transport keeps its no-dial rule:
  importability is an allocation flavour the engine derives per acquisition, and IOSurface joins
  DMA-BUF and OPAQUE_FD as one of them; a driver that refuses the import falls back to CPU upload.
  Owner, 2026-09-21, while shipping #2359, on evidence that MoltenVK refuses every CoreVideo 4:2:0
  surface as a multi-planar `VkImage` — its import check compares the surface's top-level element,
  one byte in 1×1, against the whole six-byte 2×2 block, through v1.4.2 and on `main` — while
  MoltenVK 1.4.2 imports the same surface's memory as a buffer and a compute kernel reads both
  planes from it.
- ADDED: **a camera frame carries one capture instant on both of its stamps.** A bag has two: the
  payload's own `timestamp_ns` and the envelope's, which `OutputWriter::write` sets from its own
  `MediaClock::now()` before delegating to `write_with_timestamp`. Today the camera writes plainly,
  so both are publication time and agree by accident, while different consumers read different
  doors — the encoder reads the payload, `Mp4Sink` and the mesh read the envelope. The camera
  therefore resolves the device's instant once and writes it to both: assigned to the frame's own
  `timestamp_ns` field **and** passed as the same value to `write_with_timestamp`, never through the
  implicit write. Swapping the call alone does not do it — `write_with_timestamp` serialises the
  payload as given and sets only the envelope, so a call-site-only change leaves the two disagreeing
  in the other direction.
- ADDED: a device stamp is trusted only when it is usable, and the platform flag alone does not
  establish that. The stamp is taken when the device reports it on the machine's monotonic clock and
  it is non-zero; otherwise the engine falls back to its own clock at dequeue and says so once per
  device, naming it. **A stamp ahead of the engine's own clock at dequeue is clamped to that instant
  and counted**, because no real capture happens in the future and trusting one silently is how a
  frame-period of audio-video skew ships. Owner, 2026-09-19, on evidence that the `vivid` driver
  sets the monotonic flag honestly and still reports every stamp roughly nine tenths of a frame
  period ahead.
- MODIFIED: windowing states where the loop lives. The engine still owns the process's one event
  pump; on Apple that pump runs on the process's first thread, which is the thread `rt.run()`
  blocks. Window policy, the raw-window-handle seam and the per-processor render thread are
  unchanged. The present target is minted from a `CAMetalLayer` on Apple.
- ADDED: a window's requested size is in the desktop's logical pixels — physical pixels divided
  by the display's scale factor — so `DisplayWindow`'s `width` and `height` mean the same apparent
  size on a 1x and a 2x display, while the swapchain renders at the screen's full density. Owner,
  2026-09-21, while shipping #2357, on a 640x360 test window showing at 320x180 on a Retina Mac.
- ADDED: camera permission on Apple is requested, never merely queried, and never awaited on a
  path that would stall the graph. The engine is not the permission subject — the terminal that
  launched it is — so a refusal names the responsible application and the setting to change. The
  engine is never daemonised, because detaching breaks the attribution chain and silently costs
  camera access.

## §Processor model & scheduling

- ADDED: cross-process frames on Apple. The surface-share service keeps its verbs and its
  per-slot-not-per-frame shape; beneath them the Unix socket becomes a raw Mach channel and
  `SCM_RIGHTS` becomes an IOSurface Mach port. A peer is validated by audit token — pid and pid
  version — and peer death is observed through Mach notifications wired to teardown. Surfaces are
  never created global. A slot is recycled when its consume timeline has passed and the surface
  reports not in use.
- ADDED: cross-process ordering keeps the timeline contract. `produce_done` and `consume_done` are
  exported as Metal shared events and re-imported by the helper at their current value; values
  advance and are never reset. Host-side ordering is the engine's own fallback behind the same
  seam, taken at runtime when the export path fails rather than chosen at build time.
- ADDED: no producer-side GPU wait is ever unbounded. A wait a peer can stall is bounded well
  under the platform's command-buffer timeout, because a stalled helper must never be able to take
  the engine's device down with it. Measured on Apple: an unsatisfied GPU wait past roughly five
  seconds loses the device unrecoverably.
- MODIFIED: the shutdown ladder's "Linux-first" clause narrows to what is still true. Process
  groups, `waitid` and CLOEXEC-at-source compile on both platforms; the parent-death signal has no
  Apple equivalent and stays unbuilt. Recorded while shipping #2357: on macOS, SIGINT and SIGTERM
  now escalate through the same ladder, from handlers installed once for the process's life;
  SIGHUP is not owned there, and no disposition is handed back when a run ends.

## §Distribution & versioning

- MODIFIED: the wheel portability model states what "the host may supply" means per platform. On
  Linux, system libraries are dlopen'd and never linked. On macOS the Vulkan driver is not present
  on a stock machine, so the wheel carries the Vulkan loader and MoltenVK and points the loader at
  them additively, leaving a user's own driver discoverable. The MoltenVK it carries is 1.4.1 or
  later — camera zero-copy under §Media I/O depends on a host-pointer import in its spec-correct
  form, which 1.4.0 refuses and 1.4.2 takes (measured), and which 1.4.1's source is the first to
  accept. Owner, 2026-09-21, #2359. Everything else keeps the rule: only
  the system frameworks may be linked.
- ADDED: the macOS artifact is an `aarch64-apple-darwin` wheel at the same abi3 floor, built on a
  macOS runner, with a pinned deployment target. No notarisation and no signing identity are
  required, because nothing pip delivers is quarantined; any post-link rewrite of a shipped binary
  is re-signed ad hoc in the same step that rewrites it.
- ADDED: the wheel's portability proof covers Mach-O as well as ELF — the same host-may-supply
  rule, plus a code-signature presence check and a deployment-target ceiling. A binary the proof
  cannot parse fails rather than skips.

## §Networking — transport, runtime mesh

- MODIFIED: **macOS gains a host identity, reversing the DECIDED clause that it has none.** The
  current entry reads "macOS has no host identity and therefore no exception, so a duplicate there
  is refused until the old token leaves" — written when no macOS runtime could start. With Apple
  Silicon a supported floor, that clause turns the duplicate-runtime-name refusal into a dev-loop
  papercut: multicast scouting is on by default, so every runtime joins the mesh, and a crashed
  `streamlib dev` cannot restart under its own name. On Apple the identity is the kernel's boot
  session UUID and nothing else — macOS has no pid namespaces, so the second half of the Linux
  identity has no counterpart and needs none. The engine already reads that UUID for the machine
  clock identity, and the Linux side keeps one read site per file for exactly this reason, so the
  Apple arm follows the same discipline rather than opening a second reader.
- ADDED: the same-host-and-pid-is-gone exception therefore fires on Apple as it does on Linux, with
  process liveness answered natively rather than by the `false` stub that keeps the refusal today.
- ADDED: the mesh is proven on macOS rather than assumed. Zenoh's transport is confirmed working
  there — session, publisher and subscriber, with the engine's own feature set — and the remaining
  work is the identity arm, an end-to-end two-runtime proof, and a CI lane that keeps both honest.

## Removals

- REMOVED: runtime/streamlib-engine/src/metal
  The parallel Metal RHI, 2488 LOC against 89332 LOC of Vulkan RHI. Mine
  `metal/rhi/texture_cache.rs:44-56` and `pixel_buffer_pool.rs:44-69` for their AVFoundation
  serialisation constraints before deleting. Conditional on NEEDS DECISION 2.
- REMOVED: backend-metal
  The Cargo feature whose only effect is to turn Vulkan off, plus every `cfg` site that reads it
  (`Cargo.toml:19`, `lib.rs:145,182,188,322`, `core/rhi/device.rs:9,33,40,69`,
  `gpu_context.rs:1034`). Conditional on NEEDS DECISION 2.
- REMOVED: runtime/streamlib-engine/src/apple/time.rs
  A dead second copy of the Mach clock, linked against CoreServices, zero call sites, its only test
  ignored. `apple/media_clock.rs` is the live one.
- REMOVED: runtime/streamlib-engine/src/apple/arkit.rs
  Two empty modules and an empty test. Zero references in the tree.
- REMOVED: runtime/streamlib-engine/src/apple/pixel_transfer.rs
  A `VTPixelTransferSession` wrapper with zero callers that reaches the Metal tree through
  `as_metal_device()`, `as_metal_texture()` and `metal_queue_ref()` — it cannot outlive the
  backend those facade arms belong to. Recorded while shipping #2355.
- REMOVED: runtime/streamlib-engine/src/apple/texture_pool_macos.rs
  The IOSurface-backed pool arm, built on `MetalTexture` and reached only from the macOS
  `allocate_slot` arm of `core/context/texture_pool.rs`, itself a Metal facade arm. The IOSurface
  allocation flavour returns through the Vulkan RHI under §Media I/O. Recorded while shipping #2355.
- REMOVED: backend-vulkan
  The other half of the backend selector. With one RHI it selects nothing; every site reading it is
  a backend-selection `cfg` this change simplifies. Owner, 2026-09-19. Recorded while shipping #2355.
- REMOVED: runtime/streamlib-engine/src/core/rhi/backend.rs
  `RhiBackend` and its `STREAMLIB_RHI_BACKEND` environment variable — a *runtime* backend selector,
  public at `core::RhiBackend`, with zero consumers. One RHI selects nothing and takes no dial.
  Recorded while shipping #2355.
- REMOVED: runtime/streamlib-engine/src/core/rhi/texture_cache.rs
  `RhiTextureCache` and `RhiTextureView`, whose only constructor (`new_metal`) lived in the Metal
  tree, together with `GpuContext::create_texture_cache` and `GpuContext::metal_device`. Zero
  consumers. Recorded while shipping #2355.
- REMOVED: runtime/streamlib-engine/src/vulkan/rhi/vulkan_texture_cache.rs
  `VulkanTextureCache`, reachable only through the facade above. Recorded while shipping #2355.
- REMOVED: runtime/streamlib-engine/src/core/rhi/gl_interop.rs
  `GlContext`, `GlTextureBinding` and `gl_constants` — crate-root re-exports whose only
  implementation was Metal's CGL/IOSurface path, with zero consumers. The GL surface adapter
  (`streamlib-adapter-opengl`'s `OpenGlContext`) is the live one and is untouched. Recorded while
  shipping #2355.
- REMOVED: RhiBlitter::blit_copy_iosurface_raw
  And `blit_copy_iosurface` on `GpuContext`, `GpuContextFullAccess` and `GpuContextLimitedAccess`.
  The Metal blitter was its only implementation; what remained refused by name, making the three
  facade layers a no-op chain with no callers. An IOSurface reaches the RHI as ~~a `VkImage` through
  `VK_EXT_metal_objects`~~ an imported storage buffer (#2359, §Media I/O), not a raw blit. Recorded
  while shipping #2355.
- REMOVED: Texture::iosurface_id
  An ungated `pub fn` that existed on Linux and answered `None` there; its only producer was the
  Metal texture. Recorded while shipping #2355.
- REMOVED: PooledTextureHandle::iosurface_id
  The pooled-handle forwarder onto it, ungated and equally producerless. Recorded while shipping
  #2355.
- REMOVED: NativeTextureHandle::IOSurface
  The variant those two answered with, left without a constructor or a match arm anywhere in the
  tree. Recorded while shipping #2355.
- REMOVED: HostVulkanTexture::placeholder
  Its only production caller was `Texture::from_metal`; what remained was a test exercising nothing
  else. Recorded while shipping #2355.
- REMOVED: imported_from_iosurface
  A `HostVulkanTexture` flag whose last `true` writer went with the Metal import sketch, leaving the
  branch it guarded inside `Drop` unreachable. Recorded while shipping #2355.
- REMOVED: imported_from_metal
  A `VulkanSemaphore` flag in the same position, written `false` once and read nowhere. Recorded
  while shipping #2355.
- REMOVED: the macOS XPC arm of core/context/surface_store.rs
  An XPC client with no server behind it, reaching the deleted Metal tree for its mach ports. Its
  `CheckedInSurfaces` map went with it — the map had no writer left, which made
  `disconnect`'s per-surface release loop provably empty on Linux too; the socket-close the service
  already treats as a full release is what actually released them. The Apple transport returns as
  raw Mach in #2360. Recorded while shipping #2355.
- REMOVED: Mp4Sink on macOS
  Not deleted — gated to Linux, with `mp4_annex_b_access_unit`, `mp4_fragmented_file_writer` and
  `mp4_track_sample_entry`. They read parameter sets through the engine's Vulkan Video NAL parser
  (`nv_video_parser`), which MoltenVK cannot serve and which stays Linux-only, so a macOS runtime
  registers no `Mp4Sink` and records no MP4. The parser's Annex-B helpers are pure byte walking and
  nothing about them is Linux-bound; lifting them out from under the Vulkan Video tree would make
  the sink cross-platform again, and is backlog rather than milestone work. Recorded while
  shipping #2355.
- REMOVED: tonic-build
  A macOS-only build dependency for a surface-share gRPC service that does not exist;
  `build.rs` names neither it nor protobuf.
- REMOVED: prost-build
  Its sibling, same dead block at `Cargo.toml:299-301`.
- REMOVED: runtime/streamlib-engine/src/apple/runtime_ext.rs
  The hand-rolled `NSApplication` loop (`run_macos_event_loop`) with `setup_macos_app`,
  `ensure_macos_platform_ready` and `StreamlibAppDelegate`. winit's own application delegate
  launches the app, sets its activation policy and dispatches `Init`/`Resumed`, so a second
  delegate would have kept the one pump from ever starting. Recorded while shipping #2357.
- REMOVED: window_shared_with_event_pump
  The registration's `Arc<Window>` accessor. A registration now hands its window back to the
  pump to close — winit closes an AppKit window only on the first thread — and the present
  target is minted from `present_surface_source`. Recorded while shipping #2357.
- REMOVED: trigger_macos_termination
  The macOS signal arm's hop to `NSApplication.terminate`. Ctrl-C and SIGTERM feed the same
  escalation the Linux arm does. Recorded while shipping #2357.
- REMOVED: runtime/streamlib-engine/src/apple/xpc_ffi.rs
  The XPC FFI surface, whose listener was never called; the transport is raw Mach, carried by
  `streamlib-surface-client`'s macOS arm. Recorded while shipping #2360.
- REMOVED: STREAMLIB_XPC_SERVICE_NAME
  The `start()` arm that read it and connected a `SurfaceStore` whose macOS `connect()` refused.
  The runtime now registers its own Mach service in `new()` and connects to it in `start()`,
  exactly as the Linux arm does with its socket. Recorded while shipping #2360.
- REMOVED: macOS: IOSurface ID
  The `RhiExternalHandle::IOSurface { id }` variant, named by its doc line because the surviving
  `IOSurfaceMachPort` shares its prefix — a global-id handle with no constructor, and the
  shortcut the plan forbids: a private surface's id does not resolve cross-process, and making it
  resolve means a global surface. Its `mach_port()` accessor, which had no callers, went with it.
  `IOSurfaceMachPort` is the one Apple handle. Recorded while shipping #2360.
- REMOVED: create_metal_texture_from_iosurface
  With `iosurface_format_to_metal` — Metal-RHI residue in `apple/iosurface.rs` with no callers.
  The file now holds the private-IOSurface allocator the pool's slots come from. Recorded while
  shipping #2360.
- REMOVED: core-graphics = "0.24"
  The engine's direct dependency, with no use site; the macOS tests reach Core Graphics through
  `objc2-core-graphics`. The crate stays in the lockfile through the vendored vulkanalia fork.
  Recorded while shipping #2360.

---

## [NEEDS DECISION] 1 — Is macOS a supported platform floor, or a developer-machine floor?

- **A. Supported floor.** The MVP sentence names it, CI gates it, the wheel is released for it, and
  a macOS regression blocks a release like a Linux one.
- **B. Developer-machine floor.** The engine builds and runs on Apple hardware so contributors are
  not stranded, but the sentence, the release closure and the support bar stay Linux + NVIDIA.

**RESOLVED — A, owner, 2026-09-19.** A supported platform floor: *"I want people to be able to
build and run it, not just develop on it."* Consequences, all binding: §Product names Apple Silicon,
CI gates macOS on every PR, the macOS wheel is part of the release closure, and a macOS-only
regression blocks a release exactly as a Linux one does. This is what makes the capture, present,
cross-process and wheel work part of the milestone rather than optional — under B the milestone
would have stopped at a window opening on a developer's machine.

## [NEEDS DECISION] 2 — Vulkan everywhere, and does `src/metal/` delete?

- **A. One Vulkan RHI; delete the Metal tree.** MoltenVK is the macOS driver.
- **B. Keep a Metal backend.** Revive `src/metal/` as a real second RHI beside Vulkan.

**RESOLVED — A, owner, 2026-09-19.** One Vulkan RHI; the Metal tree deletes. The removal bullets
above are unconditional. Linux impact: none behavioural. `backend-metal` is absent from
`default = []`, so `not(feature = "backend-metal")` is already always true on Linux and its removal
is a no-op there — but it does edit the `cfg` expressions in six shared files (`lib.rs`,
`core/rhi/{device,texture,command_buffer,command_queue}.rs`, `core/context/gpu_context.rs`), so
Linux CI green is the ticket's proof obligation.

## [NEEDS DECISION] 3 — Which service-less rendezvous, and does the helper spawn move?

- **A. Dynamic bootstrap name now.** Spawn-agnostic, no change to the spawn host. The name is
  listed in the user's launchd domain, so the audit-token check becomes load-bearing rather than
  defence in depth.
- **B. Move the helper spawn to `posix_spawn` first, then take the capability-secure port stash.**
  Unguessable and unlisted. Requires replacing the two `pre_exec` closures in
  `python_helper_process_spawn_host.rs:925,989` — Rust's `Command` falls back to fork+exec whenever
  a closure is registered, and the stash does not survive that.

**RESOLVED — A now, B as tracked follow-up, owner, 2026-09-19.** Linux impact of A: **none.** The
bootstrap-name rendezvous lives entirely in the Apple arm of the surface-share service; Linux keeps
its Unix socket and `SCM_RIGHTS` untouched, and no shared file changes behaviour. Linux impact of
B, when it is scheduled: **real, and this is a second reason it is deferred.**
`python_helper_process_spawn_host.rs` is one shared file with per-platform arms, so replacing
`Command` + `pre_exec` with a direct `posix_spawn` changes the spawn path on **both** platforms.
B is therefore its own change with its own Linux proof, never a rider on this one — filed as #2368.

## [NEEDS DECISION] 4 — Does a camera frame carry the capture instant or the publication instant?

- **A. Capture instant.** The backend stamps what the device reports, as audio already does
  (`microphone_source.rs:334-338`).
- **B. Publication instant.** Keep today's behaviour: `camera_source.rs:1152` stamps
  `MediaClock::now()` at publish and never reads the V4L2 buffer stamp.

**RESOLVED — A, owner, 2026-09-19. The Linux check has reported and the decision stands.** The
camera carries the device's capture instant. **This is the one decision in this delta that is not
additive**: every other entry is macOS-only, this one changes Linux behaviour.

No consumer breaks outright, and no test asserts on a camera timestamp today — timestamp assertions
exist only on audio, and the loss metrics are sequence-based and immune. Two findings amended the
§Media I/O entries above rather than reopening this block: a bag carries **two** stamps and the
consumers split across them, so the change must move both; and `vivid` sets the monotonic flag
honestly while reporting every stamp roughly nine tenths of a frame period in the future, which is
why the trust rule needs the clamp and not the flag alone. Residual, carried rather than closed: the
Linux box has no real camera — all four nodes are `vivid` — so what a UVC device reports is still
unmeasured, and the clamp is what makes that gap safe to carry.

## [NEEDS DECISION] 5 — Apple Silicon only, or Intel Macs too?

- **A. `aarch64-apple-darwin` only.** One wheel, one CI lane.
- **B. Both architectures.** A second wheel and lane, or a universal2 build.

**RESOLVED — A, owner, 2026-09-19.** `aarch64-apple-darwin` only. **No Intel, and Rosetta is not a
supported path** — not as a fallback, not as a courtesy. One wheel, one CI lane, one architecture.

The owner attached a direction to this: optimise for modern Apple Silicon — M1 Max and beyond — and
take the newest capability available rather than the most compatible. Two places that bites, neither
settled here because neither is this delta's to settle:

- The wheel's deployment target is chosen in the distribution ticket, not inferred. MoltenVK's own
  runtime floor is the lower bound; "prefer modern" argues for going above it rather than sitting on
  it, and the cost is the oldest macOS a user may run.
- KosmicKrisp stays out of scope below, but the direction raises it from curiosity to a real
  evaluation: it is the MIT-licensed, fully Vulkan 1.3 conformant Metal driver, Apple Silicon only.
  Adopting it means a macOS 26 floor, which is a far larger call than "prefer modern" and is a
  change of its own, not a ticket in this one.

---

## Decided before this proposal, recorded not asked

- Cross-process frames are in scope for this milestone, not deferred — owner, 2026-09-19. A macOS
  build whose scaffolded app does not run is not the MVP sentence.
- The artifact is the wheel, identical to Linux: no `.app` bundle, no launchd, no installer, under
  any justification. `brew` is a build-time dependency only. Embedding StreamLib in another
  application, and a future iOS library, are out of scope — owner, 2026-09-19.

## The condition that was outstanding, now discharged

Block 4's resolution was conditional on a Linux regression check. It reported 2026-09-19: no
consumer breaks, no test at risk, and two findings folded into §Media I/O above — the two-stamp
split, and the clamp the monotonic flag cannot give. **The gate on the timestamp work is lifted.**

## Not in scope

VideoToolbox codecs and the four Vulkan Video built-ins, which MoltenVK cannot serve. CoreAudio and
the audio device backend's Apple arm. Ray tracing. The Apple `VirtualCameraSink`, which is a
CoreMediaIO Camera Extension and therefore a different distribution artifact entirely, not a port.
`MonotonicTimer`, so continuous-execution Python processors lag one release. The six surface
adapters. `packages/screen-capture`'s disposition. KosmicKrisp, the conformant Vulkan-on-Metal
driver, which is worth revisiting once macOS 26 is the floor.

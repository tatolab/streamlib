# What it takes to run StreamLib on macOS again — camera → display

**Question.** The plan's floor is Linux + NVIDIA (`docs/plan/ARCHITECTURE.md:1229`). What is the
minimum set of work that gets `pip install streamlib` → `streamlib new` → `streamlib dev` → camera
live in a window on Apple Silicon, keeping Vulkan as the one RHI and reaching for Apple SDKs only
where Vulkan cannot go?

**Answer.** Keeping Vulkan on macOS is not a compromise — it is the cheap path, and it is proven on
hardware below. MoltenVK gives a Vulkan 1.3 device that accepts every feature the engine hard-requires
at `vkCreateDevice`, presents through `CAMetalLayer`, and imports an `IOSurface` as a `VkImage`
with no copy. The engine does not compile for `aarch64-apple-darwin` today, but **not one** of its
65 compile errors is a MoltenVK capability gap: they are an unapplied edition-2024 migration, a stale
parallel Metal backend, and `#[cfg(target_os = "linux")]` gates on code that has nothing Linux in it.

The real work is not the GPU. It is that four Linux *kernel* interfaces have no Apple equivalent and
need Apple-native arms behind seams that mostly already exist: V4L2 capture (→ AVFoundation),
DMA-BUF/OPAQUE-FD external memory (→ IOSurface + `VK_EXT_metal_objects`), `SCM_RIGHTS` fd passing
(→ IOSurface mach ports over a raw Mach channel — see §10, it is not XPC), and the winit any-thread
event pump (→ main-thread pump).

This memo files no tickets and makes no decision. Extending the platform floor is an architecture
decision for the owner — `/align` or `/propose-change`. §13 drafts the change and the ticket list so
that step is short.

---

## 1. Measured on this machine

Host: Apple M1 Max (`T6000`), macOS 26.3, rustc 1.91.1 host `aarch64-apple-darwin`,
MoltenVK 1.4.0 + Vulkan Loader 1.4.335.0 (Homebrew), Xcode 26.5 SDK.
Probe sources were scratch programs under `/tmp` (`vkprobe/probe.c`, `vkprobe/probe2.c`,
`iosprobe/probe.m`, `icetest/`) — not durable; each output below is quoted rather than referenced,
and the probes are cheap to rewrite from those quotes. The engine measurements ran in a throwaway
`git worktree` that has been removed; the checkout itself was never modified.

### 1.1 MoltenVK clears the engine's device bar

`vkCreateInstance` at `apiVersion` 1.3 **and** 1.4 with `VK_KHR_portability_enumeration` +
`ENUMERATE_PORTABILITY_BIT_KHR` → `VK_SUCCESS`. One physical device: **`Apple M1 Max`, apiVersion
1.3.323**, 128 device extensions, 4 queue families each `GRAPHICS|COMPUTE|TRANSFER`, 1 queue each.

That answers the open question the RHI audit could not settle from the tree: **the engine's ~60 call
sites on promoted 1.3 entry points (`cmd_pipeline_barrier2`, `cmd_begin_rendering`, `queue_submit2`,
`wait_semaphores`) resolve. No KHR-alias rewrite is needed.**

**But be precise about why, because the obvious reading is wrong.** MoltenVK reports the *device's*
`apiVersion` clamped to whatever the instance asked for — it is not a capability report:

```
request 1.0  ->  device apiVersion 1.0.323
request 1.1  ->  device apiVersion 1.1.323
request 1.2  ->  device apiVersion 1.2.323
request 1.3  ->  device apiVersion 1.3.323
request 1.4  ->  device apiVersion 1.4.323
```

The engine asks for `make_version(1, 4, 0)` at instance creation (`vulkan_device.rs:528`), so it sees
1.4 and the promoted entry points are there. Two consequences worth carrying: **any code that probes
the device `apiVersion` to decide which entry points to call would be misled on MoltenVK**, and a
future change that lowers the requested instance version would silently take the promoted 1.3 entry
points away. This bit the sample-app spike, which requested 1.2 and then reported "MoltenVK is 1.2".

`vkCreateDevice` with every feature the engine pushes unconditionally
(`vulkan_device.rs:1133–1203`) → **`VK_SUCCESS`**:

| Hard-required by the engine | MoltenVK |
|---|---|
| `dynamicRendering` | YES |
| `timelineSemaphore` | YES |
| `synchronization2` | YES |
| `samplerYcbcrConversion` | YES |
| `shaderStorageImageReadWithoutFormat` / `WriteWithoutFormat` | YES |

Present / absent, against what the RHI probes:

| Present | Absent |
|---|---|
| `VK_KHR_swapchain`, `VK_KHR_timeline_semaphore`, `VK_KHR_synchronization2`, `VK_KHR_dynamic_rendering` | `VK_KHR_external_memory_fd`, `VK_EXT_external_memory_dma_buf` |
| `VK_EXT_descriptor_indexing`, `VK_KHR_buffer_device_address`, `VK_KHR_maintenance1–8` | `VK_EXT_image_drm_format_modifier`, `VK_KHR_external_semaphore_fd` |
| `VK_EXT_metal_objects`, `VK_EXT_external_memory_metal`, `VK_EXT_external_memory_host` | `VK_KHR_video_queue` and every `video_decode_*` / `video_encode_*` |
| `VK_EXT_metal_surface` (instance), `VK_KHR_portability_subset`, `VK_EXT_hdr_metadata` | `VK_KHR_acceleration_structure`, `VK_KHR_ray_tracing_pipeline`, `VK_KHR_ray_query` |

`VK_KHR_portability_subset` costs almost nothing here — only `pointPolygons`, `samplerMipLodBias`,
`tessellationIsolines` and `tessellationPointMode` are `VK_FALSE`. `triangleFans`,
`imageViewFormatSwizzle`, `mutableComparisonSamplers`, `separateStencilMaskRef`,
`vertexAttributeAccessBeyondStride` are all **true**. `geometryShader` is false — the engine
deliberately uses neither geometry nor tessellation shaders (`core/rhi/graphics_kernel.rs:8,31`).
`minImportedHostPointerAlignment` is 16384.

### 1.2 IOSurface → VkImage is zero-copy, single-plane and biplanar

```
IOSurfaceCreate 1920x1080 BGRA ok (id=65)
vkCreateImage(pNext=VkImportMetalIOSurfaceInfoEXT) = SUCCESS
vkCreateImageView on the imported image      = SUCCESS
biplanar 420v IOSurfaceCreate ok (planes=2)
vkCreateImage(420v IOSurface, R8_UNORM)      = SUCCESS
```

This is the whole AVFoundation capture story. `AVCaptureVideoDataOutput` hands back a
`CMSampleBuffer` whose `CVPixelBuffer` is IOSurface-backed; `CVPixelBufferGetIOSurface` yields the
`IOSurfaceRef`; that ref chains onto `VkImageCreateInfo` and the engine samples it. Biplanar `420v`
imports per plane, which is exactly the shape the existing NV12 compute converter already wants — it
reads Y and UV as separate views.

### 1.3 CAMetalLayer → VkSurfaceKHR → VkSwapchainKHR

```
vkCreateMetalSurfaceEXT                      = SUCCESS
queue family 0 present support               = yes
surface caps: minImageCount=2 maxImageCount=3 currentExtent=1280x720 usage=0x1f
TRANSFER_DST on swapchain images             = yes
present modes: FIFO, IMMEDIATE               (no MAILBOX)
60 surface formats incl. HDR10_ST2084 / HLG / EXTENDED_SRGB colorspaces
vkCreateSwapchainKHR(FIFO, COLOR_ATTACHMENT|TRANSFER_DST) = SUCCESS, 3 images
```

`vulkan_swapchain_colorspace.rs`'s existing walk (PQ+BT.2020 → `HDR10_ST2084_EXT`, HLG →
`HDR10_HLG_EXT`, else the sRGB ladder) maps onto this list unchanged. One behavioural delta:
MoltenVK advertises no `MAILBOX`, so the picker's non-vsync branch
(`vulkan_present_target.rs:1043`) falls through to FIFO. That is correct, not a bug, but it means
"vsync off" is not honourable on macOS and should say so once rather than silently.

### 1.4 iceoryx2 works

`iceoryx2` 0.9.3, node + `publish_subscribe` service + publisher + subscriber, 5 samples sent, 5
received. The data plane is portable as-is. Upstream lists macOS at the same support tier as Linux.

One wrinkle for later: Darwin's 31-character `PSHMNAMLEN` forces iceoryx2's macOS PAL to map long
logical names to generated short ones, keeping the mapping in `.shm_state` files under a
compile-time-constant `/tmp/`. Those do not honour the engine's configured domain root, so
§Control plane's "one runtime directory holds the iceoryx2 domain" is only partly true on macOS and
cross-domain isolation there rests on the name prefix alone.

### 1.5 The consumer RHI compiles on macOS essentially unchanged

In a throwaway worktree, widening `streamlib-consumer-rhi`'s `cfg(target_os = "linux")` gates to
include macOS and giving it `vulkanalia` + `libc` on that target:

- first pass: **2 errors**, both `streamlib_surface_client::MAX_DMA_BUF_PLANES`;
- replacing that one constant: **0 errors**.

`ConsumerVulkanDevice`, `ConsumerVulkanTexture`, `ConsumerVulkanBuffer`, the sync types, the layout
and format primitives — all of it typechecks on Apple Silicon with no code change. Widening
`streamlib-surface-client` itself fails exactly where it should: `MSG_CMSG_CLOEXEC` does not exist on
Darwin and `cmsg_len` is `u32` there, not `usize`. That is the honest Linux boundary — fd passing —
and it is the one the Apple arm has to replace rather than port.

### 1.6 The engine's 65 errors are all ours

`cargo check -p streamlib-engine` on macOS, default features → **44 errors**. Forcing the Vulkan
backend on and moving `vulkanalia` / `vulkanalia-vma` / `rspirv-reflect` / `shaderc` / `winit` /
`raw-window-handle` off the Linux-only dependency table → **65 errors**. Every one falls in a bucket
we own:

| Count | Class | Fix |
|---|---|---|
| 21 | `error: extern blocks must be unsafe` across `apple/*` | one `unsafe` keyword per block — the edition-2024 migration nobody re-ran on this target |
| 23 | `crate::metal::*` unresolved from `core/rhi/*` facades | those facades dispatch to the Metal backend on macOS; they need the same cfg predicate the `vulkan` module uses |
| 3 | `couldn't read …/*.spv` | `build.rs:26-27` compiles shaders under `#[cfg(target_os = "linux")]` only — and in a build script that cfg names the **host**, not the target, so shaders are built when cross-compiling to macOS from Linux and *not* when building natively on a Mac. The file already knows this distinction and uses `CARGO_CFG_TARGET_OS` two lines below for the PipeWire shims; the shader call was missed |
| ~18 | items hidden behind `cfg(target_os = "linux")` inside `vulkan/rhi/mod.rs` and `core/rhi/mod.rs` — `VulkanRhiDevice`, `StorageBuffer`, `RhiCommandRecorder`, the kernel `refuse_*` helpers, `host_marker`'s `Sealed` impl, `drm_modifier_probe` | widen the gate, except `drm_modifier_probe`, which is genuinely EGL/DRM and wants an Apple arm or an honest absence |

**Zero errors are "MoltenVK cannot do this."** This is a compile-time measurement of the first wave
only; a linker pass and a runtime pass will find more. But the shape of the first wave is the single
strongest argument for the Vulkan-everywhere direction.

---

## 2. What is actually in the tree

### 2.1 An Apple tree that once worked

`runtime/streamlib-engine/src/apple/` — 17 files, 2 741 LOC, **no `todo!()` anywhere**. Real
CoreVideo / IOSurface / vImage / XPC / Mach / GCD / AppKit code. `git log` puts its last substantive
edits between 2026-01 and 2026-05; everything since is cross-platform sweeps. It was working code
that has bit-rotted for roughly seven months because nothing compiles it.

Live and load-bearing: `media_clock.rs` (`mach_absolute_time`, re-exported as the engine's one
`MediaClock` on Apple), `machine_clock_identity.rs`, `thread_priority.rs` (Mach
`THREAD_TIME_CONSTRAINT_POLICY`), `runtime_ext.rs` (NSApplication bootstrap + main-thread pump, wired
into `Runtime::run`), `audio_clock.rs`, `texture_pool_macos.rs`, `xpc_ffi.rs`, `vimage_ffi.rs`.

Orphaned — real code, zero callers: `main_thread.rs`, `texture.rs`, `pixel_transfer.rs`
(`VTPixelTransferSession`), `time.rs` (a dead duplicate of `media_clock.rs` linked against
CoreServices), `arkit.rs` (empty).

Query-only: `permissions.rs` reads `AVCaptureDevice.authorizationStatusForMediaType` and never calls
`requestAccessForMediaType`, so it never prompts. "Not determined" returns `true`.

### 2.2 A second RHI that contradicts the doctrine

`runtime/streamlib-engine/src/metal/` — 13 files, 2 488 LOC, a parallel Metal RHI (device, command
queue, texture, texture cache, pixel-buffer pool, blitter, format converter, GL interop). `lib.rs:139`
compiles it on every Apple build; `backend-metal` is an empty feature whose only effect is to *turn
Vulkan off*.

This is exactly the "parallel abstraction" `.claude/rules/engine-doctrine.md` names as the
default-wrong move, and per §Structure it is legacy to be replaced rather than a pattern to
accommodate. Its *interop* knowledge — the AVFoundation serialisation quirks recorded at
`metal/rhi/texture_cache.rs:44-56` and `pixel_buffer_pool.rs:44-69` (`CVMetalTextureCacheCreate` and
`CVPixelBufferPoolCreate` must be serialised against `AVCaptureSession` init) — is worth mining
before it goes.

### 2.3 The vendored fork is already macOS-capable

`vendor/tatolab-vulkanalia*` carries `VK_EXT_metal_surface`, `VK_EXT_metal_objects`,
`VK_KHR_portability_subset` and `PhysicalDevicePortabilitySubsetFeaturesKHR` bindings;
`loader.rs:23` dlopens `libvulkan.dylib` on Apple; `window.rs:48-51,175` maps
`RawWindowHandle::AppKit` → `[VK_KHR_surface, VK_EXT_metal_surface]` and calls
`create_metal_surface_ext`. The engine hardcodes a Linux instance-extension list instead of calling
`get_required_instance_extensions`, which is why none of that is reachable. **The fork is not a
blocker.**

### 2.4 Dead macOS code in the Vulkan RHI, already the right shape

`HostVulkanTexture::from_iosurface` (`vulkan_texture.rs:807-866`, `VkImportMetalIOSurfaceInfoEXT`) and
`VulkanSemaphore::from_metal_shared_event` (`vulkan_sync.rs:50-82`, `VkImportMetalSharedEventInfoEXT`)
exist with zero callers, and at least one does not compile. `core/rhi/external_handle.rs:17,23`
already reserves `IOSurface` and `IOSurfaceMachPort` variants under `#[cfg(target_os = "macos")]`.
Treat these as a sketch of the right seam, not a foundation.

### 2.5 The built-ins the MVP needs are Linux-gated

`CameraSource` and `DisplayWindow` are `#![cfg(target_os = "linux")]` at file scope
(`runtime/streamlib-media-builtins/src/{camera_source,display_window}.rs:4`). The wheel refuses both
by name off-Linux (`python_native_builtin_blocks.rs:112-132`). `streamlib new` scaffolds
`CameraSource → InvertingEffect → DisplayWindow`, so **the MVP sentence has no working default on
macOS**, and `--test-pattern` does not rescue it because `DisplayWindow` is still in the graph.

**There is no video device backend trait.** V4L2 is wired directly into `camera_source.rs` — `v4l`
crate plus raw `libc::ioctl`, enumeration through open through format negotiation through the
DQBUF/QBUF loop, all inline. Audio is the opposite and is the template to copy: `AudioDeviceBackend` +
`AudioCaptureStream` + a once-per-process probe chain
(`core/context/audio_device_backend.rs:215-305,378-395`), with `MicrophoneSource` platform-agnostic
above it. **Creating the video seam is the bulk of the capture work**, and per §Structure it means
extending one system — `CameraSource` gets rewritten against the new trait and the V4L2 code moves to
`linux/` as an impl.

### 2.6 Distribution

maturin, **abi3-py310**, one wheel per platform, `requires-python >=3.10`. Nothing binary ships
beyond `_engine.abi3.so` — shaderc/glslang/SPIRV-Tools, VMA and opus are statically linked; the
`streamlib` CLI is pure Python (`streamlib.cli:main`). System libraries are dlopen'd, never linked,
and `tests/test_wheel_portability.py` enforces that with its own ELF walk.

Release is manylinux_2_28 **x86_64 only** (`release-wheel.yml:92`), published to a static PEP 503
index on GitHub Pages. The index generator keys on project name alone
(`scripts/build_simple_index.py:60-62`) — **it needs no change to serve a second platform tag.**

**All 12 CI jobs are `ubuntu-latest`.** `grep -rni 'macos\|darwin\|apple' .github/` returns nothing.
That is the mechanism by which the Apple tree rotted: `xtask`'s `lint-logging` evaluates cfg as if the
target were Linux and skips `apple/` and `metal/` entirely
(`xtask/src/lint_logging.rs:770,1156-1260`), so not even the source-walking gates saw it. README's
claim that macOS engine paths cross-compile, and engine-doctrine's `cargo check --target
aarch64-apple-darwin` rule, are both false as of `0247aff8` — nothing enforces either.

---

## 3. The gap, by concern

**Hard-blocking for camera → display:**

1. The engine does not compile for `aarch64-apple-darwin` (§1.6).
2. No capture on macOS, and no seam to add one behind (§2.5).
3. No present on macOS: the window event pump is Linux-gated and uses
   `EventLoopBuilderExtX11/Wayland::with_any_thread(true)` (`core/window_event_pump.rs:275-289`),
   which has no macOS counterpart because AppKit demands the process's first thread. The file already
   names this as the seam an Apple implementation fills.
4. The camera's ring textures are `acquire_render_target_dma_buf_image` — tiled DRM-modifier DMA-BUF
   gated on an EGL `eglQueryDmaBufModifiersEXT` probe. MoltenVK has none of that. The allocation
   *flavour* needs an Apple arm (IOSurface-backed), which the plan's own language already anticipates:
   importability is "an allocation flavour the engine derives per acquisition", and "a flavour the
   device or format cannot take falls back rather than failing the acquire" (`ARCHITECTURE.md:995`).

**Hard-blocking for a Python processor in the graph — in scope, per the owner (§5):**

5. `ConsumerVulkanDevice` hard-requires five Linux-only extensions and refuses construction otherwise
   (`consumer_vulkan_device.rs:199-211`). The whole surface-share transport is `SCM_RIGHTS` fd
   passing. macOS needs the IOSurface + mach-port arm — for which `surface_store.rs` already has a
   *client* (XPC `check_in`/`check_out`/`register_buffer`/`lookup_buffer`) **and no server**.
6. `MonotonicTimer.__new__` raises on non-Linux ("timerfd does not exist on this platform",
   `python_monotonic_timer.rs:55-59`), so every continuous-execution Python processor is dead on macOS.

**Ships-but-broken if ignored:**

7. No `PR_SET_PDEATHSIG` equivalent → orphaned helpers after a hard app kill.
8. No audio device backend on Apple — the probe chain returns an empty `Vec` and falls to null
   (`audio_device_backend.rs:440`). `apple/audio_clock.rs` is a *timer*, not a device.
9. `xtask check-no-inheritable-descriptor` prescribes `pipe2` / `epoll_create1` / `timerfd_create` /
   `eventfd` (`check_no_inheritable_descriptor.rs:44-83`). Four of those do not exist on Darwin — a
   macOS port needing a pipe **cannot satisfy the gate as written**.

**Out of reach on macOS via Vulkan, and that is fine:**

10. Vulkan Video — the four H.264/H.265 built-ins. Not on the camera→display path (verified: the
    chain is V4L2 → compute colour convert → ring texture → pooled pixel buffer → fullscreen-triangle
    blit). VideoToolbox is the Apple answer when codecs are scheduled; `apple/pixel_transfer.rs` is a
    `VTPixelTransferSession`, i.e. format conversion, not a codec.
11. Ray tracing. Already capability-gated — refuses at construction with a typed error, which is the
    correct macOS behaviour without further work.

---

## 4. The recommended shape

**One RHI, Vulkan, everywhere. Apple SDKs at the edges only.** The evidence for this, rather than for
reviving the Metal backend:

- MoltenVK clears the device bar and every shipped shader is within SPIRV-Cross's reach — no geometry
  or tessellation shaders, no subgroup ops, no 8/16-bit storage, no descriptor indexing, no buffer
  device address in shader code (§1.1, §2.3).
- The entire consumer RHI already compiles (§1.5), and none of the engine's 65 errors is a capability
  gap (§1.6).
- The alternative is a second full RHI — 2 488 LOC today against 89 332 LOC of Vulkan RHI — which is
  the parallel-system move the doctrine forbids, and which would leave every kernel, buffer, timeline
  and present primitive to be written twice forever.

Where Apple SDKs genuinely earn their place, each one an edge rather than a layer:

| Concern | Apple SDK | Meets Vulkan at |
|---|---|---|
| Capture | AVFoundation `AVCaptureSession` + `AVCaptureVideoDataOutput` | `CVPixelBuffer` → `IOSurfaceRef` → `VkImage` via `VK_EXT_metal_objects` (§1.2) |
| Present | AppKit / `CAMetalLayer` | `vkCreateMetalSurfaceEXT` → ordinary `VkSwapchainKHR` (§1.3) |
| Cross-process surfaces | IOSurface + mach ports over a raw Mach channel (§10) | `VkImportMetalIOSurfaceInfoEXT`; timeline semaphores via `MTLSharedEvent`, proven 1:1 (§11.1) |
| Main-thread pump | AppKit / NSApplication | winit on the main thread, below the existing pump seam |
| Permissions | TCC / `AVCaptureDevice.requestAccessForMediaType` | — |
| Clock | `mach_absolute_time` | already shipped and correct |
| Codecs (post-MVP) | VideoToolbox | replaces Vulkan Video, which MoltenVK lacks |
| Audio (post-MVP) | CoreAudio / AudioUnit | the existing `AudioDeviceBackend` trait |

**The Metal RHI should be deleted, not revived** — after mining `texture_cache.rs` and
`pixel_buffer_pool.rs` for their AVFoundation serialisation constraints. That is a `REMOVED:` bullet,
not a drive-by.

---

## 5. Where the MVP line falls

The narrowest honest definition of "working again on macOS" is the plan's own MVP sentence with Linux
swapped for Apple Silicon: **`pip install streamlib` → `streamlib new` → `streamlib dev` → your
camera live in a window within a minute, zero ceremony.**

The tempting narrow line is `examples/camera-display` — `CameraSource → DisplayWindow`, two native
built-ins, no Python processor in the graph, and therefore no cross-process pixel path at all.

**That line was considered and rejected by the owner, 2026-09-19.** `streamlib new` scaffolds
`CameraSource → InvertingEffect → DisplayWindow`, and `InvertingEffect` is a Python processor that
reaches the frame through `resolve_surface` / `as_numpy` — i.e. it reads GPU memory belonging to
another process. A macOS build where the *scaffolded default app* does not run is not "working
again"; it is a demo. Cross-process frames over raw Mach + IOSurface are **in scope for this milestone**,
not deferred. Owner's words: this is a must for the framework, and it should be cheaper now than in
the pre-pivot framework, where the same capability needed a launchd-registered service — helper
processes are first-class today and XPC can be established between a parent and a child it spawned
without installing anything. *(The conclusion held and the mechanism did not: §10.1 found that XPC
cannot in fact reach a spawned child service-lessly, and raw Mach is what delivers the
no-installation outcome. The owner's framing is kept verbatim above because it is what set the
scope.)*

So the MVP line is the plan's own MVP sentence, unmodified, on Apple Silicon:

- **In scope**: engine compiles and links on macOS; Vulkan-on-MoltenVK instance/device/loader;
  AVFoundation capture behind a new video device seam; IOSurface-backed ring allocation; winit pump on
  the main thread; `CAMetalLayer` present; camera permission handled honestly; **cross-process frames
  to helper processes over raw Mach + IOSurface, including the CPU-mapped numpy view**; an
  `aarch64-apple-darwin` wheel and CI lane.
- **Deliberately out**: codecs (Vulkan Video does not exist on MoltenVK — VideoToolbox is a separate
  milestone); audio; ray tracing; the virtual camera; `MonotonicTimer` (so continuous-execution
  Python processors lag one release); the six surface adapters.

The one thing that shrinks rather than disappears: `MonotonicTimer` raising on non-Linux means the
scaffolded effect must be a reactive/manual processor, which it already is. Worth confirming before
the milestone is sized.

---

## 6. What this does not decide

Extending the platform floor is architecture. Per `.claude/rules/engine-doctrine.md`, a missing
decision stops work and goes to the owner; it is never inferred from the tree. The specific decisions
this memo surfaces and does **not** take:

0. ~~**Distribution shape.**~~ — **Decided by the owner, 2026-09-19.** The artifact is the
   pip-installable wheel served from this repo's index, **identical to the Linux approach**. No `.app`
   bundle, no launchd, no installer — under any justification. `brew` is acceptable for build-time
   dependencies only, never as end-user setup. macOS security prompts are expected and accepted;
   needing a *bundle* to get them is not. Embedding StreamLib inside someone else's application, and
   a future iOS library, are out of scope: we are an engine / runtime / SDK, not the end product.
   **This retires two fallbacks this memo had left open** — the `.app` re-exec for the TCC terminal
   lottery (§8) and the bundled XPC service for the shared-event seam (§11.1 route 3). Both are off
   the table; what remains must work from a plain unbundled process.
1. **Is macOS a supported floor, or a developer-machine floor?** "Contributors on Apple hardware can
   run the engine" and "a Mac user gets the Linux user's install experience" are different products
   with different CI, release and support costs.
2. **Vulkan-everywhere vs reviving Metal.** §4 recommends the former with evidence; the call is the
   owner's.
3. **Delete `src/metal/`?** Follows from (2), but is its own `REMOVED:` bullet.
4. ~~**Does the macOS MVP include a Python processor in the graph?**~~ — **Decided by the owner,
   2026-09-19: yes.** Cross-process frames over raw Mach + IOSurface are a must for the framework and are
   in this milestone, not deferred. See §5.
5. **Does the camera carry the device's capture timestamp or the publish timestamp?** The camera
   currently stamps publication with `MediaClock::now()` (`camera_source.rs:1152`) and never reads the
   V4L2 buffer stamp, while audio stamps capture and says so
   (`microphone_source.rs:334-338`). AVFoundation hands back a real capture PTS, so the question
   becomes unavoidable — and answering it changes the camera's behaviour on **both** platforms.
6. **x86_64 macOS (Intel), or Apple Silicon only?**

---

## 7. What remains unknown

- ~~**TCC for an unbundled interpreter.** …the single largest risk to the "delightful install"
  claim.~~ — **Superseded 2026-09-19 by §8, measured on this machine.** It is not a blocker: the
  terminal emulator is the TCC subject, not our process, and every mainstream terminal already ships
  a camera usage string for exactly this. What replaces it is smaller and entirely ours to fix —
  see §8.
- **The second and third waves.** §1.6 measures compile errors only. Linking (framework flags,
  `@rpath`) and first run (MoltenVK discovery, validation layers, device-lost behaviour) are unmeasured.
- ~~**Vulkan loader vs direct MoltenVK in the wheel**, and the signing / notarisation rules for a
  dylib arriving inside a wheel.~~ — **Superseded 2026-09-19 by §9, measured.** Bundle both loader
  and MoltenVK; nothing pip delivers is quarantined, so no notarisation is needed.
- ~~**GitHub Actions Apple Silicon runners.**~~ — **Superseded 2026-09-19 by §9.** `macos-latest` is
  macOS 26 arm64 and free for public repos.
- ~~**How an `MTLSharedEvent` reaches a helper without a launchd-registered XPC service.**~~ —
  **Resolved 2026-09-19 (§11.1a):** a public `NSXPCCoder` subclass extracts the handle's mach send
  right, which crosses the existing raw Mach channel. 500 device-side cross-process round trips
  proven. Rated 7/10 durable, with host-side ordering (+26–100 µs/frame) wired behind it.
- **Whether the wheel crate itself compiles.** Every measurement so far died in `streamlib-engine`
  first. `python_helper_process_pixel_exchange.rs` has 119 `cfg(linux)` items and zero `cfg(not(linux))`
  arms, yet is imported ungated — expect a second wave there. This is now the largest unmeasured
  surface, because it is exactly the code the cross-process pixel path lives in.
- **Performance.** Nothing here measures a frame. MoltenVK's translation cost on the camera→display
  path, and the cost of an IOSurface crossing a process boundary per frame, are unknown and
  unbudgeted. The Linux CPU path is tuned around write-combined memory reading at ~175 MB/s; the
  Apple equivalent's characteristics on unified memory are not yet known, and the scaffolded effect's
  performance advice is written against the Linux number.

---

## 8. Camera permission (TCC) — settled, and it is not the blocker I thought

Measured on this machine by reading `tccd`'s own `AttributionChain` / `AUTHREQ_SUBJECT` log lines
across four probe binaries under Terminal.app, iTerm2, and a `.app` launched via LaunchServices.

**We are never the TCC subject, and we never will be from a terminal. The terminal emulator is.**
`tccd` walks the responsibility chain up to the GUI host and attributes the request there. Raw log,
Homebrew Python under iTerm2:

```
AttributionChain: responsible={identifier=com.googlecode.iterm2, …},
                  accessing={identifier=org.python.python, pid=31575, …}
AUTHREQ_SUBJECT:  subject=com.googlecode.iterm2
```

Consequences, all measured:

- **Nothing needs to ship for the happy path.** No `Info.plist`, no `-sectcreate` embedded plist, no
  `.app`, no Developer ID. The same binary with the same embedded bundle id returned `authorized`
  under iTerm2 and `notDetermined` under Terminal.app — **the embedded plist never became the
  subject in any configuration.** It buys a stable code-signing designated requirement, which is a
  different thing from a TCC identity.
- **Every mainstream terminal already ships a camera usage string for exactly this** — iTerm2,
  Ghostty, Zed, Cursor, VS Code all carry one worded as a proxy for their children. Terminal.app
  ships none and prompts anyway via the Apple-private `com.apple.private.tcc.allow-prompting`
  entitlement, which we cannot obtain and do not need.
- **Child processes inherit the grant, the responsible process and the subject** through plain
  `posix_spawn`. Our helper-process model costs nothing here. There is therefore no camera argument
  for co-hosting — the placement ban is unaffected.
- **A Homebrew Python upgrade does not invalidate the grant**, and neither does a venv symlink,
  because the interpreter's identity never enters the decision. This inverts the usual folklore.
- **Apple's documented "the system terminates your app" for a missing usage string did not fire**
  for an unbundled binary — that behaviour is bundle-only. Do not use the absence of a crash as
  evidence of correct configuration.

What replaces the risk, smaller and entirely ours:

1. **`requestAccess` never returns while the prompt is pending.** Measured: the completion handler
   did not fire inside an 8-second bound and status stayed `notDetermined`. A `streamlib dev` that
   awaits it on the critical path looks hung with no output. **Never block the pipeline on it.**
2. **The diagnostic must name the responsible app, not us.** On `denied`/`restricted` we must tell
   the user to enable *iTerm2* (or whichever host) in System Settings › Privacy › Camera. Telling
   them to look for "python" sends them hunting for an entry that will never exist — **the Camera
   pane has no "+" button**, so an app that has never prompted cannot be added by hand.
3. **Never daemonise the engine.** Detaching breaks the responsibility chain and silently costs
   camera access. Treat it as a standing constraint.

**Do not ship `responsibility_spawnattrs_setdisclaim`.** It works on 26.3 and actively makes things
worse: it converts a stable bundle-id grant into one keyed on the binary's absolute path, which dies
on every venv move, Python upgrade and rebuild.

The residual failure is the **terminal lottery** — a host that has never successfully prompted leaves
the user stuck with no manual recovery, because the Camera pane has no "+" button.

~~The sanctioned fallback is a small Developer-ID-signed, notarized `.app` shipped in the wheel that
we re-exec into via LaunchServices.~~ — **Ruled out by the owner, 2026-09-19 (§6 item 0).** So this
residual has **no fallback and is accepted**: it is a property of how macOS attributes permission to
the GUI host, not something the wheel can fix. What we owe instead is a good diagnostic — name the
responsible app, say what to enable, and point at "run it from a different terminal" as the recovery.
Every mainstream terminal already carries the usage string, so the failure is rare rather than
theoretical, and it costs the user a retry rather than a broken install.

Separately: an Apple **virtual camera** is a CoreMediaIO Camera Extension — a sandboxed system
extension embedded in a notarized `/Applications` app, admin-approved. It cannot ship inside a wheel
in any meaningful sense. The Linux `VirtualCameraSink` has no port; it has a successor product.

---

## 9. Shipping the wheel — settled

**Gatekeeper is a non-issue, verified three ways on this machine.** pip does not set
`com.apple.quarantine` (it fetches over its own HTTP stack and unzips with `zipfile`, never touching
LaunchServices) — a pip-installed extension carries only `com.apple.provenance`. And Gatekeeper
evaluation is *triggered by quarantine*. The experiment:

| dylib state | `dlopen` |
|---|---|
| ad-hoc signed, no quarantine — **as pip delivers** | **loads** |
| ad-hoc signed, quarantine xattr set | hangs indefinitely in the kernel signature path |
| signature removed | fails: *"Trying to load an unsigned library"* |

So: **no notarization, no Developer ID, no signing secrets in CI.** Apple's linker ad-hoc signs every
arm64 output automatically, so a wheel built on a GitHub arm64 runner is already signed.

**The one real hazard is a post-link byte rewrite that forgets to re-sign.** `install_name_tool`,
`strip`, `lipo -thin` or `codesign --remove-signature` all invalidate the signature, and the result
fails for *every user* and *never* on the builder. Any vendoring step must end in
`codesign -f -s - <file>`.

Other settled facts:

- **maturin ≥ 1.13 has a built-in delocate equivalent, on by default** — but it discovers
  dependencies from `LC_LOAD_DYLIB` only, so **it will never find a library we `dlopen`. Bundling
  MoltenVK is our job.** That is fine: `dlopen` with a path containing a slash bypasses
  `@rpath`/install-name resolution entirely, so no Mach-O surgery is needed at all.
- **Platform tag**: `MACOSX_DEPLOYMENT_TARGET=12.0` → `cp310-abi3-macosx_12_0_arm64`. The floor is set
  by MoltenVK (v1.4.2 raised its runtime minimum to macOS 12), not by Python. maturin derives the
  arm64 default from rustc and will only ever raise it, so pin it in `pyproject.toml`.
- **arm64-only, not universal2.** pip prefers the more specific tag anyway, universal2 doubles the
  wheel, every bundled dylib would have to be fat, and maturin's own source notes universal2 support
  may be removed when Apple drops x86_64.
- **MoltenVK v1.4.2** ships a canonical prebuilt universal dylib plus its ICD manifest, Apache-2.0,
  ad-hoc signed by Khronos (`spctl` rejects it — it is simply never asked). Thinned to arm64 and
  stripped it is **~1.4 MB of wheel growth**, and every one of its dependencies is under `/System/`
  or `/usr/lib/`.
- **Recommended: bundle the Vulkan loader *and* MoltenVK, not MoltenVK alone**, and set
  `VK_ADD_DRIVER_FILES` (additive, so a user's own driver still works) at import time. The delta is
  one small dylib and a JSON file. It buys layers as an opt-in extra, multi-ICD enumeration, and —
  decisively — a migration path to **KosmicKrisp**: LunarG's Vulkan-on-Metal driver in Mesa, MIT,
  **fully Vulkan 1.3 CTS conformant**, macOS 26+ Apple Silicon, already shipping in the LunarG SDK
  beside MoltenVK. Hard-wiring MoltenVK dead-ends on a driver its own README calls non-conformant.
- **Going through the loader to a portability driver requires enabling
  `VK_KHR_portability_enumeration` + `VK_INSTANCE_CREATE_ENUMERATE_PORTABILITY_BIT_KHR` at instance
  creation and `VK_KHR_portability_subset` at device creation.** The engine has the first two in a
  block that has never compiled and the third on the macOS arm already.
- **CI**: `macos-latest` now maps to **macOS 26 arm64**; `macos-14` is deprecated. Standard runners
  are **free for public repos**; for private repos macOS is **~10× the Linux per-minute rate**, so
  pin a label and keep the macOS lane narrow.
- **`test_wheel_portability.py` silently `pytest.skip`s a non-ELF binary** — on macOS the coverage
  does not fail, it *evaporates*. It needs a Mach-O arm reading `LC_LOAD_DYLIB` (skipping the
  leading `LC_ID_DYLIB` line for a dylib), asserting the same `/usr/lib/**` + `/System/**` prefix
  policy that maturin and delocate both use. Two assertions worth adding that the ELF side does not
  need: **every Mach-O in the wheel carries `LC_CODE_SIGNATURE`** (the direct guard against the
  forgot-to-re-sign failure above), and `LC_BUILD_VERSION.minos ≤ 12.0`. Note that on macOS 11+ the
  system dylibs **do not exist on disk** — they live in the dyld shared cache — so the test must
  match load-command strings and never `stat` a path.

---

## 10. Cross-process frames — the transport half

### 10.0 Scope: this replaces the surface-share socket, not iceoryx2

Worth stating first, because it decides how much of this matters. The Mach channel is **only the
handle-exchange path** — the Apple peer of the per-runtime `surface-share-<runtime_id>.sock` and its
`SCM_RIGHTS` fd passing. **iceoryx2 remains the data plane on macOS exactly as on Linux**, and it
already works there unmodified (§1.4).

The split is already the shape of the tree, so nothing about it is new:

- **Per frame**, a bag carries a `surface_id` *string* over iceoryx2. No handle, no fd, no port.
- **Per pool slot, once**, the producer registers the buffer with the surface-share service and a
  consumer checks it out — that is where the fd (Linux) or mach port (Apple) crosses. Confirmed in
  the tree: `register_buffer` is called from the pool's pre-allocation loop
  (`gpu_context.rs:448`, `:573`), once per slot, not from any per-frame path.

So the Mach channel is a **low-frequency control path**, touched a couple of dozen times at
setup and then effectively never. Two consequences:

- **Transport latency is irrelevant.** The ~4 µs Mach round-trip and the ~5–6 µs port handoff are
  measured for completeness, not because anything depends on them. The per-frame cost on macOS is
  iceoryx2 (proven) plus an already-imported, per-slot-cached `VkImage` plus a timeline-semaphore
  wait — none of which touches Mach.
- **The rendezvous choice in §10.2 is a security and lifecycle decision, not a performance one.**

The honest answer to "so we should be good?" is **yes**, with the specifics in §11: the produce/consume
timeline contract crosses processes intact, import costs 4–18 µs and is cached per slot, and the CPU
view is *faster* than the Linux one. The pre-iceoryx2 arrangement pushed frame data through this
channel; nothing does now.

### 10.1 The mechanism is raw Mach, not XPC

Measured on this machine with eighteen probe programs (`/tmp/xpcprobe/`), including a full
parent → child IOSurface handoff that reads back the pixel the parent wrote.

**The headline correction: the mechanism is raw Mach, not XPC.** The instinct that XPC no longer
needs a whole installed service is right about the *outcome* and wrong about the *route*.

- `xpc_connection_create(NULL, q)` does make an anonymous listener, and `xpc_endpoint_create` does
  hand out a connectable endpoint — **proven working in-process, off the main thread, with no
  runloop.** But an endpoint **cannot be serialised to bytes**: `NSXPCListenerEndpoint` conforms to
  `NSSecureCoding` yet throws *"This class may only be encoded by an NSXPCCoder"*, and there is no
  public `xpc_endpoint` → data API. An endpoint can only travel over an XPC channel that already
  exists. For a child that does not exist yet, that is circular.
- `xpc_connection_create_mach_service(..., XPC_CONNECTION_MACH_SERVICE_LISTENER)` cannot adopt a
  dynamically registered name — the header says so outright and it fails `Operation not permitted`.
- Replacing the child's bootstrap port at spawn **breaks the child**: its `bootstrap_look_up` fails,
  taking libxpc, fonts and notifyd with it. Do not.

So to reach a spawned child you must bootstrap raw Mach anyway — and once you have, wrapping it back
in XPC only adds message framing we do not need for fixed-shape frame descriptors, while giving up
the public audit trailer and dead-name notifications. **Raw Mach is both the shorter path and the
better one.** Round-trip latency measured at **~4.0 µs**, against ~4.3–5.1 µs for a socketpair — no
performance reason to prefer sockets either.

**What the user does that a Linux user does not: nothing.** No launchd, no plist, no bundle, no
first-run daemon, no OS-specific documentation. Confirmed from an ad-hoc-signed, unbundled,
terminal-launched, non-sandboxed `python3` — both Homebrew's and Apple's.

### 10.2 Two service-less rendezvous, and the tradeoff between them

| | `mach_ports_register` stash | dynamic `bootstrap_check_in` name |
|---|---|---|
| Name needed | **none** — capability-style | a unique string, passed by argv/env |
| Discoverable by other processes | **no** | **yes** — `launchctl print gui/$UID` lists it, and any process in the user domain can look it up and connect |
| Survives `posix_spawn` | yes | yes |
| Survives `fork()`+`execve` / default `subprocess.Popen` | **no — the stash is dropped at exec** | yes |
| Documented | yes — xnu `task.defs`: *"child tasks inherit this stash at task_create time"* | no; undocumented but shipped in production by Chromium and servo/ipc-channel |

**But our current spawn path cannot use the stash, and that is measured, not assumed.**
`python_helper_process_spawn_host.rs` builds the child with `std::process::Command` and registers
**two `pre_exec` closures** (`:925` sets up the process group, `:989` sweeps descriptors past stdio).
Rust's standard library bails out of `posix_spawn` whenever any closure is registered — from the
source shipped with this toolchain
(`library/std/src/sys/process/unix/unix.rs:460-468`):

```rust
if self.get_gid().is_some()
    || self.get_uid().is_some()
    || (self.env_saw_path() && !self.program_is_path())
    || !self.get_closures().is_empty()      // ← ours
    || self.get_groups().is_some()
    || self.get_chroot().is_some()
{ return Ok(None); }                         // ← falls back to fork + exec
```

So today the helper is **fork + exec**, and the stash would be dropped at `execve`. Two ways out:

- **Take the bootstrap-name path.** Spawn-agnostic, no change to the spawn host, matches what
  Chromium and servo ship. Cost: the name is listed in the user's launchd domain, so an
  audit-token check on every connection becomes **mandatory** rather than defence in depth.
- **Move the spawn to `posix_spawn` directly** and re-express both closures as spawn attributes.
  This is more attractive than it first looks on macOS, because both jobs have native equivalents:
  `POSIX_SPAWN_SETPGROUP` + `posix_spawnattr_setpgroup` replaces the process-group closure, and
  **`POSIX_SPAWN_CLOEXEC_DEFAULT`** (an Apple extension, `sys/spawn.h:62`) makes *every* descriptor
  close-on-exec by default, with `posix_spawn_file_actions_addinherit_np` naming the few that should
  survive — which is strictly stronger than the sweep loop and aligns with the repo's own
  CLOEXEC-at-source gate. It also sidesteps the `close_range`-is-absent note the plan already carries
  at `ARCHITECTURE.md:822`.

**Recommendation: the bootstrap-name path for the first landing**, because it does not perturb a
load-bearing spawn path in the same ticket as a new transport, with the `posix_spawn` restructure as
a follow-up that then unlocks the capability-secure stash. The audit-token check is worth having
either way. **This is a genuine fork and belongs in the change as a `[NEEDS DECISION]`.**

Two sharp edges on the stash: there are only **three slots** and **slot 0 already holds the
process's launchd port**, so it must be read-modify-written or the child loses launchd entirely; and
the slot should be cleared immediately after the spawn so later children do not inherit it.

### 10.3 What the frame rides on

`IOSurfaceCreateMachPort` / `IOSurfaceLookupFromMachPort` — Apple's own header states the intent:
*"securely pass an IOSurface to another task without making the surface global."* Proven end to end.
`IOSurfaceCreateXPCObject` / `IOSurfaceLookupFromXPCObject` is the equivalent when you already hold an
XPC dictionary; also proven, but moot under the raw-Mach recommendation. Plain fds cross via
`fileport_makeport` / `fileport_makefd`, the Mach peer of `SCM_RIGHTS`.

**`kIOSurfaceIsGlobal` is banned.** Measured: a global surface's id resolves from *any* process on the
machine — it leaks frames system-wide. It is also `API_DEPRECATED("Global surfaces are insecure")`. A
private surface's id is resolvable only by a process already holding a port to it, which is the
property we want.

### 10.4 Lifecycle, and one real hazard

- Child death reaches the parent as `MACH_NOTIFY_DEAD_NAME` in **~0.05 ms**. Parent death reaches the
  child as `MACH_NOTIFY_NO_SENDERS` plus a dead-name notification. Both are prompt and reliable, and
  an inherited socketpair gives a belt-and-braces EOF for any event loop.
- **A surviving child keeps an IOSurface alive after the parent dies** — measured: the orphan still
  read its pixel and could re-look-up the surface. Unlike Linux, where closing the fd is enough, the
  child must explicitly release on parent-death or **GPU memory leaks**. This needs a teardown
  handler wired to the notification, and it belongs in the ticket.

### 10.5 Threading

**None of this needs the main thread or a CFRunLoop** — proven with `main()` blocked in
`pthread_join`. A Mach receive right can be driven from `EVFILT_MACHPORT` on the same kqueue mio
already owns, which is the clean tokio bridge, or from a private GCD queue. The AppKit/winit
main-thread pump is untouched.

### 10.6 Security without entitlements

An ad-hoc-signed binary has no Team ID and no certificate chain, so `anchor apple generic` and
team-identifier requirements are unusable — measured. The only requirement that pins an ad-hoc peer is
its **cdhash**. For a self-spawned child of the same binary the simpler and sufficient check is the
**audit token**: `audit_token_to_pid` against the pid we spawned, plus `audit_token_to_pidversion` to
defeat pid reuse — exactly Chromium's check. On the raw-Mach path the audit trailer is public
(`MACH_RCV_TRAILER_AUDIT`); on the XPC path `xpc_connection_get_audit_token` is SPI, which is one more
reason the raw path is cleaner.

### 10.7 What this means for the tree

`apple/xpc_ffi.rs` already declares `xpc_dictionary_set_mach_send` / `copy_mach_send` and
`xpc_connection_create_mach_service_listener`, and `surface_store.rs` already carries an XPC *client*
for check-in/check-out/register/lookup with no server behind it. That client was written against the
XPC-service shape this research rules out for a spawned child. **Expect to keep the IOSurface and
mach-port halves and re-point the connection layer at raw Mach** — the surface-store call shape
survives, the transport underneath it does not.

---

## 11. Cross-process frames — the GPU half

Measured on this machine against MoltenVK 1.4.0, with MoltenVK's source read at `v1.4.0` to explain
each result. Two processes, each with **its own `VkInstance` and `VkDevice`**.

### 11.1 The ordering contract survives intact — this was the open question

**A Vulkan timeline semaphore crosses the process boundary with its values 1:1.** MoltenVK backs every
timeline semaphore with an `MTLSharedEvent` on Apple Silicon; `VkExportMetalObjectCreateInfoEXT`
+ `vkExportMetalObjectsEXT` yields the `MTLSharedEvent`, `newSharedEventHandle` carries it over XPC,
and the child re-imports it with `VkImportMetalSharedEventInfoEXT`. The Vulkan value *is* the Metal
`signaledValue` — signal, wait, `vkGetSemaphoreCounterValue` and `vkWaitSemaphores` all map straight
through.

A 500-round producer/consumer ping-pong over a shared `produce_done` / `consume_done` pair, the exact
shape of the Linux contract, ran clean in every mode:

| mode | result | per round |
|---|---|---|
| both host-side `vkWaitSemaphores` | **0 mismatched frames / 500** | 824 µs |
| both device-side waits in `vkQueueSubmit` | **0 / 500** | 561 µs |
| server host-wait, client device-wait | **0 / 500** | 696 µs |
| server device-wait, client host-wait | **0 / 500** | 853 µs |
| server Vulkan, client pure Metal | **0 / 500** | 497 µs |

`maxTimelineSemaphoreValueDifference` is `2^64-1`; host wake latency after a CPU signal is 20 µs mean,
69 µs max. **So `produce_done`/`consume_done` needs no redesign — only a different export call.**

> **✅ RESOLVED 2026-09-19 — a public-API, service-less, bundle-free route exists and is proven.**
> Jump to §11.1a. The analysis below is kept because it explains why every *obvious* route fails and
> why the working one looks the way it does.
>
> **⚠ The seam as it stood before the spike.**
> The ping-pong above moved its two `MTLSharedEventHandle`s over **NSXPC hosted by a launchd
> LaunchAgent**, because `xpc_connection_create_mach_service(…, LISTENER)` refuses a dynamically
> registered name (§10.2) and the probe needed *some* XPC channel. **We cannot ship a LaunchAgent.**
>
> The IOSurface itself is fine — it crosses over raw Mach, proven (§10). The event handle is the
> problem, and the SDK is unhelpfully narrow about it. `MTLSharedEventHandle`'s entire public surface
> is `NSSecureCoding` conformance and a `label` property; its header says the object *"may be passed
> between processes via XPC connections"* and names no other route. There is **no public constructor
> from a mach port and no public port accessor** — confirmed by reading every Metal header in the SDK.
> `NSKeyedArchiver` on it throws *"This object may only be encoded by an NSXPCCoder"*. Its `_priv`
> ivar is `{mach_port_t, label, trace_id}` and there is a private `-eventPort`, but private API is not
> shippable. `xpc_endpoint_t` has no public port accessor either, so an endpoint cannot be smuggled
> over the raw Mach channel to bootstrap a real XPC connection.
>
> Three candidate routes, none tested:
> 1. **Find a service-less way to stand up one real XPC connection** between parent and child, then
>    send the handle over it as `NSSecureCoding` intends. This is the clean answer if it exists.
> 2. **Order host-side instead.** Drop cross-process GPU semaphores and have the child CPU-wait before
>    submitting, signalling completion over the channel we already have. Correctness is preserved;
>    the cost is a CPU hop per frame where Linux has none. The measured host-wait ping-pong mode
>    (824 µs/round including 16 MB of GPU traffic) suggests this is survivable at camera cadence, and
>    the Linux design already supports host-side waits as a mode — but it is a real regression and it
>    would need to be stated in the plan, not slipped in.
> 3. ~~Ship a `.app` bundle and register a real XPC service inside it.~~ — **Ruled out by the owner,
>    2026-09-19 (§6 item 0): no bundle, under any justification.**
>
### 11.1a How the shared event actually crosses — measured, service-less, no bundle

**`NSXPCCoder` is a public Foundation class** (`NSXPCConnection.h:212`) with public
`-encodeXPCObject:forKey:` / `-decodeXPCObjectOfType:forKey:`, and
`xpc_dictionary_set_mach_send` / `copy_mach_send` are public in `xpc.h`. `MTLSharedEventHandle`
gates on `isKindOfClass:NSXPCCoder` — **it never consults the coder's connection**, which is nil in a
subclass. So a subclass of the public coder is accepted with no XPC connection anywhere:

**Parent** — export the `MTLSharedEvent` from the Vulkan timeline semaphore, `newSharedEventHandle`,
`encodeWithCoder:` into the coder subclass (which observes one `encodeXPCObject:` of a mach send
right under key `Port`, plus an `NSString` label), then `xpc_dictionary_copy_mach_send` to get a
plain `mach_port_t`, and send it over the same raw Mach channel the IOSurface already uses.
**Child** — `xpc_dictionary_set_mach_send`, replay it from `-decodeXPCObjectOfType:forKey:`,
`[[MTLSharedEventHandle alloc] initWithCoder:]`, `newSharedEventWithHandle:`, then
`VkImportMetalSharedEventInfoEXT`.

Proven end to end: the parent `posix_spawn`s the child, hands over the port, and the two run **500
device-side cross-process round trips at 120.6 µs each** with no CPU in the loop — both semaphores
finishing at the expected counter. The data path was verified alongside it: a 1920×1080 BGRA
IOSurface imported as a `VkImage` in both processes, 8 MB copied each way per frame, pixel checked
and matching every run.

Every other route stays dead, and now precisely: `xpc_endpoint` has no public port accessor
(`copy_mach_send` on one returns null, and faking it **crashes the process**);
`NSXPCListenerEndpoint` throws even for an `NSXPCCoder` subclass because it checks the *concrete*
coder; `xpc_connection_create_mach_service(LISTENER)` and the macOS 14+ `xpc_listener_create` both
refuse a dynamic name; and no Metal API names or looks up an event by token.

**Rate it public-but-undocumented — roughly 7/10 durable, and build the fallback in behind the same
interface.** What is load-bearing is not the API (public) but the *encoded shape*: exactly one mach
send right plus a label. A future OS that encoded an `xpc_endpoint` instead would break it, and
endpoints are not extractable. The failure mode is safe — a bogus or dead port yields
`newSharedEventWithHandle:` returning nil, not a crash — so the swap to host-side ordering can be a
runtime fallback rather than a compile-time choice.

**And the fallback is cheap enough that this is not a gamble.** Measured head to head at 1920×1080,
producer submit → consumer observes completion:

| | device-side | host-side | delta |
|---|---|---|---|
| 60 fps, mean | 1144 µs | 1215 µs | **+71 µs** |
| 60 fps, p99 | 1320 µs | 1415 µs | +95 µs |
| 30 fps, mean | 1266 µs | 1292 µs | **+26 µs** |

**Under 1% of a 16.7 ms frame budget**, and run-to-run noise is the same order as the delta. Unpaced
throughput is 1718 fps device-side against 1340 fps host-side. So: take the device-side route, keep
host-side wired behind it, and neither outcome threatens the milestone.

### 11.1b A hard constraint the spike found — this one is a design rule

**A committed command buffer whose GPU-side wait on a shared event is not satisfied within ~5 s is
killed by IOGPU, and MoltenVK marks the `VkDevice` lost** — unrecoverable without
`MVK_CONFIG_RESUME_LOST_DEVICE`:

```
T=5.0s: [mvk-error] ... kIOGPUCommandBufferCallbackErrorTimeout ... VK_ERROR_DEVICE_LOST
```

This is a Metal/IOGPU property rather than anything cross-process, but cross-process is where a peer
can stall — the spike hit it for real when its child blocked on a full socket. **A slow or wedged
Python helper must never be able to take the engine's device down.** The rule that falls out: never
let the producer GPU-wait on a consumer that may stall. The buffer-free direction is host-side with a
timeout, or bounded by a watchdog well under 5 s. This belongs in the ticket, not in the
implementer's discretion.

Two rules fall out, both measured:

- **Binary semaphores do not cross.** MoltenVK allocates a shared event for an exported binary
  semaphore but never returns it — export yields nil. `vkGetPhysicalDeviceExternalSemaphoreProperties`
  returns all-zero for every handle type. Timeline-only, by construction.
- **Values are monotonic for the event's lifetime and importing can *raise* the counter.** Importing
  with `initialValue = 0` is a no-op (correct); importing with a larger value silently advances the
  shared counter. Importers must pass zero, and a recycled slot must **advance** its values, never
  reset them.

### 11.2 Import, proven across processes

Parent creates the IOSurface, imports it as a `VkImage`, GPU-writes a pattern, hands over a mach port;
child looks it up, imports into *its own* device, GPU-reads and CPU-reads it back. **0 mismatches**,
repeatedly. Import cost after warm-up: `vkCreateImage` **4–18 µs**, allocate+bind **7–27 µs**.

There is **no device-UUID or LUID matching requirement** — `VkImportMetalIOSurfaceInfoEXT` is not a
`VK_KHR_external_memory` handle type and carries only an sType VU. IOSurface is device-agnostic by
design. (Multi-GPU Macs untested.)

MoltenVK's import check is narrow: width, height, bytes-per-element against the `VkFormat`'s block
size, and per-plane geometry. **Nothing else** — not pixel format, tiling, usage or layout. Fifteen
formats round-tripped cleanly, including `420v` biplanar per-plane. Note the sharp edge: **the
`VkFormat` decides byte order, and the IOSurface `OSType` is metadata only** — the same BGRA surface
read as `B8G8R8A8_UNORM` and as `R8G8B8A8_UNORM` gives reversed bytes, with no error.

### 11.3 Three traps that would each have cost a day

1. **Bind memory type 0, never the host-visible type.** Binding host-visible eagerly allocates a real
   8 MB `MTLBuffer` per image — **859 µs and +8 MB RSS each** (100 pooled frames → 840 MB) — and arms
   a pull-on-barrier path. Type 0 costs 6.6 µs and nothing.
2. **`vkMapMemory` on the image's memory is not the frame.** MoltenVK hands back a pointer into its
   own buffer; writes on either side are invisible to the other. The CPU view must come from the
   IOSurface.
3. **Import the IOSurface, never an `MTLTexture`.** `VK_EXT_external_memory_metal`'s MTLTEXTURE import
   is broken in 1.4.0 and still on `main` (it overwrites the imported texture), and MoltenVK #2705
   shows MTLTexture imports miss residency registration. The IOSurface path calls `makeResident`.

And one to avoid: **MoltenVK sets `kIOSurfaceIsGlobal` on every IOSurface it creates itself** — the
deprecated, system-wide-readable flag §10 bans. Always allocate the surface ourselves.

### 11.4 The CPU path is *better* than Linux, not worse

The Linux path reads write-combined memory at ~175 MB/s, which is why the scaffolded effect does a
bulk `pixels.copy()` instead of editing in place. **On Apple Silicon the default IOSurface mapping is
cached and reads at heap speed:**

| mapping | NEON read | memset write |
|---|---|---|
| `malloc` baseline | 49.6 GB/s | 87.5 GB/s |
| **IOSurface, default cache mode** | **49.8–51.4 GB/s** | 177–186 GB/s |
| `kIOSurfaceMapWriteCombineCache` | 0.24 GB/s | 111 GB/s |
| `kIOSurfaceMapInhibitCache` | **SIGBUS** on NEON loads | — |

So the write-combined pathology exists only if we opt into it. **Do not set a map cache mode.** The
scaffolded effect's performance comment is Linux-specific and will read as wrong on a Mac.

Coherency needs nothing special on unified memory: 50 rounds of CPU-write → GPU-read and GPU-write →
CPU-read, without locks, gave 0 mismatches. Keep the `IOSurfaceLock`/`Unlock` bracket anyway — it costs
**~0.58 µs** and it is what keeps the code correct on a discrete-GPU Mac.

There is also a path that lets the *existing* Rust mapped-allocation code survive almost unchanged:
**`VK_EXT_external_memory_host` import of `IOSurfaceGetBaseAddress()`**. `minImportedHostPointerAlignment`
is 16384 and IOSurface base and size are both 16 KiB-aligned, so it fits; `vkMapMemory` then returns
**the IOSurface base pointer itself**, and writes through either view are seen by the other. One
caveat: MoltenVK 1.4.0 rejects `VkExternalMemoryBufferCreateInfo{HOST_ALLOCATION}` — the 1.4.1 notes
claim a fix, so this argues for pinning ≥ 1.4.1.

### 11.5 Layouts, QFOT and tiling

Under MoltenVK a barrier only records `layoutState`; **queue-family ownership transfer is a complete
no-op** — the indices are stored and never read. Acquiring with `oldLayout = UNDEFINED` and copying
out gave 0 mismatches: layouts are metadata and content is never discarded. Keep the portable
`EXTERNAL → family` barriers for the Linux build; on Apple they cost nothing and do nothing.

**Use `OPTIMAL` tiling and take strides from the IOSurface.** With `LINEAR`,
`vkGetImageSubresourceLayout.rowPitch` is MoltenVK's own computation and disagrees with
`IOSurfaceGetBytesPerRow` whenever the surface pads (width 1000 → 4000 vs 4096). GPU access is right
either way; host code trusting the Vulkan layout would not be.

### 11.6 Lifetime — and a correction

The leak picture is better than §10 alone suggests. Measured: when a child holding a surface is
`SIGKILL`ed, `IOSurfaceIsInUse` drops to false **by the time `waitpid` returns** — the kernel releases
a dead task's refs and use counts atomically with teardown, and a child that exits without
decrementing leaks nothing. The residual hazard is narrower than "orphans leak": a **live** child can
pin a surface indefinitely, exactly as a live Linux child can hold a DMA-BUF fd open.

> ~~`IOSurfaceIsInUse` drops to false **by the time `waitpid` returns** — the kernel releases a dead
> task's refs and use counts atomically with teardown~~ — Superseded in part 2026-09-21 by #2360's
> measurement (`docs/learnings/iosurface-in-use-tracks-ports-and-use-counts.md`). The release is
> prompt but asynchronous: on an idle machine it has happened by `waitpid`, and with other work
> running it lands 100–400 µs after the reap. A dead child still leaks nothing.

macOS is actually *ahead* here: `IOSurfaceIsInUse` is a kernel-truthful liveness signal with no
protocol needed, and DMA-BUF offers nothing equivalent. Recycle a slot when
`consume_done ≥ frame` **and** `IOSurfaceIsInUse == false`.

The genuine §10 finding stands for the other direction: a child that outlives its **parent** keeps the
surface readable, so the child still needs a parent-death teardown — which it needs anyway to exit.

### 11.7 Costs that bear on the milestone

| operation | measured |
|---|---|
| `IOSurfaceCreate` | 15.8 µs |
| first touch of a fresh surface (page-in) | 673–850 µs |
| `CFRelease` (last ref) | 131 µs |
| `vkCreateImage` import, warm | 4–18 µs |
| mach handoff parent → child | 5–6 µs |
| cross-process round: 8 MB clear + 8 MB copy + 2 timeline hops | **500–560 µs** |
| **`VkInstance` + `VkDevice` creation, per process** | **505–540 ms** cold, 26–70 ms with warm Metal caches |

Two consequences worth stating before tickets are sized:

- **Pool the surfaces.** Creation is cheap but first touch and release are not.
- **The half-second per-process device bring-up is a real risk to the helper startup budget**, which
  the plan pins with `test_every_helper_interpreter_goes_live_inside_the_startup_budget`. Warm the
  device before the real-time loop, and expect to revisit that budget for macOS.

One unexplained observation worth carrying as a known risk: in 1 of ~15 runs a client blocked
indefinitely inside Metal's shader-cache `flock` while another process of the **same executable** was
alive. A 6×3 stress did not reproduce it. It matters because every Python helper shares one
`sys.executable` and therefore one Metal shader cache.

---

## 12. The spike — a runnable proof, with a real Python child

Built and run on this machine, 2026-09-19. Lives at `/tmp/slmac-sample/`; one command
(`bash /tmp/slmac-sample/run.sh`) builds it, creates its venv, runs three positive variants at 300
frames each plus a negative control, and prints `OVERALL: PASS`.

It reproduces the engine's real topology rather than a simplification: a parent (engine stand-in)
allocates a pool of four 1920×1080 BGRA IOSurfaces, imports each as a `VkImage` through MoltenVK,
GPU-writes a moving test pattern, `posix_spawn`s **a real Python 3.12 process**, hands the surface over
a **raw Mach** channel, and the child edits the pixels through a **numpy view over the mapped
surface** — the `InvertingEffect` shape — before the parent GPU-reads it back and verifies.

**Result: PASS**, and the verification is real rather than an exit code: every one of 2,073,600 pixels
of every one of 300 frames is compared against the expected inversion, with the pattern moving per
frame so a stale surface would fail. The negative control (child told to skip the edit) correctly
reports `FAIL` on all 2,073,600 pixels. I independently opened both PNGs
(`out/frame_before_child.png`, `out/frame_after_child.png`) and confirmed they are exact colour
inverses with the checkerboard intact.

### What it settles

- **The whole topology works unbundled, unsandboxed, ad-hoc-signed, from a terminal.** No `.app`, no
  plist, no `launchctl`, no entitlements, no signing step, no env vars. The linker's default ad-hoc
  signature is enough, and the venv's Homebrew Python is ad-hoc too.
- **Python was not a stretch — it worked first try.** The child is an ordinary `python3` with its own
  pid and GIL, binding a ~150-line C shim by `ctypes` and editing through numpy. No PyObjC.
- **No TCC prompt appeared**, as expected: IOSurface, Mach bootstrap and Metal are not TCC-gated. The
  camera is a separate question (§8) and the only one that prompts.
- **Both service-less rendezvous work.** `bootstrap_check_in` is the default because it is
  spawn-agnostic; `mach_ports_register` passes too and is kept behind a flag.

### The numbers

Per frame at 1920×1080, median over 300 frames, with the child caching the imported `IOSurfaceRef`
per pool slot:

| phase | median |
|---|---|
| IOSurface handoff (Mach send → child recv) | 0.011 ms |
| child import (cached) | 0.009 ms |
| child `IOSurfaceLock` + `Unlock` | 0.008 ms |
| **child numpy edit, 8.3 MB in place** | **0.177 ms** |
| reply (child → parent) | 0.009 ms |
| **cross-process round trip** | **0.216 ms** |
| GPU write + host wait | 0.691 ms |
| GPU readback + host wait | 0.467 ms |

**698 fps** for GPU-write + round-trip + GPU-read, fully serialised in lock-step. The cross-process hop
is **0.216 ms of a 16.7 ms frame budget** — 30 and 60 fps are met with roughly 5× margin, and that is
*before* any pipelining. Child startup to first contact: 60 ms warm, ~220 ms cold.

### Two findings that go straight into tickets

1. **The first `IOSurfaceLookupFromMachPort` for a given surface in a given child costs ~9–10 ms** —
   kernel client setup and mapping. Once per surface, not per frame, but it means **the engine should
   hand a helper its whole pool at startup**, not lazily on first use, or the first frame of each
   slot eats most of a 60 fps budget.
2. **A helper must cache the `IOSurfaceRef` per pool slot.** Releasing and re-looking-up every frame
   makes the numpy edit **4× slower** (0.66 ms vs 0.177 ms) because the mapping is torn down and
   page-faulted back in. The message shape supports "port once per slot, id thereafter", which is what
   the engine should do.

### What a user must do beyond `pip install`

One thing: **have MoltenVK and the Vulkan loader on disk.** The spike links Homebrew's loader by
absolute path and the loader finds the ICD itself with no env vars set. Bundling
`libMoltenVK.dylib` in the wheel (§9) makes this list **empty**.

### What it does not prove

It uses **host-side ordering** — the parent GPU-signals its own timeline and CPU-waits before the
handoff — because it was built while §11.1's shared-event question was still open. **§11.1a has
since resolved that question**, so what the spike exercises is the *fallback* path rather than the
route the milestone will take. The sync is isolated behind two functions, so the device-side shared
events drop in without touching anything else. The submit-plus-
host-wait hops cost ~0.45–0.7 ms each; that is the CPU overhead device-side events would remove.
It also does not touch AVFoundation, the window, or the wheel.

---

## 13. Draft change and tickets

Not filed. This is the shape `/propose-change` would take, so that step is short.

**Change name:** `macos-platform-floor`. **Milestone:** *Camera → display on Apple Silicon* — a
product capability, per `/derive-tickets` step 4.

`[NEEDS DECISION]` blocks the change must carry: items 1, 2, 3, 5 and 6 from §6 (item 4 is decided —
cross-process frames are in), plus the spawn fork from §10.2: take the discoverable
`bootstrap_check_in` name now, or move the helper spawn from `Command` + `pre_exec` to a direct
`posix_spawn` first and take the capability-secure port stash.

Tracer bullets, each a vertical slice, blockers first:

1. **The engine compiles and links for `aarch64-apple-darwin`.** Apply the edition-2024 `unsafe
   extern` migration across `apple/`; move the Vulkan stack off the Linux-only dependency table; make
   the `core/rhi` facades follow the same cfg predicate as the `vulkan` module; widen `build.rs`
   shader compilation past Linux; delete `src/metal/` after mining its AVFoundation serialisation
   notes; delete the dead `apple/{time,arkit}.rs` and the unused build-deps. Proof: `cargo check`
   green on macOS **and** a CI job that keeps it green.
   *Blocks everything.*
2. **A Vulkan device comes up on MoltenVK.** Fix `vulkan_device.rs:531` (`let` → `let mut`), move
   `VK_EXT_metal_objects` to the device list where it belongs, request `VK_KHR_surface` +
   `VK_EXT_metal_surface` — or call the vendored fork's `get_required_instance_extensions` instead of
   hardcoding; loader discovery for MoltenVK; a non-Linux `INCOMPATIBLE_DRIVER` message. Proof: an
   engine test that creates a device and dispatches one compute kernel on this hardware.
   *Blocked by 1.*
3. **A window presents on macOS.** The pump's Apple arm: winit on the process's first thread, under
   the existing `window_event_pump` seam, replacing the hand-rolled NSApplication loop in
   `apple/runtime_ext.rs`; `CAMetalLayer` → `VkSurfaceKHR`; `DisplayWindow` ungated. Say once that
   vsync-off is not honourable where MAILBOX is absent. Proof: `two_display_windows_live`'s contract,
   re-expressed against the macOS window server.
   *Blocked by 2.*
4. **A video device seam, with V4L2 moved behind it.** Modelled on `AudioDeviceBackend` /
   `AudioCaptureStream` / `probe_*`: a backend trait, a capture-stream trait with a hand-off callback,
   one probe logged once, no dial, a named-`device_id` miss refused at `setup()`. `CameraSource`
   becomes platform-agnostic above it; the V4L2 code moves to `linux/` unchanged. Proof: Linux CI stays
   green through a pure refactor.
   *Blocked by 1. Parallel with 2 and 3.*
5. **AVFoundation capture behind that seam.** `AVCaptureSession` + `AVCaptureVideoDataOutput` →
   `CVPixelBuffer` → `IOSurfaceRef` → `VkImage`; the IOSurface-backed allocation flavour beside the
   DMA-BUF one; colour attachments mapped to `ColorInfo` the way `v4l2_color.rs` maps V4L2's; the
   capture PTS in the `mach_absolute_time` domain; a real TCC prompt, not a status read; enumeration
   through the seam. Proof: frames from the built-in camera reach a window.
   *Blocked by 3 and 4. TCC is answered (§8) — implement the non-blocking request and the
   name-the-responsible-app diagnostic here.*
6. **A surface handle crosses to a helper process.** The Apple arm of the surface-share service:
   a raw Mach channel replacing the Unix socket, `IOSurfaceCreateMachPort` /
   `LookupFromMachPort` replacing `SCM_RIGHTS`, an audit-token peer check
   (`audit_token_to_pid` + `pidversion`), and dead-peer notifications
   (`MACH_NOTIFY_DEAD_NAME` parent-watches-child, `MACH_NOTIFY_NO_SENDERS` the other way)
   wired to teardown. The `surface_store.rs` call shape survives; the transport beneath it is
   re-pointed off the XPC-service client that never had a server. Never
   `kIOSurfaceIsGlobal`. Recycle on `consume_done ≥ frame` **and** `IOSurfaceIsInUse == false`.
   Proof: a Rust integration test round-tripping a surface between two engine processes —
   no Python yet.
   *Blocked by 2. Parallel with 3, 4, 5. Carries the §10.2 spawn `[NEEDS DECISION]`.*
7. **A Python processor edits a frame on macOS.** The GPU half: engine allocates the IOSurface
   (explicit `Width`/`Height`/`BytesPerElement`/`PlaneInfo`, default cache mode, **memory type 0**,
   `OPTIMAL` tiling, strides from the surface); helper imports it into its own `VkDevice`, cached per
   slot; `produce_done`/`consume_done` exported as `MTLSharedEvent`s via
   `VkExportMetalObjectCreateInfoEXT` and re-imported with `initialValue = 0`; the numpy view from
   `IOSurfaceGetBaseAddress` under a lock bracket, or equivalently through a
   `VK_EXT_external_memory_host` import. Drop the write-combined copy-out advice on macOS — the
   mapping is cached there. Proof: the scaffolded `InvertingEffect` inverts a real camera frame and
   the window shows it.
   *Blocked by 5 and 6. The largest ticket; split it if §11.7's device bring-up forces a budget change.
   §12's spike is the working reference — hand the helper its whole pool at startup and cache the
   `IOSurfaceRef` per slot, or pay 9–10 ms on first touch and 4× on every edit. Take the device-side
   shared-event route (§11.1a) with host-side ordering behind the same interface as a runtime
   fallback. **Bound every producer-side GPU wait well under 5 s** — §11.1b: a stalled helper
   otherwise takes the engine's `VkDevice` down with it.*
8. **The wheel installs and runs.** An `aarch64-apple-darwin` maturin lane on a `macos-15` runner;
   `MACOSX_DEPLOYMENT_TARGET=12.0` pinned in `pyproject.toml`; bundled Vulkan loader + MoltenVK with
   `VK_ADD_DRIVER_FILES` set at import; every vendored dylib prepped as
   `lipo -thin arm64` → `strip -x` → **`codesign -f -s -`**; `CameraSource` and `DisplayWindow`
   registered on macOS with their stub entries updated (`stubtest` gates them);
   `test_wheel_portability` given a Mach-O arm — it currently *skips* rather than fails — asserting
   the `/usr/lib/**` + `/System/**` prefix policy, `LC_CODE_SIGNATURE` on every Mach-O, and
   `minos ≤ 12.0`; MoltenVK and Vulkan-Loader (both Apache-2.0) into `THIRD-PARTY-NOTICES.md`.
   Proof: clean machine → `pip install` → `streamlib new` → `streamlib dev` → camera in a window.
   *Blocked by 7.*

Two gate changes ride along rather than standing alone: `xtask lint-logging` needs a macOS cfg pass or
`apple/` stays unlinted forever, and `check-no-inheritable-descriptor` prescribes `pipe2` /
`epoll_create1` / `timerfd_create` / `eventfd`, four of which do not exist on Darwin — it needs a
portable-CLOEXEC allowance before any macOS code can satisfy it.

Filed as backlog, not in this milestone: `MonotonicTimer` (so continuous-execution Python processors
lag one release); the `PR_SET_PDEATHSIG` equivalent; CoreAudio; VideoToolbox codecs; the six surface
adapters; `packages/screen-capture`'s disposition; the Camera Extension that would be the Apple
`VirtualCameraSink`; and the KosmicKrisp evaluation once macOS 26 is the floor.

Three risks to carry on the milestone rather than inside a ticket: the **~0.5 s per-process
`VkDevice` bring-up** against `test_every_helper_interpreter_goes_live_inside_the_startup_budget`; the
one unexplained **Metal shader-cache `flock` hang** between two processes of the same executable
(every helper shares one `sys.executable`); and the fact that **linking and first run are still
unmeasured** — every measurement here stops at compile or at a standalone probe.

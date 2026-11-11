# display-importable-viewer-frames

> **Decided by the owner, 2026-11-11** (#3499's project thread, on a decision card): "Runtime
> blits viewed frames to DMA-BUF" — spike Electron's import on the Linux rig first, then the
> runtime copies a viewed frame into a DMA-BUF Studio imports, one GPU copy only while viewed,
> nodes and CUDA unchanged. The rig spike ran the same day (below).

A change of one concern: how an Image frame reaches Tato Studio's viewer on Linux when the
memory it lies in is not memory Chromium imports. Today a viewer imports the pool slot a frame
names, as a helper does. That holds on macOS (an IOSurface) and nowhere on Linux NVIDIA. After
this change, the engine copies such a frame on the GPU into a display-importable image and the
viewer imports that; a frame already in a display-importable image still crosses with no copy.

**The direction, in four sentences:**
1. A viewed Image frame reaches Studio in memory Chromium imports: on Linux, a DMA-BUF image
   with an explicit tiled DRM modifier from the engine's render-target probe.
2. A frame whose slot is not that — an OPAQUE_FD texture, a LINEAR pixel buffer — is copied
   into one by the engine, on the engine's device, once per viewed frame, and only while a
   viewer reads it.
3. Nothing a node allocates, publishes or links changes, and neither does a CUDA consumer's
   OPAQUE_FD import.
4. The viewer only imports, as `.claude/rules/rhi.md` ("GPU work belongs to the engine") states.

## Known (2026-11-11, rig spike: RTX 3090, driver 595.99.02, Electron 44.11.0 / Chrome 152)

- Every bundled Image node (`tatolab-testpattern`, `tatolab-compositor`, `tatolab-ops`
  `ImageFile`) asks for `rgba8_unorm` without `render_attachment`, so on a device with the
  OPAQUE_FD image pool its slots are OPAQUE_FD, OPTIMAL tiling
  (`acquisition/linux.rs` `derive_texture_cross_process_importability`).
- `sharedTexture.importSharedTexture` with a `nativePixmap` of a tiled render-target DMA-BUF
  (`create_texture_render_target_dma_buf`, modifier `0x0300000000606014`) imports with every
  pixel right, under `--ozone-platform=wayland`.
- A LINEAR DMA-BUF — a pixel-pool `VkBuffer` or a LINEAR-modifier image — imports as solid
  black with no error anywhere: LINEAR is sampler-only on NVIDIA. So today's Linux viewer path
  for pixel buffers (`DRM_FORMAT_MOD_LINEAR`) shows nothing on NVIDIA either.
- Under Wayland with `--use-angle=vulkan`, a LINEAR DMA-BUF (a pixel-pool `VkBuffer` or a
  LINEAR image) and a tiled one all import with every pixel right. LINEAR is black only under
  Chromium's default ANGLE-on-GL backend.
- Under X11 on NVIDIA no case shows in an on-screen window. The default backend has no
  shared-image backing for a native pixmap (`shared_image_factory.cc: Could not find
  SharedImageBackingFactory`); `--use-gl=angle --use-angle=gl-egl` fails creating the window
  surface (`eglCreateWindowSurface ... EGL_BAD_MATCH`); ANGLE-Vulkan and Chromium's Vulkan
  compositing import and read zeros. Only an offscreen window under gl-egl shows a tiled image.
- `importSharedTexture` never threw in any failing case; a failure is visible only as blank
  pixels or in the GPU process log.

## MODIFIED: docs/plan/ARCHITECTURE.md §Networking, "How a viewer reads a port"

The entry's "an Image frame is shared, not copied — Studio imports the surface as a helper
imports it — so a preview is zero-copy" becomes: an Image frame in memory Studio's display
imports is shared, not copied; one that is not is copied once on the engine's GPU into an image
the display imports, only while a viewer reads it, and never reaches the CPU.
A viewer whose display shows no DMA-BUF it imports (Chromium on NVIDIA under X11) reads a
host-visible copy instead, through the CPU (owner, 2026-11-11, below). Rejected: a CPU readback
for every viewer — a copy through host memory per frame, against the zero-copy preview (owner,
2026-11-11). Rejected: allocating node output textures as DMA-BUF so
the viewer needs no copy — strands every CUDA consumer of them, since NVIDIA exports one flavour
per allocation (owner, 2026-11-11).

## ADDED: a display residency of the engine's export staging

`core/context/surface_export_staging.rs` already is the one system for "one GPU blit into an
exportable staging" for a consumer one hop away: per pool slot, refilled by a GPU copy each
time a consumer asks, published once to the surface-share service, with a timeline. It has two
residencies, `DeviceLocal` and `HostVisible`, both `VkBuffer`s. This change adds a third:

- ADDED: SurfaceExportStagingResidency::DisplayImportable
  An image, not a buffer: allocated through the render-target DMA-BUF path
  (`vulkan_device.rs` `create_texture_render_target_dma_buf`, `vulkan_texture.rs`
  `new_render_target_dma_buf`) at the source's extent and a colour format of the source's byte
  order, with an explicit modifier from the probe; registered with surface-share as a
  `texture` over `dma_buf`, which `GpuPoolSlotOfAViewer` already reads (#3524).
- The refill is an image-to-image copy for a texture source and a buffer-to-image copy for a
  pixel-buffer source, through the RHI's recorded copies; it waits for the producer's frame as
  the existing residencies do, and retires on the host before it answers, so Chromium, which
  waits on no Vulkan timeline, never samples a half-written image.
- A source already in a display-importable image (an explicit-modifier DMA-BUF texture) is never
  staged: the viewer imports its slot as today.
- One staging per viewed pool slot, shared by every viewer of it, evicted with the slot as the
  existing residencies are (`GpuContext::evict_surface_export_stagings`). About 8.3 MB per
  1080p RGBA slot.

## ADDED: a viewer's ask for a frame it can read

- ADDED: SURFACE_SHARE_IMPORT_VIEWED_FRAME_OP
  A surface-share op a viewer sends with the frame its message named, after claiming it in the
  pool's claim table as it does now, and how it reads the frame (`ViewedFrameReadBy`:
  `display_import` or `cpu_read`). For a display import the engine answers with the frame's own
  slot when that is display-importable, and otherwise refills the slot's `DisplayImportable`
  staging from the frame; for a CPU read it refills the slot's `HostVisible` staging, which
  states the device that exported it. Either way it answers with the registration and its
  handles. The engine serves it with
  the one `GpuContext` (`EngineResourcesSharedByEveryStream`), so the surface-share service
  gains a handle to it once the context exists, and refuses the op by name before then.
- `GpuPoolFrameImporterOfAViewer` is made on Linux with how it reads frames, and `hold_the_frame`
  sends the op in place of `import_pool_slot` for an Image frame; the held frame keeps the source's claim until it
  drops, so the slot is not reused under a refill.

[NEEDS DECISION] Who drives the copy.
- (a) The viewer asks per frame over the surface-share socket, as above. One request per
  viewed frame (the viewer reads only the latest, so at most its display rate); no thread or
  channel of the engine's own; the copy happens only for frames a viewer actually reads.
- (b) The engine reads the port itself while a viewer is open, copies each latest frame and
  republishes it on a channel of its own the viewer subscribes to instead. No per-frame
  request, but a reader thread, a second channel and a copy for frames no viewer reads.
Recommendation: (a). It extends the surface-share service and the export staging, adds no
parallel channel, and copies nothing a viewer does not read.

DECIDED (owner, 2026-11-11): an NVIDIA X11 session reads by CPU. Studio picks the reading
before its first frame, because there the import reports success and shows black, and runs
with `--use-angle=vulkan` on Linux; Studio warns while it runs on X11 (tatolab-app#279,
tatolab-app#280).

## MODIFIED: the viewer's import client

- MODIFIED: runtime/streamlib-gpu-frame-import-client/src/linux/gpu_pool_slot_of_a_viewer.rs
  A pixel-buffer slot's DMA-BUF planes are offered to a display only where the engine says
  linear memory is display-importable; on NVIDIA they arrive staged.

## Not in this change

- Tato Studio's addon reading the plane layout and modifier (tatolab-app#275), choosing the
  reading and launch switch (tatolab-app#279) and its X11 warning (tatolab-app#280): app work
  against this contract, filed there.
- A CPU read at the viewer's on-screen size rather than the frame's: the copy is full-size.
- macOS: IOSurface crosses as today.
- AMD and Intel: LINEAR may import there; the op answers the source's own slot wherever the
  engine knows the device's display imports it, which this change measures on NVIDIA only.

## Proof

- Engine tests on the GPU lane: a viewed OPAQUE_FD texture frame and a viewed LINEAR pixel
  buffer each answer a `DisplayImportable` staging whose pixels equal the source's; a viewed
  explicit-modifier texture answers its own slot; a second viewer reuses the staging.
- Live on the rig: Test Pattern Source's `video` in Studio's Viewer under Wayland, through
  `/verify-app` once tatolab-app#275 lands.

# browser-and-rive-source-modules

> **Approved by the owner, 2026-11-02**, with B1–B3 as recommended ("Approve all three").
> The owner asked for this plan once the Linux camera went zero-copy ("Plan after camera
> lands"), then added page audio out and media inputs into the page.

Built on `media-built-ins-become-modules`. A stream's visuals today come from a camera, a test
pattern or a user's own GPU kernel. The owner wants two more sources, both as first-party
modules built the way a third party would build them. The first is a web page, as in OBS's
browser source, so a marketer can draw a lower third in ordinary HTML and JavaScript. The
second is a Rive animation. Both are driven by data arriving on a port, so a graph can swap
or fill a template per viewer segment (owner, 2026-11-02: visuals a marketer "can visually wire
a dynamic flow" around, with "a basic html app with JavaScript").

**After this change:**
- **`tatolab.browser:BrowserSource`** renders a URL offscreen with Chromium and publishes each
  painted frame as a video frame. A `data` input delivers each message to the page as a DOM
  event. The page can publish back on an `events` output. The page's audio comes out on an
  `audio` output.
- **A page can composite the stream's own media.** Each video or audio input wired to a
  browser node reaches the page as an ordinary `MediaStreamTrack`, so a web app can mix
  camera, overlays and sound itself and the node publishes what it paints.
- **`tatolab.rive:RiveSource`** plays a `.riv` file's state machine and publishes each frame.
  A `data` input sets the file's view-model properties by name.
- **`tatolab.compositor:AlphaOverCompositor`** layers N video inputs, back to front, into one
  frame. This makes a lower third over a camera a three-node graph.
- **Any GPU renderer a third party embeds can draw straight into a stream frame.** The renderer
  gets a Vulkan device that `streamlib-consumer-rhi` creates and owns. Rive is the first
  renderer to use it, but nothing about the mechanism is specific to Rive.

**Scale gate:** this skill alone, with no ADR. The change adds modules and one public
RHI accessor under decisions already recorded (D1–D3 of `media-built-ins-become-modules`), and
retires nothing.

## Current state (recon, 2026-11-02, `main` `a7e3953f4`)

- **The host table (ABI 1.2) can already publish a frame from foreign memory.**
  `surface_import_dma_buf`, `output_texture_acquire_from_node_output_pool` and
  `surface_convert_into` take a DMA-BUF, convert it into a node output texture and hand back a
  surface id to publish (`sdk/tatolab-module-sdk/include/tatolab_module_host_table.h:452-462`).
  The camera module publishes this way (`packages/tatolab-camera/src/v4l2_camera_capture.rs`).
- **On NVIDIA that path costs one copy.** Cross-device DMA-BUF import is blocklisted there, so
  the engine maps the buffer read-only and copies it through host-visible staging. The camera
  still matches the built-in's frame rate on that path (#3064).
- **There is no host-memory upload verb.** A module holding CPU pixels cannot write them into
  an output texture without inventing a DMA-BUF.
- **The owned-device tier exists.** `streamlib-consumer-rhi` creates a Vulkan device on the
  runtime's GPU, matched by UUID, and imports surface-share surfaces onto it
  (`runtime/streamlib-consumer-rhi/src/owned_vulkan_video_encode_device.rs`; the H.264 module
  encodes this way). It never exposes raw Vulkan handles.
- **Vulkan stays confined.** `cargo xtask check-boundaries` refuses `vulkanalia` in any Rust
  file outside the RHI crates, `packages/` included (`xtask/src/check_boundaries.rs:372`).
- **No compositor node exists.** `VulkanOffscreenCompositor` is private to the virtual camera.
- **CEF** (the Chromium Embedded Framework, BSD-3) is what OBS's browser source uses. Its
  offscreen rendering paints either to a CPU BGRA buffer (`OnPaint`) or to a shared GPU
  texture (`OnAcceleratedPaint`): a DMA-BUF on Linux, an IOSurface on macOS. CEF is
  multi-process: it re-executes a subprocess binary for its renderer and GPU processes. Its
  `CefAudioHandler` hands the host each page audio stream as planar float samples.
- **A page cannot take a GPU texture from outside.** Stock Chromium has no import path for a
  foreign texture into a page, and no supported way to present a fake capture device to
  `getUserMedia` without patching Chromium. A page can build a track from frames and samples it
  is handed (`MediaStreamTrackGenerator`, WebCodecs `VideoFrame` and `AudioData`), which
  costs a GPU-to-CPU readback of each input frame.
- **Rive's C++ renderer** (`rive-runtime`, MIT) has a Vulkan backend.
  `RenderContextVulkanImpl::MakeContext` takes the host's instance, physical device, device
  and `vkGetInstanceProcAddr`, and records into a command buffer the host supplies. It also
  has Metal, D3D and GL backends.

## ADDED

- **Host table 1.3: `surface_write_from_host_memory`.** Writes tightly described CPU pixels
  (format, width, height, row stride) into an output-pool texture. This is the fallback for a
  renderer that cannot share a GPU buffer, and it is public to every module.
- **`streamlib-consumer-rhi` external-renderer device.** On the owned-device tier, a
  `ConsumerVulkanDeviceForExternalRenderer` hands an embedded renderer the raw instance,
  physical device, device, one graphics queue with its family index, and
  `vkGetInstanceProcAddr`. It also imports a node output texture onto that device as a
  render target, with the acquire and release semaphores the engine already uses for
  surface-share. The renderer's own C++ makes its Vulkan calls on that device. Rust outside
  the RHI still never names `vulkanalia`.
- **`packages/tatolab-browser`.** A CEF offscreen browser runs in the node's helper. Its
  subprocesses execute a small `tatolab-browser-subprocess` binary that ships in the wheel,
  never `sys.executable`.
  - Each frame is imported zero-copy where the platform allows it, otherwise written from host
    memory, and published from CEF's paint thread through the host table.
  - Config: `url`, `width`, `height`, `frame_rate`, `transparent_background`.
  - Each `data` message is a msgpack map and reaches the page as
    `window.dispatchEvent(new CustomEvent("tatolab:data", {detail}))`.
  - `window.tatolab.publish(object)` writes one msgpack map to `events`.
  - The page's audio is published on `audio` as the house audio block, resampled to the
    configured `audio_sample_rate`; a silent page publishes nothing.
  - Optional `video_in_N` and `audio_in_N` inputs reach the page as
    `window.tatolab.inputTrack("video_in_0")`, a `MediaStreamTrack` built from frames read
    back to host memory and samples handed over unchanged. An unwired input has no track.
- **`packages/tatolab-rive`.** It renders a `.riv` file on an external-renderer device into the
  output texture: zero copy, NVIDIA included.
  - Config: `riv_path`, `artboard`, `state_machine`, `width`, `height`, `frame_rate`.
  - Each `data` message is a msgpack map from view-model property path to value.
  - A missing path is logged once and skipped, never fatal.
- **`packages/tatolab-compositor`.** `AlphaOverCompositor` takes inputs `layer_0` through
  `layer_N`, back to front, in premultiplied alpha. It emits on its first layer's cadence and
  holds the newest frame of every other layer.
- **A scaffolded example stream, `examples/lower-third`:** camera, then a browser lower third
  and a Rive bug, then the compositor, then the display. A `data` message switches the
  template's text and variant.

## MODIFIED

- **§Packages & extension model**, the first-party module list: add `tatolab.browser`,
  `tatolab.rive` and `tatolab.compositor`.
- **§GPU** (the consumer RHI): the owned-device tier serves video codecs and embedded
  renderers. The external-renderer accessor is the single place raw Vulkan handles leave the
  RHI. It is named for that purpose, never for general use.

## REMOVED

Nothing; this change only adds.

## Decisions

**B1 DECIDED (owner, 2026-11-02): the web engine.**
- (a) **CEF.** It is what OBS uses, it runs on Linux and macOS, and it shares textures on both.
  It costs about 250 MB per platform, and Chromium's patent-encumbered codecs are off in
  standard builds.
- (b) **WPE WebKit.** It is built for embedded use and exports DMA-BUF natively, but in practice
  it is Linux only.
- (c) **The system webview** (WKWebView or WebKitGTK). It is small, but it has no supported
  offscreen texture path and behaves differently on each OS.

Chosen (a): parity with OBS browser sources matters to the marketers it is for.

**B2 DECIDED (owner, 2026-11-02): how Rive gets its GPU.**
- (a) The **external-renderer device** above. It is zero copy everywhere, and the same
  mechanism serves Skia, Godot or any engine a third party embeds next.
- (b) **Rive bootstraps its own Vulkan device** inside its C++ and exports each frame. StreamLib
  adds no RHI surface, but on NVIDIA every frame pays the staging copy.

Chosen (a): D1 says a first-party need becomes public API, and "embed my renderer" is the
next thing a third party asks for.

**B3 DECIDED (owner, 2026-11-02): the compositor.**
- (a) Ship **`tatolab-compositor`** as a first-party module.
- (b) Leave compositing to each stream's own kernel.

Chosen (a): without it, the lower-third demo needs custom GPU code, which defeats the point
for marketers.

**Assumed, not asked:**
- CEF ships inside platform wheels on the repo-hosted PEP 503 index, which has no PyPI-style
  size cap.
- Mouse and keyboard input into the page is later work.
- Overlays composite on the GPU through `tatolab-compositor`. Page inputs are for a page that
  mixes media itself, and they pay one readback copy per input frame; the GPU compositor stays
  the default for overlays.
- A fake capture device visible to `getUserMedia` is out: it needs a patched Chromium.
- The `data` and `events` payloads are msgpack maps, the house wire.

## Tracer bullets

1. **Browser source, CPU path.** Host table 1.3 `surface_write_from_host_memory`, and
   `tatolab-browser` with `OnPaint`, the `data`, `events` and `audio` ports, and the
   subprocess binary.
   - Done: a page playing a tone publishes it on `audio` with no gaps over 10 minutes.
   - Done: a 1080p page animates at 60 fps on the dev box.
   - Done: a `data` message changes the page within one frame.
   - Done: `kill -9` of the helper takes CEF's subprocesses with it.
2. **Browser source, zero copy.** `OnAcceleratedPaint`, imported through
   `surface_import_dma_buf` on Linux and `import_iosurface` on macOS (after #3046).
   - Done: copies per frame are counted and recorded against bullet 1, on NVIDIA and on macOS.
3. **Rive source.** The external-renderer device in `streamlib-consumer-rhi`, and
   `tatolab-rive` on it.
   - Done: a reference `.riv` renders at 1080p60 with zero copies per frame.
   - Done: `data` drives a view-model property.
   - Done: the boundary check still passes.
4. **Compositor and the lower-third demo.** `tatolab-compositor` and `examples/lower-third`.
   - Done: the three-source graph runs at 60 fps on the dev box.
   - Done: a `data` message swaps the template live.
   - Done: a recording of it is attached to the PR.
5. **Page inputs.** `video_in_N` and `audio_in_N` on `tatolab-browser`, readback in the
   engine's surface path, and `window.tatolab.inputTrack`.
   - Done: a page draws the camera and plays the microphone through its own canvas mix, and
     the node publishes the result at 1080p30 on the dev box.
   - Done: added latency per input is measured and recorded.

Order: 1, then 2 and 3 in parallel, then 4, then 5. Bullet 2's macOS half waits on #3046.

# jpeg-after-the-robotics-cut

> **Approved by the owner, 2026-10-05**, as written, with its stated assumptions. Tickets derived
> 2026-10-05 (below).

The build of the JPEG align (owner, 2026-10-04): the removals and MJPEG capture, as one change.
After this change:
- the tree holds no JPEG block, no nvJPEG backend, no `libnvjpeg` probe, no
  `ThirdPartyGpuCapabilities`, and none of the held JPEG consumers or their fixture;
- `sdk/vulkan-jpeg` is no longer a crate: its parser, entropy decode and kernel live in the
  engine, read 4:2:0 and 4:2:2, and write the capture stage's scratch texture;
- a Linux camera whose best mode is MJPEG is captured in MJPEG at that mode's size and rate,
  and lands in the pooled `Rgba32` buffer like every other capture;
- nothing a stream author writes or configures changes.

**Scale gate — this skill, plus the existing ADR** (`docs/decisions/jpeg-after-the-robotics-cut.md`).
It is new behaviour in a built-in. It adds a compute kernel inside the engine but no RHI
primitive. It touches no IPC wire and no Python API contract: `CameraSourceConfig` keeps its
three keys and only the stub's transport sentence changes.

**Precondition.** Every entry built is DECIDED:
- §Media I/O `ARCHITECTURE.md:2201-2209` (MJPEG capture on Linux) and `:2717-2742` (codec
  blocks: `JpegDecoder` retired, the nvJPEG tree and probe deleted);
- `:3220-3242` (the held codec consumers);
- §Consumers `:711-727` (the JPEG pair deletes outright).

**Verified against the tree 2026-10-05 (HEAD 62395e617)** in two read-only sweeps. `E` means
`runtime/streamlib-engine/src`, `J` means `sdk/vulkan-jpeg`, and `V` means
`E/linux/v4l2_video_device_backend.rs`.

**The removals**
- `E/vulkan/_nvjpeg_impl_pending_/`: 8 files, 2,713 lines, never compiled. Its
  `tests_pending/gpu_decode.rs:65-333` holds a GPU-against-CPU-reference PSNR test worth carrying
  forward (Y ≥ 50 dB; its reference hardcodes 4:2:0 at `:194-199`).
- `ThirdPartyGpuCapabilities` (`E/vulkan/rhi/vulkan_device.rs:59-141`): `nvjpeg` is its only
  field. Also the device field (`:160-164`), the probe call (`:682-688`, `:1588`), the getter
  (`:3009-3025`), three tests (`:4457-4506`) and the re-exports (`E/host_rhi.rs:64`,
  `E/vulkan/rhi/mod.rs:32`).
- `packages/jpeg` (472 lines) builds against `streamlib-plugin-sdk`, `streamlib-plugin-abi` and
  `streamlib-macros` 0.16.0, plugin crates the tree no longer has. `examples/jpeg-psnr` (253 lines)
  and `E/../tests/fixtures/e2e_fixture_psnr_jpeg.sh` (328 lines) run through it.
- Residue that names them:
  - the `verify-live` skill, whose `SKILL.md:66-71` counts "three fixture rigs";
  - `.claude/scripts/tests/rig-brake.test.sh:221`, `:243`, `:246` and `:285`, which use
    `jpeg-psnr` paths as hook inputs;
  - comments at `E/core/context/texture_ring.rs:123-124` and
    `xtask/src/lint_logging.rs:376`, `:411` (`_apple_impl_pending_` stays as the example
    there);
  - `docs/architecture/third-party-gpu-backends.md`, whose only shipped instance is nvJPEG
    and `J`'s backend trait, and `texture-ring.md:30`, `:206`, `:282`, `:330`;
  - `Cargo.toml:5`, `deny.toml:184-187`, and the comment at `release-please.yml:59`.

**The decoder, today**
- **No raw vulkanalia.** Every import is the facade's re-export of an engine item:
  `ResolvedColorInfo`, `GpuContextFullAccess`, `TextureRing`, `VulkanComputeKernel`,
  `StorageBuffer`, `ComputeKernelDescriptor` and so on. `check-boundaries` flags nothing
  under `E/vulkan/`.
- **Its shader includes a copy of `color_convert_common.glsl`** that is byte-identical to the
  engine's. The engine compiles its shaders from the list in
  `runtime/streamlib-engine/build.rs:148-223`.
- **It writes its own `TextureRing` and submits its own fence** (`J/src/vulkan_compute_backend.rs:100-134`,
  `J/src/kernel.rs:321`). The recorder path, `VulkanComputeKernel::record`, is `pub(crate)` and
  reachable only from inside the engine.
- **The 4:2:0 gate and its buffer sizing.**
  - The gate is at `J/src/kernel.rs:369-407`.
  - Only `jpeg_decode.comp:137-140` hardcodes the chroma geometry; two shifts fit the
    push-constant pads (`kernel.rs:69-70`).
  - `worst_case_coefficient_buffer_bytes_420` (`:325-340`) budgets 12.5 MB at 1080p. A 4:2:2
    frame needs 16.6 MB and fails the size check at `:224-231`.
  - The CPU entropy decode already walks any sampling (`J/src/scan.rs:19-79`).
- **A scan with no Huffman table is refused** (`scan.rs:31-42`). The Annex K tables exist only
  as a test fixture (`huffman.rs:123-129`).
- **Colour resolves from the bitstream.** JFIF defaults to full-range BT.601; Adobe APP14 is
  honoured (`J/src/color.rs:463-537`). The result is the engine's `ResolvedColorInfo` and
  `ColorConverterPushConstants`.
- **Cost, read from the code and not measured.**
  - Huffman decode reads one bit at a time with no lookahead (`huffman.rs:99-116`).
  - Each frame allocates and repacks about 25 MB (`scan.rs:52`, `kernel.rs:467-477`).
  - The shader evaluates a full 64-term IDCT with two `cos()` per term
    (`jpeg_decode.comp:94-165`). Its own comment targets 640×360 at 30 Hz.
- **No CI step runs its tests.** Linux clippy builds the lib only (`test.yml:121-122`), and
  macOS checks `--all-targets` without running them (`:801`). The ADR's line saying they run
  is corrected in this PR.

**The V4L2 arm, today**
- **Negotiation** (`V:369-452`) tries NV12, then YUYV, at the most pixels within the cap.
  - It never calls `enum_formats` or `enum_frameintervals`, and never sets an interval.
  - The rate is read back with G_PARM after S_FMT (`V:219-224`), so it is the driver's default
    for the chosen size.
- **Per frame**, each frame's bytes reach a storage buffer, imported over DMA-BUF (`V:832-881`) or
  copied from the mmap (`V:882-923`, `buf.len().min(sizeimage)`).
  - `bytesused` is read nowhere.
  - The stage `CapturedVideoFrameToPooledRgbaConversionStage`
    (`E/core/context/captured_video_frame_to_pooled_rgba_conversion_stage.rs`) runs the colour
    converter into a local `Rgba8Unorm` scratch texture (`:84-87`). It then copies that into an
    acquired pooled `Rgba32` buffer and waits on the host (`:136-242`).
- **Colour** comes from G_FMT (`V:543-611`). The frame carries `cached_color` (`V:962`).
  - The Cam Link 4K in MJPG reports sRGB with a Rec.709 encoding, which `v4l2_color.rs` maps to
    limited-range BT.709.
  - The JPEG it sends is JFIF, which means full-range BT.601.
- **Precedent.** The Apple arm already ranks modes by most pixels within the cap, then highest
  rate (`E/apple/avfoundation_video_device_backend.rs:183-230`), and applies the rate
  (`:681-683`). Its selector is a pure function with unit tests (`:1138-1170`).
- **Tests and devices.**
  - No test reaches `negotiate_capture_format`, which takes a live `v4l::Device`.
  - CI runs V4L2 tests by name only (`test.yml:325-348`, mirrored in `xtask/src/main.rs:437-441`).
  - vivid offers no MJPG.
  - The Cam Link (USB 3) offers MJPG at every size from 640×480 to 3840×2160, at 60/50/30/25.
    One captured 1920×1080 frame is baseline 4:2:2 with its tables present.
  - No USB 2.0 webcam is attached to the rig.

## MODIFIED: §Media I/O — the codec blocks, and the held consumers

At ship, `:2717-2742` and `:3220-3242` drop "removal unbuilt". They state, as shipped, that
the roster is seven blocks and that the nvJPEG tree, its probe, `packages/jpeg`,
`examples/jpeg-psnr` and `e2e_fixture_psnr_jpeg.sh` are gone. Nothing is mined.
`ThirdPartyGpuCapabilities` goes with its only field.

## MODIFIED: §Media I/O — Camera → GPU transport (`:2183-2200`)

The "one stage" sentence gains its third input:

> Both arms land frames through one stage: the device's NV12 or YUYV bytes, read as a storage
> buffer by the RHI colour converter, or an MJPEG frame, entropy-decoded on the CPU and
> written by the engine's JPEG kernel — into one local scratch texture, copied into a pooled
> `Rgba32` pixel buffer and waited on host-side before the hand-off.

An MJPEG frame uses neither DMA-BUF import nor the NV12/YUYV input buffers, because the CPU
must read its bytes. The automatic choice between zero-copy and upload is unchanged for the
other two formats.

## MODIFIED: §Media I/O — MJPEG capture on Linux (`:2201-2209`)

At ship, the entry drops "(unbuilt)" and the workspace-member clause. It then reads:

> `CameraSource`'s V4L2 arm reads a camera's MJPEG modes beside NV12 and YUYV, picks a mode by
> size, then rate, then format, and sets the rate it picked. An MJPEG frame is decoded by the
> engine's JPEG decoder (4:2:0 and 4:2:2; 4:4:4 and greyscale are refused by name), with its
> colour taken from the bitstream, and lands in the pooled `Rgba32` buffer every capture lands
> in.

## MODIFIED: §Consumers (`:660`, `:711-727`)

- "fourteen converted beside two held" becomes "beside one held" (`examples/screen-recorder`).
- The held list loses its codec-blocks clause and keeps audio plugins and screen capture.

## Factual records in the same PRs

- `docs/architecture/third-party-gpu-backends.md` is deleted: it describes a pattern with no
  shipped instance left.
- `texture-ring.md` loses its "future JPEG decoder" lines and its `J` citation.
- The ADR's consequences line is annotated (this PR).

## Assumptions stated, not asked

1. **One change, not two.**
   - The plan tags both halves `[jpeg-after-the-robotics-cut]`, and this keeps one proposal
     for one align.
   - The removal slice depends on nothing and can land first.
   - *Bites if* you want the removals shipped and archived before the capture work is
     approved. Say so and I split them.
2. **The mode rule** (the plan leaves it ticket-level).
   - Among modes within the cap, take the most pixels.
   - Then rank by rate, but count every rate of 30 fps or more as equal.
   - Then prefer NV12, then YUYV, then MJPEG.
   - Then take the highest rate.
   - Apply the chosen interval with S_PARM, extracted as a pure selector with unit tests,
     after the Apple arm.

   What this gives:

   | Camera and cap | Picked |
   |---|---|
   | Cam Link at the default cap | NV12 1080p at 60, where today it gets NV12 at the driver's default |
   | Cam Link at a 4K cap | NV12 4K30, not MJPG 4K60, which the decoder cannot hold |
   | USB 2.0 webcam offering YUYV 1080p5 and MJPEG 1080p30 | MJPEG |

   *Bites* on a camera offering MJPEG 1080p60 beside YUYV 1080p30: it gets YUYV at 30.
3. **The bitstream wins on colour.**
   - For MJPEG, both the decode and the frame's colour use the decoder's resolution
     (JFIF/Adobe), not G_FMT.
   - The Cam Link's G_FMT report (limited BT.709) contradicts the JFIF frame it sends, and the
     bytes are what gets decoded.
   - *Bites* only on a camera that writes colour it does not signal in the bitstream.
4. **Where the code goes, and what the move drops.** These are pattern choices.
   - Parser, entropy decode and kernel go under `E/vulkan/jpeg/`, Linux-only like
     `E/vulkan/video/`, which holds the codec kernels.
   - The shader joins `build.rs`, and the duplicate GLSL is deleted.
   - The kernel records into the stage's recorder and writes its scratch texture.
   - `SimpleJpegDecoder`, the one-implementor `JpegDecodeBackend` trait,
     `VulkanComputeBackend` and the `TextureRing` output are dropped.
   - The stage gains an MJPEG input beside `CapturedVideoFrameBytesInAStorageBuffer`.
   - No new name contains `JpegDecoder` (a REMOVED pattern).
5. **The Annex K default tables are added** (about 50 lines; `integration.rs:316` flips from
   refusal to decode).
   - UVC payloads may omit DHT, and whether real webcams do is unmeasured.
   - Carrying them costs less than a camera that shows nothing.
6. **The rate bar is measured, not assumed.**
   - The acceptance "live at the advertised size and rate" needs decode within the frame
     interval at 1080p30 on the rig, on the capture thread that already waits on the host.
   - A table-driven Huffman decode, no per-frame allocation, and a separable, precomputed IDCT
     are in scope as the measurement demands.
   - 4:4:4 and greyscale stay refused: no camera case needs them, and 4:4:4 grows the buffer
     to 24.9 MB.
7. **Proof, as the floor allows.**
   - The CPU tests move into the engine and are named in `test.yml` and the xtask mirror:
     parser, entropy, colour, Annex K and the selector.
   - A `hardware-tests` PSNR test carries `gpu_decode.rs`'s CPU reference, generalised to
     4:2:2, against JPEGs the test encodes itself. It is rig-only, per the GPU-tests rule.
   - Acceptance is the plan's: a USB 2.0 webcam with its mode list recorded and its MJPEG mode
     live at the advertised size and rate, through the engine's
     `codec_roundtrip_rig --source camera --camera <dev>` and `/verify-live`.
8. **Public text only.**
   - The stub's transport sentence (`_engine.pyi:107-110`) and the processor description
     (`camera_source.rs:52`) say MJPEG is decoded on capture.
   - `stubtest` is unaffected.

## Owner action before ship

**Attach a USB 2.0 webcam to the rig.** The acceptance names one and none is attached. The
Cam Link is USB 3 and picks NV12 under assumption 2, so it cannot stand in. The removal and
decoder slices do not wait on this; the capture slice's acceptance does.

## Expected slices

1. **Removals.**
   - Delete the nvJPEG tree, the probe and `ThirdPartyGpuCapabilities`, `packages/jpeg`,
     `examples/jpeg-psnr`, and the fixture.
   - Sweep the residue above, and delete `third-party-gpu-backends.md`.
   - Depends on nothing.
2. **The decoder in the engine.**
   - The move, 4:2:2, Annex K, recording into the stage, and the rate work.
   - The CPU tests named in CI, plus the rig PSNR test.
   - `sdk/vulkan-jpeg` is deleted.
   - If slice 1 lands first, it takes `gpu_decode.rs`'s reference from git history.
3. **MJPEG capture.**
   - The selector and S_PARM, the MJPG branch, `bytesused`, colour from the bitstream, and the
     public text.
   - Accepted on the USB 2.0 webcam.
   - Blocked by slice 2.

## Left to later

- MJPEG on the Apple arm (the plan leaves it unchanged).
- 4:4:4 and greyscale JPEG.
- Progressive JPEG.
- Parallel entropy decode across restart intervals.

## Tickets

Derived 2026-10-05; milestone #63, *Webcams live over MJPEG*.

1. #2639 — the JPEG block's leftovers are gone (slice 1). Independent; needs no rig. It carries
   the nvJPEG, consumer and fixture bullets. `nvjpeg` and `nvJPEG` also need #2640.
2. #2640 — the engine decodes a 4:2:0 or 4:2:2 JPEG through the capture stage (slice 2).
   Independent; ultracode; needs the GPU rig. It carries the crate's bullets.
3. #2641 — a Linux camera whose best mode is MJPEG is captured live (slice 3). Blocked by #2640;
   ultracode; needs the GPU rig and a USB 2.0 webcam.

## REMOVED

- REMOVED: sdk/vulkan-jpeg
- REMOVED: vulkan-jpeg
- REMOVED: vulkan_jpeg
- REMOVED: SimpleJpegDecoder
- REMOVED: JpegDecodeBackend
- REMOVED: VulkanComputeBackend
- REMOVED: JpegDecoder
- REMOVED: runtime/streamlib-engine/src/vulkan/_nvjpeg_impl_pending_
- REMOVED: _nvjpeg_impl_pending_
- REMOVED: nvjpeg
- REMOVED: nvJPEG
- REMOVED: ThirdPartyGpuCapabilities
- REMOVED: third_party_gpu_capabilities
- REMOVED: probe_nvjpeg_loadable
- REMOVED: docs/architecture/third-party-gpu-backends.md
- REMOVED: third-party-gpu-backends
- REMOVED: packages/jpeg
- REMOVED: examples/jpeg-psnr
- REMOVED: jpeg-psnr
- REMOVED: runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_jpeg.sh
- REMOVED: e2e_fixture_psnr_jpeg
- REMOVED: JpegBytesSource
- REMOVED: worst_case_coefficient_buffer_bytes_420
- REMOVED: CHROMA_SAMPLING_420

# Testing guide

When to use which testing technique in streamlib. Unit tests are always the first
line of defense — this doc focuses on the cases where unit tests are not enough
and you need to exercise the real pipeline.

> For the **canonical `cargo test` command, the exclusion list, and per-crate
> expected test counts**, see [`docs/testing-baseline.md`](testing-baseline.md).
> Run the workspace baseline before reaching for an E2E scenario — most bugs
> surface there first.

> **Running an example is a two-step, per-directory flow.** The `examples/*`
> are **standalone crates** (each its own `[workspace]` root, not a member of
> the engine workspace), so `cargo run -p <example>` from the repo root does
> **not** resolve them. Run one from inside its own directory, after a one-time
> `./setup.sh` that links the local SDK + the processor packages it needs into
> `streamlib_modules/`:
>
> ```bash
> cd examples/<name>
> ./setup.sh          # streamlib link --engine <checkout> + link the packages
> cargo run -- <args> # e.g. vulkan-video-roundtrip: `cargo run -- h264 /dev/video2 15`
> ```
>
> The `cargo run -q -p <example>` / `cargo build -p <example>` invocations in
> the scenarios below (and in the `e2e_*.sh` fixture scripts) are written
> against the older workspace-member shape and need the `cd examples/<name> &&
> ./setup.sh && cargo run` form for a standalone example. Substitute
> accordingly until the fixture scripts are updated.

---

## Decision tree

1. **Can the change be covered by a unit test?** (pure logic, parser, state
   machine, DPB bookkeeping, etc.) → Write a unit test. Done.
2. **Does it touch GPU memory, Vulkan drivers, V4L2, or the swapchain?** →
   Unit tests will miss driver-only failure modes (see
   [`nvidia-dma-buf-after-swapchain`](learnings/nvidia-dma-buf-after-swapchain.md)).
   You need a real end-to-end run. Pick the scenario below that matches your
   change.
3. **Does the change affect an encoder or decoder?** →
   [Encoder/decoder scenario](#encoderdecoder-scenario).
4. **Does the change affect only camera, display, GPU-compute, or GPU texture
   flow (no codec)?** →
   [Camera/display-only scenario](#cameradisplay-only-scenario).

If you're unsure, default to the more demanding scenario (encode/decode) — it
also exercises camera and display.

---

## Encoder/decoder scenario

**Use `examples/vulkan-video-roundtrip`** (`camera → encoder → decoder → display`)
and sample the decoded frames as PNGs through the display processor's
debug hooks, then **read the PNGs with the Read tool to visually confirm
content**.

### When to use

- Changes to `runtime/streamlib-engine/src/vulkan/video/` (RHI coupling,
  session/DPB, NV12 conversion, rate control, etc.).
- Changes to `packages/h264/src/linux/{encoder,decoder}.rs` or
  `packages/h265/src/linux/{encoder,decoder}.rs`.
- Changes to any GPU code that the encoder or decoder reaches through
  `GpuContext`, `VulkanDevice`, or the RHI.
- Changes to the H.264/H.265 validator, MP4 writer, or anything consuming
  `Encodedvideoframe`.

### Run it

```bash
OUT=/tmp/e2e-$(date +%s)
mkdir -p "$OUT/png_samples"

STREAMLIB_DISPLAY_PNG_SAMPLE_DIR="$OUT/png_samples" \
STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY=30 \
STREAMLIB_DISPLAY_FRAME_LIMIT=300 \
timeout --kill-after=3 25 \
    cargo run -q -p vulkan-video-roundtrip -- h264 /dev/video2 15 \
    2>&1 | tee "$OUT/pipeline.log"
```

- First positional arg is `h264` or `h265` — run **both** for encode/decode
  changes.
- Second arg is the camera device. Use `/dev/video2` (vivid, always available
  in CI-like setups) as the baseline. If the change is driver-path-sensitive
  (MMAP fallback, DMA-BUF, UVC), also run against a real UVC device such as
  `/dev/video0` (Cam Link 4K on this workstation) — several past bugs
  (#288/#289/#292) only reproduced on real hardware.
- For **motion-sensitive** changes (frame ordering, frame drops, timestamp
  drift, skipped DPB frames) use the [v4l2loopback motion
  scenario](#v4l2loopback-motion-scenario) — vivid's built-in animation is
  slow enough that many motion bugs hide behind it.
- `STREAMLIB_DISPLAY_FRAME_LIMIT` makes the run self-terminate so no stranded
  winit window survives the test.
- `STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY=30` writes one PNG per 30 displayed
  frames (adjust for longer runs).

### Verify

1. Grep the log for `OUT_OF_DEVICE_MEMORY`, `DEVICE_LOST`, `process() failed`,
   and `Validation Error`. Zero occurrences for the first three is the pass
   bar. Validation errors are acceptable only if they exist on `main` for the
   same run — otherwise fix or file a follow-up.
2. Confirm progress markers fired: `[H{264,265}Encoder] First frame encoded`,
   `[H{264,265}Decoder] First frame decoded`, plus at least one `Encode
   progress` / `Decode progress` line.
3. **Read at least one PNG sample with the Read tool** and visually confirm
   the content matches the source:
   - vivid → green/purple SMPTE-style test pattern with `00:00:…` timecode
     overlay.
   - Cam Link / real UVC camera → the physical scene the camera sees.
   - A uniform black or magenta frame is a regression even if the run didn't
     error out.
4. **PSNR validation (when a reference frame is available).** For synthetic
   sources (vivid, BGRA file source, fixture video), compute PSNR between the
   source frame and the corresponding decoded PNG to catch lossy-but-silent
   regressions (wrong color matrix, wrong range, off-by-one plane strides,
   chroma on wrong subsample, etc.). For real cameras, PSNR against the raw
   source isn't practical — treat the Read-tool visual check as the sole
   gate.

```
# inside this codebase, with the agent:
Read("/tmp/e2e-.../png_samples/display_001_frame_000060.png")
```

#### PSNR — how to compute

**Primary path: the fixture PSNR rig** (`e2e_fixture_psnr.sh`,
[`runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh`](../runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh)).
Feeds checked-in reference PNGs (solid colors, gradients, a complex
ffmpeg testsrc2 pattern) through `BgraFileSource → encoder → decoder →
display` at a step-locked FPS, pairs each decoded PNG with its reference
by input-frame index threaded through the pipeline, then computes Y/U/V
PSNR via ffmpeg and classifies against the pass bar below:

```bash
# encoder/decoder roundtrip vs. checked-in fixtures
runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh /tmp/psnr-h264 h264
runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh /tmp/psnr-h265 h265

# Sanity-check that the rig flags real regressions. Each PSNR_INJECT_BUG
# variant post-processes the decoded samples through a different
# regression class and is expected to drop Y PSNR below FAIL on the
# references that carry the affected channels:
#
#   swap-channels — R↔B plane swap (catches wiring regressions where the
#                   R and B planes are exchanged anywhere in the
#                   pipeline; fails on red/blue/complex references).
#   bt601-bt709   — encode RGB→YUV as bt601, decode YUV→RGB as bt709
#                   (real matrix mis-interpretation; fails on chroma-
#                   bearing references).
#   range-swap    — encode RGB→YUV at PC range, decode pretending it
#                   was TV range (range expansion mis-interpretation;
#                   fails on the gradient references where mid-luma
#                   variation is heaviest).
PSNR_INJECT_BUG=swap-channels \
    runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh /tmp/psnr-bug-swap   h264
PSNR_INJECT_BUG=bt601-bt709 \
    runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh /tmp/psnr-bug-matrix h264
PSNR_INJECT_BUG=range-swap \
    runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh /tmp/psnr-bug-range  h264
```

Exit codes: `0` — all references at or above the WARN threshold, `1` —
any reference FAILed or NO-SAMPLE, `77` — prerequisites missing. The
harness prints a TSV report to `<output>/psnr_report.tsv`. Pass bar:

```
#   Y PSNR ≥ 35 dB  — good quality
#   Y PSNR 30–35 dB — acceptable, flag it
#   Y PSNR < 30 dB  — regression, investigate color-matrix / range / plane layout
```

Useful env overrides (see the script header for the full list): `FPS`,
`FIXTURE_REPS`, `PNG_SAMPLE_EVERY`, `PSNR_INJECT_BUG`. (Note: an
earlier rig revision exposed a single `PSNR_INJECT_BUG=color-matrix`
mode that was actually an R↔B swap, not a matrix swap; it has been
renamed to `swap-channels`, and `bt601-bt709` is the real matrix-swap
mode. Unknown `PSNR_INJECT_BUG` values now error out instead of
silently no-op'ing.)

**Vivid color regression gate**
(`e2e_fixture_psnr_vivid.sh`,
[`runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh`](../runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh)).
Sister fixture that guards the V4L2 colorimetry path against the
matrix mis-interpretation class (the historical green/magenta tint
symptom). Forces vivid into a saturated single-color test pattern
(`100% Red` by default), runs `vulkan-video-roundtrip` against it,
computes the rig-wide mean of each RGB channel across the sampled
decoded frames, and compares to the checked-in baseline TSV at
[`runtime/streamlib-engine/tests/fixtures/psnr_vivid_baseline.tsv`](../runtime/streamlib-engine/tests/fixtures/psnr_vivid_baseline.tsv)
with a fixed absolute tolerance (±0.05 on the `[0,1]` channel scale).
The saturated pattern magnifies matrix mis-interpretations — a
bt.601 vs bt.709 mis-conversion on `100% Red` lifts the G channel by
~0.07, well above tolerance, whereas the same bug on the default
color-balanced colorbar would only shift G by ~0.005 and slip
through.

```bash
# Standard regression check against the checked-in baseline.
runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh /tmp/vivid-psnr h264

# Re-capture the baseline (do this when the color-management code
# legitimately moves and the post-fix vivid output is the new normal).
BASELINE_CAPTURE=1 \
    runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh /tmp/vivid-baseline h264

# Negative test — proves the gate isn't vacuous.
INJECT_BUG=bt601-bt709 \
    runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh /tmp/vivid-bug    h264
```

The range-swap class is intentionally NOT covered by this fixture —
saturated single-color patterns are insensitive to range
mis-interpretation. The main fixture rig's gradient references are
where range-swap deterministically drops Y PSNR below FAIL.

**JPEG decode rig** (`e2e_fixture_psnr_jpeg.sh`,
[`runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_jpeg.sh`](../runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_jpeg.sh)).
Sibling of the main rig for GPU JPEG decode (`@tatolab/jpeg::JpegDecoder`
wrapping `vulkan-jpeg::SimpleJpegDecoder`). Same shape, decode-only:
ffmpeg encodes each reference PNG to JPEG (the "encoder" half), then
`JpegBytesSource → JpegDecoder → Display` decodes back to a PNG sample
and ffmpeg computes Y/U/V PSNR vs reference. Same `swap-channels |
bt601-bt709 | range-swap` bug-injection modes, same Y ≥ 35 dB pass bar.

```bash
# Clean pipeline — every reference should hit Y PSNR ≥ 35 dB.
runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_jpeg.sh /tmp/psnr-jpeg

# Negative tests — each is expected to deterministically drop Y PSNR
# below FAIL on the references that carry the affected channels:
PSNR_INJECT_BUG=swap-channels \
    runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_jpeg.sh /tmp/psnr-jpeg-bug-swap
PSNR_INJECT_BUG=bt601-bt709 \
    runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_jpeg.sh /tmp/psnr-jpeg-bug-matrix
PSNR_INJECT_BUG=range-swap \
    runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_jpeg.sh /tmp/psnr-jpeg-bug-range
```

The `JPEG_QUALITY` env var sets encoder quality (1–100, default 70).
The default is a known-good baseline that lands every reference well
above the 35 dB Y PSNR pass bar; the rig has been run end-to-end at
quality 95 with all references clearing the pass bar.

> ~~The default `JPEG_QUALITY=70` is load-bearing on the rig's payload-fit
> arithmetic: `rmp_serde` serializes `EncodedJpegFrame.data: Vec<u8>` as
> a msgpack array (per-byte tag overhead, ~1.5× wire expansion), and
> iceoryx2's per-slot default of 64 KiB applies when `@tatolab/jpeg`'s
> declared 16 MiB bound isn't registered with the runtime. q=70 keeps
> even the worst-case `complex_pattern` fixture under that wire budget;
> quality settings above ~70 will trip `ExceedsMaxLoanSize` on
> `complex_pattern` at 1920×1080.~~ — Superseded 2026-05-18 by PR
> `perf/jtd-codegen-serde-bytes-859`. JTD codegen now emits
> `#[serde(with = "serde_bytes")]` on `EncodedJpegFrame.data`, so the
> wire shape is msgpack `bin` (1× footprint) rather than an array of
> integers (~1.5× footprint). The 64 KiB per-slot fallback budget now
> easily covers `complex_pattern` at q=95; the historical q ≤ 70 cap is
> no longer in effect.

**Manual gate for color-management PRs.** Until the GPU CI runner
milestone lands and CI can run the rigs automatically, treat the
fixture rigs above as a mandatory manual gate on any PR that touches
the color path (RHI color converter, encoder/decoder VUI / colorimetry,
display swapchain color space, tone mapper, any sampler-conversion
key). The gate is:

1. Main fixture rig clean — `e2e_fixture_psnr.sh /tmp/psnr-baseline h264`
   passes; same for `h265`.
2. Vivid regression gate clean — `e2e_fixture_psnr_vivid.sh` passes
   against the checked-in baseline TSV.
3. JPEG fixture rig clean — `e2e_fixture_psnr_jpeg.sh /tmp/psnr-jpeg`
   passes (when the change touches the JPEG path).
4. At least one negative test from the matrix above runs as part of
   PR validation and is shown to deterministically FAIL — proves the
   gate is non-vacuous for this branch.

Paste the verdicts plus the negative-test output into the PR
description so reviewers can see the gate fired.

**Fallback path: ad-hoc comparison.** When you need a one-off PSNR for a
non-fixture scenario, run ffmpeg directly against a same-resolution
reference PNG:

```bash
ffmpeg -hide_banner -i "$OUT/png_samples/display_001_frame_000060_input_000060.png" \
       -i reference_frame_000060.png \
       -lavfi "[0:v]format=rgba,setparams=range=pc,format=yuv420p[a];[1:v]format=rgba,setparams=range=pc,format=yuv420p[b];[a][b]psnr" \
       -f null - 2>&1 \
    | grep -E "PSNR|average:"
```

For real-camera runs (`vulkan-video-roundtrip /dev/video0`) there is no
ground-truth reference, so PSNR isn't practical — treat the Read-tool
visual check as the sole gate and write `n/a — real-camera source` under
PSNR in the [test-report template](#standardized-test-output-template).

### Reference

- Reproduced and fixed this way: #288, #289, #292.
- Driver-specific notes: [`docs/learnings/nvidia-dma-buf-after-swapchain.md`](learnings/nvidia-dma-buf-after-swapchain.md),
  [`docs/learnings/camera-display-e2e-validation.md`](learnings/camera-display-e2e-validation.md).

---

## v4l2loopback motion scenario

vivid's test pattern animates slowly — motion-related bugs (duplicated
frames, skipped frames, timestamp drift, reordered output) can hide
behind a source that barely changes frame-to-frame. `ffmpeg`'s
`testsrc2` pattern has both a scrolling timecode and a per-frame
counter embedded in the image, so a dropped or repeated frame is
visible by eye in a PNG sample.

### Host setup (one-time)

```bash
sudo apt-get install v4l2loopback-dkms ffmpeg
sudo modprobe v4l2loopback video_nr=10 card_label=Virtual_Camera exclusive_caps=0
# exclusive_caps=0 (NOT 1) — caps=1 breaks ffmpeg→v4l2loopback writes.
```

Verify: `v4l2-ctl -d /dev/video10 --get-fmt-video` should report
`1920x1080 NV12` (or whatever ffmpeg last pushed). The device survives
until reboot.

### Run it

Start the writer in one terminal (leave it running for the whole test
session):

```bash
ffmpeg -re -f lavfi -i 'testsrc2=size=1920x1080:rate=30,format=nv12' \
       -f v4l2 /dev/video10
```

Then run the example against `/dev/video10` exactly like any other
camera device:

```bash
# camera-only
OUT=/tmp/camdisp-v4l2loop-$(date +%s)
mkdir -p "$OUT/png_samples"
STREAMLIB_CAMERA_DEVICE=/dev/video10 \
STREAMLIB_DISPLAY_PNG_SAMPLE_DIR="$OUT/png_samples" \
STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY=30 \
STREAMLIB_DISPLAY_FRAME_LIMIT=150 \
timeout --kill-after=3 15 cargo run -q -p camera-display \
    2>&1 | tee "$OUT/pipeline.log"

# encoder/decoder roundtrip
cargo run -q -p vulkan-video-roundtrip -- h264 /dev/video10 15
cargo run -q -p vulkan-video-roundtrip -- h265 /dev/video10 15
```

### Verify

1. Same log gates as the other scenarios — zero `OUT_OF_DEVICE_MEMORY`,
   `DEVICE_LOST`, `process() failed`.
2. Read at least one PNG with the Read tool. testsrc2 produces vertical
   color bars (red, green, yellow, blue, magenta, cyan), a diagonal
   rainbow line, a moving numeric timecode in the upper-left corner
   (`HH:MM:SS.mmm`), and a per-frame counter just below the timecode.
   All of those should be visible and crisp; a blank or solid-color
   frame is a regression.
3. Between two consecutive PNG samples (N and N+30), the timecode and
   frame counter should advance by roughly 30 frames' worth of time at
   30fps (~1s). A gap substantially larger than that flags a
   frame-drop bug in the pipeline.

### When to use

- Any change that touches frame ordering, timestamping, FPS
  propagation, dropped-frame handling, or decoder reordering.
- Any camera MMAP / DMA-BUF path change that should be re-verified
  against a strict-conformance V4L2 driver (v4l2loopback does **not**
  tolerate `poll()` before `VIDIOC_STREAMON`, which exposed #303).
- Any encoder rate-control / VBV change where the source frame
  complexity needs to be steady and reproducible across runs.

### Reference

- Added in #303, which fixed the camera MMAP path so it actually
  streams frames from v4l2loopback.

---

## Camera/display-only scenario

**Use `examples/camera-display`** (`camera → display`, no codec) with the
same PNG sampling hooks, and **read the PNGs with the Read tool** to verify.
Prefer this over the roundtrip example when no codec is involved — it
isolates the camera + GPU-compute + display path from encode/decode noise
and runs faster.

### When to use

- Changes to `packages/camera/src/linux/camera.rs` (V4L2, MMAP, DMA-BUF
  import, NV12/YUYV compute shaders, ring textures).
- Changes to `runtime/streamlib-engine/src/linux/processors/display.rs` (swapchain,
  acquire/present, descriptor layout, PNG sampler itself).
- Changes to `GpuContext`, `PixelBufferPool`, `TextureCache`, or `VulkanTexture`
  that do **not** involve a codec.
- Changes to the RHI surface/DMA-BUF paths when there's no encode/decode
  coupling.

### Run it

Prefer the packaged fixture script — it loads vivid, sets the env vars,
and handles cleanup:

```bash
runtime/streamlib-engine/tests/fixtures/e2e_camera_display.sh /tmp/streamlib-e2e
```

Or run the example directly (use this when you need to point at a specific
device like Cam Link):

```bash
OUT=/tmp/camdisp-$(date +%s)
mkdir -p "$OUT/png_samples"

STREAMLIB_CAMERA_DEVICE=/dev/video0 \
STREAMLIB_DISPLAY_PNG_SAMPLE_DIR="$OUT/png_samples" \
STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY=30 \
STREAMLIB_DISPLAY_FRAME_LIMIT=200 \
timeout --kill-after=3 20 cargo run -q -p camera-display \
    2>&1 | tee "$OUT/pipeline.log"
```

### Verify

1. Log: zero `OUT_OF_DEVICE_MEMORY`, `DEVICE_LOST`, `process() failed`.
2. `First frame captured` appears, and PNGs accumulate in
   `$OUT/png_samples/`.
3. **Read at least one PNG with the Read tool** and confirm the expected
   visual content (vivid test pattern or the real camera scene).

### Reference

- Full fixture script:
  [`runtime/streamlib-engine/tests/fixtures/e2e_camera_display.sh`](../runtime/streamlib-engine/tests/fixtures/e2e_camera_display.sh).
- Prerequisites, troubleshooting, and PNG details:
  [`docs/learnings/camera-display-e2e-validation.md`](learnings/camera-display-e2e-validation.md).

---

## Display PNG sampler — env var reference

Read by `linux::processors::display` at processor start.

| Env var | Effect |
| --- | --- |
| `STREAMLIB_DISPLAY_PNG_SAMPLE_DIR` | Directory to write PNG samples into. Unset disables sampling. |
| `STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY` | Sample interval in displayed frames (default 30). |
| `STREAMLIB_DISPLAY_FRAME_LIMIT` | Auto-exit after N displayed frames. Always set this for automated runs — avoids stranded winit windows. |
| `STREAMLIB_CAMERA_DEVICE` | Override `/dev/video0` default for the camera processor. |

PNGs are full-resolution (1920×1080) BGRA→RGBA, handwritten encoder — no
extra dependencies. The sampled data comes from the source HOST_VISIBLE
pixel buffer, so it validates camera→display data flow but not the
actual swapchain rendering step.

---

## Standardized test output template

Whenever you run an E2E scenario from this guide, report results using the
template below. Fill in every field — "N/A" with a reason is acceptable, a
blank field is not. Paste the filled template into the PR description, the
task report, or the comment where you're requesting review. Keep it verbatim
(no rewording) so it's grep-able across PRs.

````markdown
### E2E Test Report

- **Scenario**: encoder/decoder | camera+display-only
- **Example**: `vulkan-video-roundtrip` | `camera-display` | `e2e_camera_display.sh`
- **Codec**: h264 | h265 | n/a
- **Camera device**: `/dev/videoN` (vivid | Cam Link 4K | other)
- **Resolution**: 1920x1080 | 1280x720 | other
- **Duration / frame limit**: `STREAMLIB_DISPLAY_FRAME_LIMIT=… ` (run length in seconds)
- **Build profile**: debug | release
- **Command**:
    ```
    <exact cargo run command with env vars>
    ```

#### Log signals

- `OUT_OF_DEVICE_MEMORY`: <count> (0 = pass)
- `DEVICE_LOST`: <count> (0 = pass)
- `process() failed`: <count> (0 = pass)
- `Validation Error` (with `VK_LOADER_LAYERS_ENABLE=*validation*`): <count or "not enabled">
- `First frame encoded` / `First frame decoded` / `First frame captured`: <timestamps or "not seen">
- `Encode progress` / `Decode progress` high-water mark: <frames>

#### PNG samples

- Directory: `<OUT>/png_samples/`
- Sample count: <N>
- Sample interval: `STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY=<N>`
- PNGs read with Read tool: `<file1>`, `<file2>`, …
- **What was in the image(s)**: <one or two sentences per PNG read — what you
  actually saw. E.g., "frame 60: dark room with chair back and wood door,
  matches the Cam Link scene" or "frame 30: vivid green/purple SMPTE bars
  with `00:00:06:603` timecode overlay, matches expected test pattern". A
  response of "looks fine" is NOT acceptable — describe the content so a
  reviewer can tell at a glance whether you actually looked.>
- Anomalies (black frames, tearing, wrong colors, off-center, etc.): <list or "none">

#### PSNR

- Reference frame: `<path>` or `n/a — <reason>`
- Y / U / V PSNR (dB): <y>, <u>, <v> — or `n/a — <reason>`
- Command used:
    ```
    <ffmpeg or equivalent command>
    ```

#### Outcome

- **Pass** / **Pass with caveats** / **Fail**
- Caveats / follow-ups filed: <list of issue numbers, or "none">
````

Example of a minimal but complete report (from PR #301):

````markdown
### E2E Test Report

- **Scenario**: encoder/decoder
- **Example**: `vulkan-video-roundtrip`
- **Codec**: h264
- **Camera device**: `/dev/video0` (Cam Link 4K)
- **Resolution**: 1920x1080
- **Duration / frame limit**: `STREAMLIB_DISPLAY_FRAME_LIMIT=300`, 15 s
- **Build profile**: debug
- **Command**:
    ```
    STREAMLIB_DISPLAY_PNG_SAMPLE_DIR=/tmp/camlink-e2e-h264/png_samples \
    STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY=30 \
    STREAMLIB_DISPLAY_FRAME_LIMIT=300 \
    timeout --kill-after=3 25 cargo run -q -p vulkan-video-roundtrip -- h264 /dev/video0 15
    ```

#### Log signals

- `OUT_OF_DEVICE_MEMORY`: 0
- `DEVICE_LOST`: 0
- `process() failed`: 0
- `Validation Error`: not enabled
- `First frame encoded` / decoded / captured: 22:04:34.662 / 22:04:34.725 / 22:04:34.6xx
- `Encode progress` high-water: 900 frames; `Decode progress`: 300 frames

#### PNG samples

- Directory: `/tmp/camlink-e2e-h264/png_samples/`
- Sample count: 8
- Sample interval: `STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY=30`
- PNGs read with Read tool: `display_001_frame_000090.png`
- What was in the image(s): "frame 90 — dark room, Secretlab chair back with
  embroidered logo visible in center, wood-panel door top-right against a
  dark-blue wall. Matches Cam Link's live scene."
- Anomalies: none

#### PSNR

- Reference frame: n/a — real-camera source, no ground-truth reference available.
- Y / U / V PSNR: n/a
- Command used: n/a

#### Outcome

- **Pass**
- Caveats / follow-ups filed: #302 (decoder probe hard-coded resolution)
````

---

## Rules of thumb

- **Always read at least one PNG** when PNG sampling is in play. A run that
  doesn't error but produces black frames is not a pass.
- **Always describe what the image shows** in the [test-report
  template](#standardized-test-output-template). "Looks fine" is not a
  description — a reviewer should be able to tell at a glance whether you
  actually inspected the frame.
- **Always set `STREAMLIB_DISPLAY_FRAME_LIMIT`** for automated runs. winit +
  X11 don't always respect SIGTERM cleanly.
- **Run vivid first, real hardware second.** Vivid isolates your change from
  driver quirks; real hardware catches the driver quirks vivid hides.
- **Run PSNR when a reference frame exists.** For synthetic sources it
  catches silent color-space / plane-layout regressions that visual
  inspection misses.
- **Keep the PNG sampling interval coarse** (20–60 frames). Every sample is
  ~8 MB and the PNG encoder is on the display thread.
- **Report every E2E run using the
  [standardized template](#standardized-test-output-template)** so reviews
  and diffs across PRs are grep-able.
- **Unit tests still come first.** E2E runs are slow and noisy — use them to
  confirm, not to explore.

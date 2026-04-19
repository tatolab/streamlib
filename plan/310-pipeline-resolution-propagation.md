---
whoami: amos
name: Pipeline-wide resolution propagation for non-1080p roundtrips
status: pending
description: Design and implement a mechanism so camera → encoder → decoder → display can run at resolutions other than 1920x1080 without editing example sources. Includes a research + choice phase before implementation. Follow-up to #302 / #309.
github_issue: 310
dependencies:
  - "down:Retest camera + encoder + display roundtrip after Vulkan cleanup"
  - "down:GPU capability-based access (sandbox + escalate)"
adapters:
  github: builtin
---

@github:tatolab/streamlib#310

## ⚠️ Research & choice phase — run BEFORE PROMPT.md step 2 (announce)

When this task is pulled up for execution, **do not announce per PROMPT.md
step 2 yet**. The implementation shape is deliberately undecided — several
designs are viable and they differ in multi-runtime behavior, so the
choice has to be made by the user before a real scope can be announced.

Sequence:

1. Do the research below (concrete code reads, not speculation).
2. Present the options in this file — with the tradeoffs as they apply to
   *this* codebase, not generic hand-waving — and ask the user to pick.
3. Once the user picks, fill in the `## Branch`, `## Steps`, and
   `## Verification` sections of this file based on the choice.
4. **Then** announce per PROMPT.md step 2 with the chosen scope, and wait
   for the user's "proceed".

### Research questions (read the code, don't guess)

1. **Camera actual resolution** — how does `LinuxCameraProcessor` currently
   expose the resolution it actually negotiated with V4L2? Does it end up
   in the `Videoframe` it publishes, or only in logs? Look at
   `libs/streamlib/src/linux/processors/camera.rs`.
2. **Videoframe schema** — what fields does `Videoframe` carry today
   (width, height, fps, surface_id, ...)? Is every downstream processor
   already receiving width/height per frame, or only through its own
   Config? Check `libs/streamlib/src/_generated_/` and the encoder
   processor's `process()`.
3. **Encoder / decoder Config** — where do `H264EncoderProcessor::Config`,
   `H265EncoderProcessor::Config`, `H264DecoderProcessor::Config`,
   `H265DecoderProcessor::Config`, and `DisplayProcessor::Config` get
   their `width`/`height` today? Which of them currently default vs
   require an explicit value?
4. **Lazy vs eager init** — can `SimpleEncoder` / `SimpleDecoder` be
   created with dimensions discovered at first-frame time, or do
   `pre_initialize_session()` / `prepare_gpu_encode_resources()` require
   dimensions up-front? Could either grow a "configure from first frame"
   entry point without violating the pre-swapchain allocation requirement
   (see [docs/learnings/nvidia-dma-buf-after-swapchain.md](../docs/learnings/nvidia-dma-buf-after-swapchain.md))?
5. **Multi-runtime / cross-process link** — when two runtimes are wired
   together via iceoryx2 or MoQ, does anything besides message payloads
   cross the boundary? I.e., can a "runtime-global resolution variable"
   even be observed by the downstream runtime? Trace the link handshake
   in `libs/streamlib/src/core/pubsub.rs` and
   `libs/streamlib/src/core/link/` (and the MoQ transport if relevant).
6. **Existing precedent** — has any feature already solved "upstream
   format flows to downstream" in this codebase? FPS, pixel format, and
   sample rate are candidates. Check how `fps` reaches the MP4 writer
   today — see #272 (FPS propagation) and the current `Videoframe::fps`
   field.

### Options to present

Flesh each bullet out with the findings from the research phase before
showing this to the user. Do **not** paste this template verbatim — the
tradeoffs that actually matter depend on what the research turns up.

- **A. CLI-arg only, no pipeline-wide plumbing.** Accept `--width W
  --height H` in `vulkan-video-roundtrip` and `camera-display`; push the
  values into each processor's Config explicitly. Smallest scope.
  Tradeoff: doesn't help any other consumer — every new example has to
  repeat the wiring.
- **B. Runtime-global default resolution.** Add a `default_resolution` on
  `StreamRuntime` (or `RuntimeContext`) that processors inherit when
  their Config leaves width/height unset. Tradeoff: the user already
  flagged this — a second runtime connected via iceoryx2 or MoQ does
  **not** see the upstream runtime's globals, so a source/sink split
  across runtimes would silently fall back to a hardcoded default. Need
  to confirm via research Q5 whether this is truly fatal or just a
  caveat.
- **C. First-frame inspection (lazy init).** Downstream processors read
  `width`/`height` from the first `Videoframe` they receive and lazily
  configure their GPU resources. Crosses runtime boundaries cleanly
  because `Videoframe` serializes. Tradeoff: collides with the
  "pre-allocate before swapchain" rule — the first frame arrives *after*
  setup, by which point the display swapchain may already exist. May
  need a bounded max-resolution cap at setup() to size pools
  conservatively.
- **D. Explicit format-spec link.** A dedicated link type (alongside
  video / audio / encoded-video) that carries format metadata (width,
  height, fps, pixel_format). Encoders/decoders subscribe to it at
  setup() and block until the first spec arrives. Tradeoff: adds a new
  link pattern; needs MoQ/iceoryx2 support for small control messages.
- **E. Setup-time handshake.** Processors introspect their upstream
  during `setup()` (via a new `format_spec()` API on
  `ReactiveProcessor`) to learn the expected frame size. Tradeoff:
  couples processors to their upstream type; doesn't trivially cross a
  runtime boundary.

When presenting to the user:

- Lead with the option that *this* codebase's existing mechanisms favor
  (after the research, you'll know which one that is — don't guess
  now).
- Call out the multi-runtime implication explicitly for each option.
- Recommend a default and ask for confirmation; don't just list five
  equal options.

---

## Branch

_(To be filled after the user picks an option.)_

## Steps

_(To be filled after the user picks an option. Typical skeleton:)_

1. Plumb the chosen mechanism end-to-end (camera → encoder → decoder → display).
2. Teach `vulkan-video-roundtrip` (and optionally `camera-display`) to
   run at the resolution supplied through the new mechanism.
3. Add any processor-side adaptations required by the choice (lazy
   init, handshake, config defaults).

## Verification

_(To be filled after the user picks an option. Baseline that any choice
must meet:)_

- `camera → encoder → decoder → display` roundtrip at **1280x720**
  (via vivid's `VIDIOC_S_FMT` or the v4l2loopback motion scenario from
  [`docs/testing.md`](../docs/testing.md#v4l2loopback-motion-scenario))
  with zero `OUT_OF_DEVICE_MEMORY`, zero `DEVICE_LOST`, and visible
  content in sampled PNGs. Full standardized
  [test report](../docs/testing.md#standardized-test-output-template).
- Regression: the existing 1920x1080 roundtrip (vivid + Cam Link) still
  passes.
- If the chosen option touches the cross-runtime path, one scenario must
  exercise a two-runtime pipeline (iceoryx2 or MoQ).

## References

- #302 / PR #309 — removed the hardcoded decoder probe; documented the
  non-1080p gap as a follow-up.
- #272 — FPS propagation via `Videoframe::fps`. Closest existing
  precedent for "upstream format flows to downstream."
- [`docs/learnings/nvidia-dma-buf-after-swapchain.md`](../docs/learnings/nvidia-dma-buf-after-swapchain.md)
  — the pre-swapchain allocation rule that constrains option C (lazy init).
- [`docs/testing.md`](../docs/testing.md) — encoder/decoder scenario and
  v4l2loopback motion scenario for non-1080p sources.

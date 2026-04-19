---
whoami: amos
name: "[BLOCKED — do not start] Pipeline-wide color management (primaries, transfer, range, tone mapping)"
status: pending
description: "BLOCKED — queued behind #310 and the post-Vulkan-cleanup retest + CI work. Do NOT pick up. Umbrella for a standardized color-management pipeline across camera, encoder, decoder, display, and in-graph processors. Adds ColorInfo metadata to frames, leverages VK_KHR_sampler_ycbcr_conversion and _SRGB formats where possible, auto-detects from V4L2 / VUI / EDID, and adds a tone-mapper for gamut mismatches."
github_issue: 312
dependencies:
  - "down:Pipeline-wide resolution propagation for non-1080p roundtrips"
  - "down:Retest camera + encoder + display roundtrip after Vulkan cleanup"
  - "down:CI with Vulkan validation layer"
adapters:
  github: builtin
---

# 🛑 STOP — DO NOT WORK ON THIS PLAN YET 🛑

This plan is **intentionally queued**. It is a scoping document for
future work, not an active task. The user has explicitly said this
must not be started until the blocking work below is done.

**Blockers (all must be `completed` before this plan is eligible):**

- #310 — Pipeline-wide resolution propagation for non-1080p roundtrips
- #294 — Retest camera + encoder + display roundtrip after Vulkan cleanup
- #293 — CI with Vulkan validation layer

If any agent (human or AI) picks up this plan while any blocker is
still `pending` or `in_progress`, that is a mistake — close the
session and surface the misroute to the user.

When the blockers do clear, follow the research-then-announce pattern
from #310 before starting any implementation: confirm the architecture
below against the current code, present tradeoffs, get the user's
pick, then announce per PROMPT.md step 2. **No GitHub issues yet** —
the sub-ticket list below is a draft to open after that announce
step, not a backlog to grind through.

---

## Motivation

Today streamlib has no canonical representation of a frame's color
state. Each processor that converts YUV↔RGB hardcodes a matrix; the
display swapchain color space is chosen implicitly; vivid's V4L2
colorimetry is ignored on ingress; encoder VUI fields are not derived
from upstream metadata; there is no tone mapper anywhere.

Observable symptoms:

- vivid colors look wrong in the display and in decoded PNGs (the
  trigger for this plan).
- PSNR-via-ffmpeg can't be trusted across pipeline boundaries because
  matrix / range assumptions differ.
- Any future HDR / Rec.2020 / P3 source would land on a Rec.709
  swapchain with no tone mapping — content would clip.

## Design principles

One color system, like the RHI — not N parallel ones. Extend existing
core systems (RHI, GpuContext, frame message schema) rather than
building a new module alongside them.

Lean on built-in Vulkan mechanisms first; only write shader code for
the parts Vulkan can't do (3×3 primary conversion, tone mapping,
manual transfer curves when `_SRGB` doesn't apply).

## Architecture sketch

1. **`ColorInfo` on every frame.** Fields: `primaries`, `transfer`,
   `matrix`, `range`, optional `mastering_display` / `max_cll` for
   HDR. Carried on `Videoframe`, `Encodedvideoframe`, `PixelBuffer`,
   `VulkanTexture`. Defaulted to the source's advertised colorimetry,
   overridable by Config.
2. **Sources populate it.**
   - Camera: from V4L2 `V4L2_COLORSPACE_*`, `YCBCR_ENC`, `QUANTIZATION`.
   - Decoder: from SPS/VUI `colour_primaries`,
     `transfer_characteristics`, `matrix_coefficients`,
     `video_full_range_flag`.
   - File sources: from the container or an explicit Config.
3. **Sinks consume it.**
   - Display: drives swapchain `VkColorSpaceKHR` selection (from EDID
     / `vkGetPhysicalDeviceSurfaceFormatsKHR`) and inserts a tone
     mapper if source ≠ sink gamut.
   - Encoder: writes VUI/color-config fields into the bitstream so
     downstream decoders can recover the same `ColorInfo`.
4. **RHI-owned building blocks.**
   - `VK_KHR_sampler_ycbcr_conversion` sampler cache keyed by
     `(format, primaries, matrix, range)` — replaces hand-written
     NV12→RGB shader paths.
   - `_SRGB` image-format selection helper for SDR render targets.
   - Shader library: 3×3 primary conversions (BT.601 / 709 / 2020 /
     P3 / XYZ bridge), transfer curves (sRGB / PQ / HLG / linear /
     BT.1886), range expand / compact.
5. **Tone mapper as a processor.** Own slot in the graph, inserted
   automatically when the graph builder detects `source.ColorInfo`
   mismatches `sink.ColorInfo`. Algorithm: BT.2390 or ACES-fitted
   (pick during research phase).

## Proposed sub-tickets

These are draft scopes for GitHub issues — create the issues once
this umbrella is pulled up for execution and the research phase has
refined them.

| Draft | Title | Scope |
|-------|-------|-------|
| 312.1 | `ColorInfo` type + frame-message plumbing | Add to schema, `Videoframe`, `Encodedvideoframe`, `PixelBuffer`, `VulkanTexture`. Every existing processor threads it through. Default values preserve today's behavior. |
| 312.2 | Camera: populate `ColorInfo` from V4L2 colorimetry | Read `V4L2_COLORSPACE_*` / `YCBCR_ENC` / `QUANTIZATION` in `LinuxCameraProcessor` and stamp each published `Videoframe`. vivid regression fixture. |
| 312.3 | Adopt `VK_KHR_sampler_ycbcr_conversion` in camera + decoder | Replace the hand-written NV12 / YUYV → RGB shader paths with a sampler-conversion cache keyed by `ColorInfo`. Builds on #289. |
| 312.4 | Decoder: derive `ColorInfo` from SPS/VUI | Parse `colour_primaries`, `transfer_characteristics`, `matrix_coefficients`, `video_full_range_flag` out of H.264/H.265 VUI and attach to decoded frames. |
| 312.5 | Encoder: write VUI/color-config from upstream `ColorInfo` | Populate H.264/H.265 VUI fields so encoded output is self-describing. |
| 312.6 | Display: swapchain color-space selection | Query EDID / supported surface formats, pick a `VkColorSpaceKHR` that matches content where possible, otherwise pick the closest and mark `ColorInfo` mismatch for the tone mapper. |
| 312.7 | Tone-mapper processor | New processor with pluggable operator (BT.2390 / ACES-fitted). Graph builder inserts it automatically when source ≠ sink gamut. |
| 312.8 | RHI shader library for color math | 3×3 primary conversions, transfer curves, range expand/compact. Shared by the tone mapper and any ad-hoc converter. |
| 312.9 | PSNR fixtures + regression gate | Extend #305 with color-matrix bug-injection cases and add a vivid regression fixture that catches the original symptom. |

Ordering: 312.1 first (everything else depends on it). 312.2 + 312.4
in parallel (sources). 312.3 + 312.8 as enabling work for the RHI
side. 312.5 + 312.6 once sources and sinks both understand `ColorInfo`.
312.7 last (consumes the mature pipeline). 312.9 runs alongside to
gate regressions.

## Verification (umbrella-level)

- vivid source renders visually-correct colors end-to-end (camera →
  display, and camera → encode → decode → display). Read PNGs with
  the Read tool per [`docs/testing.md`](../docs/testing.md).
- A deliberately-injected BT.601 ↔ BT.709 matrix swap drops Y PSNR
  below the fail threshold via #305's rig.
- Decoded H.264 / H.265 output carries `ColorInfo` that round-trips
  through VUI (encode → decode produces the same `ColorInfo`
  upstream sent).
- A synthetic HDR10 source routed to an SDR display does not clip —
  tone mapper is inserted automatically and preserves highlights.
- No regressions on the existing 1920x1080 vivid + Cam Link roundtrips
  (per [`docs/testing.md`](../docs/testing.md)'s standardized report).

## References

- [`docs/learnings/camera-display-e2e-validation.md`](../docs/learnings/camera-display-e2e-validation.md)
  — PNG-sampling loop used for the visual gate.
- [`docs/testing.md`](../docs/testing.md#psnr--how-to-compute) — PSNR
  workflow; will absorb the fixture cases from 312.9.
- #289 — per-device `VkSamplerYcbcrConversion` scaffolding. 312.3
  generalizes this to a conversion cache keyed by `ColorInfo`.
- #305 — fixture PSNR rig. 312.9 extends it with color-bug injection.
- #310 — resolution propagation; similar "upstream format flows to
  downstream" shape, and the research-then-announce pattern this
  umbrella follows.

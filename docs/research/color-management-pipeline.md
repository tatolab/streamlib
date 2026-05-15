# Research: color management pipeline wiring

## Question

How should streamlib wire the remaining color-management work — engine-layer
color converter, encoder/decoder VUI, display swapchain selection, HDR
metadata, and tone mapping — now that `ColorInfo` plus the
`MasteringDisplay` / `ContentLight` sidecars are landed on
`VideoFrame` + `EncodedVideoFrame`?

## Context

`ColorInfo` (`primaries` / `transfer` / `matrix` / `range`, each
`Option<Enum>`) and the HDR sidecars are propagating through the
iceoryx2 + msgpack pipeline and cross-process via surface-share.
Producers (camera V4L2, blending compositor) are populating it; the
encoder passes input `ColorInfo` through into `EncodedVideoFrame`.

What is missing:

- **Consumers** of `ColorInfo` — the engine-layer color converter
  every YUV→RGB path goes through. Today three different shaders
  hand-roll the matrix with different assumptions:
  - `packages/camera/src/linux/shaders/nv12_to_rgba.comp` — BT.601 +
    full/limited flag.
  - `packages/camera/src/linux/shaders/yuyv_to_rgba.comp` — same.
  - `libs/streamlib-engine/src/vulkan/rhi/shaders/nv12_to_bgra.comp`
    — engine-layer duplicate.
  - `libs/vulkan-video/src/nv12_to_rgb.rs` —
    `VkSamplerYcbcrConversion`, BT.709 narrow-range, hardcoded at
    construction.
- **Bitstream self-description** — the encoder does not write VUI
  into H.264 / H.265 SPS; the decoder ignores any VUI that arrives.
- **Display swapchain** picks `B8G8R8A8_UNORM` + `SRGB_NONLINEAR`
  unconditionally regardless of frame `ColorInfo` or display EDID.
  `vkSetHdrMetadataEXT` is never called.
- **Tone mapping** doesn't exist; `BlendingCompositor` stamps
  `sRGB / Identity / Full` on output by assumption.

The "green vivid" symptom — vivid's `SMPTE170M` source rendering
green instead of the expected SMPTE test bars — is the visible
proxy for the engine-layer converter gap (the camera shader reads
the raw V4L2 quantization byte instead of consuming
`frame.color_info.range`).

## Architectural calls

### 1. Engine-layer color converter is the primitive, not `VK_KHR_sampler_ycbcr_conversion`

Build `VulkanColorConverter` as **one compute kernel per
`(src_format, dst_format)` pair**, push-constant-driven for the
per-frame `ColorInfo` state, with **closed-form transfer functions
inline in the shader** and a reserved 3D-LUT descriptor binding for
later tone mapping.

`VkSamplerYcbcrConversion` is the wrong primitive: its cache key
includes the immutable sampler in the descriptor-set layout AND the
pipeline layout, so every `(format, primaries, matrix, range)` tuple
is its own kernel — mid-stream `ColorInfo` change forces a full
pipeline rebuild. It also does not solve transfer curves, gamut
mapping, or tone mapping; those still live in your shader. The only
thing it gives over a hand-written compute kernel is hardware chroma
interpolation, which on NVIDIA Linux is shader-emulated anyway.

This matches libplacebo (the reference video color manager — `sh_var`
routes shader variables to push constants / uniform buffer at
dispatch time, matrices CPU-computed once per `pl_color_repr`),
GStreamer `glcolorconvert` (one shader template per `(src, dst)`,
matrices passed as uniforms), and Chromium `ui/gfx/color_transform`
(canonical pipeline: range expand → matrix → transfer-in → primaries
3×3 → transfer-out, all closed-form). No production color-management
engine uses `VkSamplerYcbcrConversion` as the primary path.

`sampler_ycbcr_conversion` stays available as a niche opt-in adapter
hook when a future zero-copy V4L2 multi-plane path needs hardware
chroma siting; never the default.

#### Push-constant struct (64 bytes)

```rust
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ColorConvertPushConstants {
    /// 3×3 YUV→RGB matrix, CPU-decomposed from (primaries, matrix, range)
    /// via a lookup table.
    yuv_to_rgb: [[f32; 3]; 3],
    range_offset: [f32; 3],         // {Y, Cb, Cr}
    range_scale:  [f32; 3],         // {Y, Cb, Cr}
    transfer_in:  u32,              // streamlib::Transfer enum
    transfer_out: u32,              // streamlib::Transfer enum
    flags: u32,                     // bit 0: apply_transfer
                                    // bit 1: apply_primaries
                                    // bit 2: tone_map
    _pad: u32,
}
```

Inside the spec minimum `maxPushConstantsSize = 128` and NVIDIA
RTX 3080's 256.

#### Per-axis default policy

`Option<T>` axes are eliminated CPU-side by
`ColorInfo::resolve_defaults() -> ResolvedColorInfo` before the
kernel sees them. The shader is unconditional.

| Axis | `None` falls back to |
|---|---|
| `primaries` | `Bt709` |
| `transfer` | `Srgb` (for RGB-format input) / `Bt709` (for YUV input) |
| `matrix` | `Smpte170m` (BT.601) — UVC convention; upgradeable to `Bt709` when `width >= 1280` later, per libplacebo `pl_color_repr_guess_yuv_for_resolution` |
| `range` | `Full` for RGB / `Limited` for YUV |

Matches V4L2's `V4L2_MAP_*_DEFAULT` macros (already in
`packages/camera/src/linux/v4l2_color.rs`) and Chromium's
`GuessGfxColorSpace`. **No `Unspecified` enum variant** — `None` is
the on-wire unknown; the resolver collapses it to a concrete value
before dispatch and the path that took the default is logged.

#### Sweep (engine-change pattern)

When this lands, every consumer of the old shapes migrates in the
same PR:

- Delete `packages/camera/src/linux/shaders/nv12_to_rgba.comp`
  and `yuyv_to_rgba.comp`; route through the engine converter.
- Delete `libs/streamlib-engine/src/vulkan/rhi/shaders/nv12_to_bgra.comp`;
  replace with parameterized kernel.
- Delete `libs/vulkan-video/src/nv12_to_rgb.rs` (the
  `VkSamplerYcbcrConversion` path); replace the vulkan-video
  consumer with the engine converter.
- Fold `libs/streamlib-engine/src/vulkan/rhi/vulkan_format_converter.rs`
  into the new converter or thin-wrap it.

### 2. Tone mapping: BT.2390 forward + BT.2446a inverse, regular processor in the graph

**Algorithm.** BT.2390 EETF (the closed-form piecewise hermite spline
in [ITU-R Report BT.2390](https://www.itu.int/dms_pub/itu-r/opb/rep/R-REP-BT.2390-4-2018-PDF-E.pdf))
forward; BT.2446a method A2 for the inverse path. Both bidirectional
on the same kernel via a push-constant curve selector. Matches
mpv's `--tone-mapping=auto` legacy default, OBS Studio's
`libobs/data/color.effect`, and FFmpeg's `tonemap` family. ACES-fitted
(Narkowicz coefficients) is the dominant *game-engine* choice but
adds warm/desat by design — wrong primitive for video tone mapping.
libplacebo's `pl_tone_map_spline` is the next iteration when in-tree
HDR content lands and adaptive scene-aware tone mapping becomes
worth the analysis-pass cost.

**Placement.** Regular processor in the graph (a `ReactiveProcessor`),
graph-builder-inserted on `ColorInfo` mismatch. The tone mapper
composes with the engine-layer color converter: the converter owns
primaries / matrix / range; the tone mapper owns the curve. Not an
RHI primitive consumed by the display — encoders targeting H.264
sRGB output from HDR sources need tone mapping just as much as the
display does, and putting it in the display forces parallel
re-implementation in the encoder.

This is the GStreamer-element / OBS-filter shape, not the
mpv-monolithic shape. mpv's renderer is single-process, single-render-
path and inlines tone mapping into the renderer; streamlib's
processor-mailbox shape is structurally GStreamer-shaped, and the
engine-model rule "one canonical way per concern" wants the tone
mapper to be a kernel-bearing processor backed by the converter.

**Compositing semantics — per-acquire.** Each input to a compositor
is converted into a canonical **scene-linear FP16 BT.2020 working
space** *before* the compositor's graphics kernel reads it. Matches
mpv (single working space, all inputs land in it) and OBS (every
source converts to canvas space at compose-input time). The
alternative — compose first, convert second — produces ringing on
edges between sources in unaligned linear-light spaces, and is
N² shaders for pairwise gamut combinations.

`BlendingCompositor` (`examples/camera-python-display/src/blending_compositor.rs`)
already stamps `sRGB / Identity / Full` on output by assumption —
i.e., it implicitly takes the per-acquire model (inputs are
sRGB-equivalent by convention, output is sRGB-equivalent by stamp).
Making this explicit lets it compose mixed-`ColorInfo` inputs without
the implicit assumption.

### 3. Display swapchain: WSI-primary, libdisplay-info reference

**EDID parse — hybrid.** Trust `vkGetPhysicalDeviceSurfaceFormatsKHR`
for *negotiation* (the driver/compositor is the only party that
knows what it will honor), and use `libdisplay-info` (Rust binding)
for HDR sidecar inference — primaries, transfer function, MaxCLL /
MaxFALL from the EDID HDR Static Metadata Data Block (CTA-861.3) —
when the WSI list under-reports. EDID is reference-only: if the WSI
list is suspiciously short and EDID claims HDR, log the discrepancy
and stay on the WSI answer. EDID is signal, not authority.

Read EDID from `/sys/class/drm/card*-*/edid` directly (world-readable
on every mainstream distro; no DRM master required).

**`VkColorSpaceKHR` selection — priority walk** over the WSI-returned
list. Match against the frame's `ColorInfo`:

1. PQ + BT.2020 → walk for `HDR10_ST2084_EXT` with
   `A2B10G10R10_UNORM_PACK32` or `R16G16B16A16_SFLOAT`.
2. Not present → frame is HDR but display isn't → walk for
   `EXTENDED_SRGB_LINEAR_EXT` + `R16G16B16A16_SFLOAT` (scRGB-style
   float scanout — Mesa Wayland-only). Engine tone-maps to display
   luminance.
3. Not present → fall to `SRGB_NONLINEAR_KHR` +
   `B8G8R8A8_UNORM/SRGB`. Engine tone-maps + gamut-maps to sRGB
   before scanout. This is the current code's only path; remains
   the catch-all.
4. Frame `ColorInfo` says sRGB → unconditionally pick
   `SRGB_NONLINEAR_KHR`. Don't promote to HDR.

**Driver-exposed reality (2025/2026):**
- NVIDIA production 570.x on X11: `SRGB_NONLINEAR` + (when a real
  HDMI 2.1 / DP HDR sink is present) `HDR10_ST2084_EXT`. Wayland is
  gappy until nvidia-open 595.58.03+.
- Mesa 25.1+ on Wayland: full `VK_EXT_swapchain_colorspace` set when
  the compositor supports `wp_color_management_v1` (Mutter 48+,
  KWin 6.2+).
- Mesa on X11: sRGB variants only — wide-gamut + HDR is Wayland-only
  on Mesa.

**HDR metadata.** Chain `vkSetHdrMetadataEXT` per-frame on transition
(not every frame — the driver caches it). Materialize
`VkHdrMetadataEXT` from the frame's `MasteringDisplay` /
`ContentLight` sidecars when the selected colorspace is one of the
PQ/HLG variants. The schema fields are already in wire format units
(1/50000 chromaticity, 0.0001 cd/m² luminance) that Vulkan accepts
directly — no unit conversion needed.

**Wayland posture — defer.** Wire the WSI negotiation now; it works
on X11 + DRI3 today and on Wayland through Mesa 25.1+ once the
compositor advertises `wp_color_management_v1`. Don't author a
parallel Wayland code path. mpv, GStreamer waylandsink, GTK, Qt all
converged on letting the WSI / libplacebo do the protocol
negotiation.

### 4. Codec VUI write + parse

streamlib's `ColorInfo` enums are **1:1 with H.273** (ISO/IEC 23091-2)
byte values, which is the same table H.264 Annex E, H.265 Annex E,
and AV1 §6.4.2 reuse. The mapping is identity at the value level;
only the C-FFI byte type differs.

**Encoder VUI write — single SPS chain.** Build
`StdVideoH264SequenceParameterSetVui` / `StdVideoH265SequenceParameterSetVui`
in `libs/vulkan-video/src/encode/session.rs::create_session_parameters`
from the configured `ColorInfo`. Set
`vui_parameters_present_flag = 1`, set
`video_signal_type_present_flag` when any of
`(primaries, transfer, matrix, range)` is `Some`, set
`colour_description_present_flag` when any of
`(primaries, transfer, matrix)` is `Some`. Use H.273 value `2`
(Unspecified) for axes that are `None` but the description block is
being written. `video_full_range_flag = matches!(range, Some(Full))`.

When `frame.color_info` is `None` on every axis, skip the VUI
entirely — set `vui_parameters_present_flag = 0` and leave
`pSequenceParameterSetVui` null. Don't fabricate a default.

This replaces the H.264 portion of `libs/vulkan-video/src/encode/vui_patch.rs`
(which exists today because the NVIDIA driver emits broken timing).
Timing info now rides in the SPS the driver emits, not as a
post-write rewrite. The H.265 VPS timing patch stays — the VPS is a
separate NAL, independently broken in the driver.

Pickup-time verification: confirm with `ffprobe` that NVIDIA emits
`vui_parameters_present_flag = 1` when given a non-null
`pSequenceParameterSetVui`. Driver Vulkan Video parameter-set bugs
are common; if the chained struct is ignored, extend `vui_patch.rs`
with a color-description-block patcher at the same offset as the
timing block.

**Decoder VUI parse — bitstream wins over passthrough.** The H.264
parser at
`libs/vulkan-video/src/nv_video_parser/vulkan_h264_decoder.rs:1207`
**already** parses VUI when present, populating `colour_primaries`
/ `transfer_characteristics` / `matrix_coefficients` /
`video_full_range_flag` on `VuiParameters`. Expose it on
`SimpleDecoder` via `current_color_info() -> Option<ColorInfo>` and
let the codec processors prefer it over the `EncodedVideoFrame`
passthrough. The bitstream is more trustworthy than the producer's
metadata, because muxers / network paths can re-encode metadata
while the bitstream is self-describing.

The H.265 parser at `vulkan_h265_decoder.rs:2562` short-circuits VUI
parsing. Extend it to walk `video_signal_type_present_flag` +
`colour_description_present_flag` + the four color bytes — only the
color-relevant subset. Full H.265 VUI parsing is huge and out of
scope.

**AV1 — defer.** No AV1 codec processor in `packages/` today; the
existing dev box (RTX 3090) has AV1 decode but not AV1 encode
hardware. File AV1 VUI in its own milestone tied to the AV1 codec
buildout.

### 5. Camera V4L2 detect — gap audit

Substantially landed via `packages/camera/src/linux/v4l2_color.rs`.
Known gaps:

- **One-shot at processor start.** Cache invalidation on mid-stream
  format change (driven by future resolution-propagation work) is
  not handled.
- **`v4l2_pix_format_mplane`** isn't on the color-query path; only
  single-plane `v4l2_pix_format`.
- **UVC quirks** — many cameras report `_DEFAULT` across the board
  and the SRGB-shorthand fallback (`sRGB primaries` + `sRGB transfer`
  + `BT.601 matrix` + `FULL range`) matches V4L2 convention but real
  cameras may output Limited range while claiming `_DEFAULT`. No way
  to know without empirical testing per device.

Small follow-up; can ride alongside the resolution-propagation
issue.

## Open questions for the user

- **312.8 (RHI shader library for color math) is subsumed by 312.3**
  (the engine-layer converter owns the kernel set). Close 312.8 as
  duplicate, or keep it as a tracking bucket for follow-on shader
  work?
- **Filing order**: 312.3 first (foundation), 312.4 + 312.5 in
  parallel (codec VUI parse / write), 312.6 (display), 312.7 (tone
  mapper) blocked-by 312.3, 312.2 small camera follow-up alongside
  the resolution-propagation work. Confirm.
- **AV1 milestone**: file as a new milestone or fold into the
  existing Vulkan Video coupling milestone?

## References

- ITU-R Report BT.2390-4 (tone mapping)
- ISO/IEC 23091-2 (codec-independent color metadata; H.273)
- [libplacebo GLSL system](https://libplacebo.org/glsl/),
  [libplacebo shaders/colorspace.c](https://github.com/haasn/libplacebo/blob/master/src/shaders/colorspace.c),
  [libplacebo tone_mapping.c](https://github.com/haasn/libplacebo/blob/master/src/tone_mapping.c)
- [GStreamer `glcolorconvert`](https://github.com/GStreamer/gst-plugins-base/blob/master/gst-libs/gst/gl/gstglcolorconvert.c)
- [OBS Studio `libobs/data/color.effect`](https://github.com/obsproject/obs-studio/blob/master/libobs/data/color.effect)
- [Bultje — Displaying video colors correctly](https://blogs.gnome.org/rbultje/2016/11/02/displaying-video-colors-correctly/)
- [VkColorSpaceKHR spec](https://registry.khronos.org/vulkan/specs/latest/man/html/VkColorSpaceKHR.html)
- [Mesa Vulkan WSI Wayland color management MR](https://gitlab.freedesktop.org/mesa/mesa/-/merge_requests/31991)
- [Khronos `VK_EXT_external_memory_acquire_unmodified`](https://docs.vulkan.org/features/latest/features/proposals/VK_EXT_external_memory_acquire_unmodified.html)
- [libdisplay-info](https://emersion.pages.freedesktop.org/libdisplay-info/libdisplay-info/info.h.html)
- In-tree:
  `packages/core/schemas/color_info.yaml`,
  `packages/core/schemas/{content_light,mastering_display}.yaml`,
  `packages/camera/src/linux/v4l2_color.rs`,
  `libs/streamlib-engine/src/vulkan/rhi/vulkan_compute_kernel.rs`,
  `libs/streamlib-engine/src/core/rhi/format_converter_cache.rs`,
  `libs/vulkan-video/src/nv12_to_rgb.rs`,
  `libs/vulkan-video/src/encode/session.rs`,
  `libs/vulkan-video/src/encode/vui_patch.rs`,
  `libs/vulkan-video/src/nv_video_parser/vulkan_h{264,265}_decoder.rs`,
  `libs/streamlib-engine/src/vulkan/rhi/vulkan_present_target.rs`.

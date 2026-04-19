<!--
Copyright (c) 2025 Jonathan Fontanez
SPDX-License-Identifier: BUSL-1.1
-->

# H.265 encoder quality configuration on Vulkan Video / NVIDIA — knob audit

Research deliverable for #330. This is an inventory, not an implementation
plan. #306 conflated several distinct "quality" concepts under one knob
(`VkVideoEncodeQualityLevelInfoKHR::quality_level`) and defaulted H.265 to
`0` because the NVIDIA driver reports `max_quality_levels = 1` for H.265 —
a framing the reviewer believes is wrong. This doc separates the concepts
and records current-vs-correct for each, then ends with a question list
for interactive review before any code change lands.

Scope: `libs/vulkan-video/` encode path only. Decoder and camera/display
are out of scope.

## Knob inventory

Four independent axes determine H.265 output quality on this pipeline.
They are **not** substitutes for each other — #306 treated the Vulkan
effort index as the master knob, which is only true for CPU-cost/effort
trade-offs, not for PSNR.

### 1. Vulkan-API encoder-effort index — `VkVideoEncodeQualityLevelInfoKHR::quality_level`

What it is: a vendor-defined effort level. Higher index = more analysis
work per frame (mode decision, RD-opt, motion-search depth), at the cost
of GPU time. It is **not** a codec parameter — nothing about it lands in
the SPS/PPS/VPS or slice header; it tunes the driver's internal encoder
behavior.

Where it's wired today:
- `libs/vulkan-video/src/encode/config.rs:371` — `SimpleEncoderConfig::quality_level: Option<u32>`
- `libs/vulkan-video/src/encode/config.rs:390` — `default_quality_level(codec)` → `0` for both codecs
- `libs/vulkan-video/src/encode/session.rs:158` — clamped to `max_quality_levels - 1`
- `libs/vulkan-video/src/encode/session.rs:789` — chained into session parameters when non-zero
- `libs/vulkan-video/src/encode/submit.rs:693` — re-set on first frame via `vkCmdControlVideoCodingKHR`

What the NVIDIA RTX 3090 driver reports (dumped via `h265_caps` bin):
- H.264: `max_quality_levels = 4` — indices `0..=3` accepted
- H.265: `max_quality_levels = 1` — only index `0` exists

The #305 PSNR rig showed H.264 PSNR is **invariant** across `0..=3` at
CQP 18. That measurement does not say "quality_level is useless," it
says "at this QP on these fixtures, the effort index did not move the
CQP-locked PSNR floor." Effort indices generally move:
- Rate-distortion mode decision (which partitions/modes are searched)
- Motion-estimation search range and sub-pel refinement
- CABAC context selection for rare syntax elements

None of which change the QP; so at fixed CQP with simple fixtures, PSNR
is expected to look flat. At CBR/VBR the effort index would show more
clearly because better mode decision hits the bitrate target with less
distortion.

**What the reviewer's objection is really about:** picking a default of
`0` on H.265 because "H.265 only exposes one level" reads the Vulkan
API's `max_quality_levels = 1` as if it said "H.265 quality cannot be
configured on this driver," which is wrong in two ways:
1. Even if the effort index is pinned to one value, every other axis
   below is still configurable — H.265 has more quality-relevant syntax
   switches than the H.264 spec does.
2. The knob is named `quality_level` in the public config, which invites
   callers to expect it to be *the* H.265 quality knob. It isn't.

### 2. H.265 stream-level profile/tier/level — SPS `profile_tier_level`

What it is: the declared stream capability tuple. Drives decoder
conformance (what buffers/bitrate a decoder must accept) and constrains
tool availability (e.g. Main vs. Main10 vs. Format Range Extensions).
Landed in the emitted SPS (and VPS).

Where it's wired today:
- `libs/vulkan-video/src/vk_video_encoder/vk_encoder_config_h265.rs:96-103` — `h265_profile::{MAIN, MAIN_10, MAIN_STILL_PICTURE, FORMAT_RANGE_EXTENSIONS, SCC_EXTENSIONS}`
- `libs/vulkan-video/src/vk_video_encoder/vk_encoder_config_h265.rs:325-343` — `init_profile_level()` auto-picks profile from bit depth + chroma subsampling and calls `determine_level_tier()` to pick the lowest passing level from `LEVEL_LIMITS_H265`
- `libs/vulkan-video/src/vk_video_encoder/vk_encoder_config_h265.rs:78-93` — `LEVEL_LIMITS_H265` table (Table A-1, levels 1.0 through 6.2)
- `libs/vulkan-video/src/encode/session.rs:633-637` — emitted as `StdVideoH265ProfileTierLevel` in both VPS and SPS

**Current behavior is correct in shape**: 8-bit 4:2:0 picks `MAIN`,
10-bit picks `MAIN_10`, level is derived from luma sample count and
(if set) bitrate/CPB budget. `general_progressive_source_flag = 1` and
`general_frame_only_constraint_flag = 1` are set, which matches typical
progressive 4:2:0 encodes.

**But it is not a quality knob**: profile/tier/level does not change the
encoded bits — it declares what a decoder must handle. Raising `level_idc`
from 4.1 to 5.1 does not make the encode better; it only lets you feed
the encoder larger resolutions or higher bitrates without a spec
violation. The reviewer's "proper H.265 configuration fix" is unlikely
to be here.

### 3. Rate control and QP — the actual PSNR driver

What it is: how many bits the encoder is allowed to spend. This is the
dominant knob for output quality; everything else is a secondary lever.

Where it's wired today:
- `libs/vulkan-video/src/encode/config.rs:62-90` — `RateControlMode::{Default, Cbr, Vbr, Cqp}` → Vulkan `VideoEncodeRateControlModeFlagsKHR`
- `libs/vulkan-video/src/encode/config.rs:463-473` — `SimpleEncoderConfig::to_encode_config()` picks mode from `preset`:
  - `Preset::Fast` → CQP 20 / 22 / 24 (I/P/B)
  - `Preset::Medium` → CQP 18 / 18 / 20 (default)
  - `Preset::Quality` → CQP 15 / 15 / 17
- `libs/vulkan-video/src/encode/submit.rs:631-700` — `VkVideoEncodeRateControlInfoKHR` + `VkVideoEncodeRateControlLayerInfoKHR` attached in `BeginVideoCoding` and `ControlVideoCoding`
- `libs/vulkan-video/src/encode/session.rs:582-583` — `pic_init_qp_minus26` (H.264 PPS) is derived from `const_qp_intra - 26`
- `libs/vulkan-video/src/encode/session.rs:735-739` — H.265 PPS `init_qp_minus26 = 0` **hardcoded** with comment "driver overrides to 0 (init_qp=26)"

H.265 caps from the dumper:
- `rateControlModes`: `{ DEFAULT, DISABLED (CQP), CBR, VBR }` — all four supported
- `minQp = 0`, `maxQp = 51` — full H.265 QP range exposed
- `PER_PICTURE_TYPE_MIN_MAX_QP` and `PER_SLICE_SEGMENT_CONSTANT_QP` —
  separate per-I/P/B QP is in the cap bitfield

**What's likely wrong or underused for H.265 quality:**

- **Default preset uses CQP 18, not VBR at a target bitrate.** CQP is
  the right mode for benchmarking (reproducible bits-per-frame) but the
  wrong default for a streaming product where the consumer expects "give
  me 6 Mbps at 1080p30." The #306 `complex_pattern` PSNR floor at
  ~29.5 dB is a CQP-18-on-complex-content artifact, not a driver issue.
- **The H.265 PPS `init_qp_minus26 = 0` hardcode is a workaround for a
  driver override, not a feature.** In CQP mode with high QP the
  slice-level `slice_qp_delta` is used to reach the target QP; the
  comment in `session.rs:737` shows we once set `-8` (= init_qp 18) and
  the driver silently overrode it to `0`, causing `slice_qp_delta` to
  produce the wrong effective QP. This is worth re-verifying on the
  current driver — if `INIT_QP_MINUS26` appears in
  `h265_encode_caps.std_syntax_flags` as "unsupported for non-zero
  values," the hardcode is correct; if not, we're carrying a stale
  workaround.
- **`VkVideoEncodeRateControlInfoKHR` extensions for H.265 are not chained.**
  The spec defines `VkVideoEncodeH265RateControlInfoKHR` (GOP structure,
  temporal sub-layer count, HRD compliance flag) and
  `VkVideoEncodeH265RateControlLayerInfoKHR` (per-layer useMinQp/MaxQp,
  use_max_frame_size). We chain only the generic structs, which means
  H.265-specific rate-control refinements are left at driver defaults.
- **B-frames disabled.** `num_b_frames = 0` in every preset (see config.rs:467-473).
  B-frames on H.265 are usually the single biggest PSNR/bitrate win at
  fixed bitrate — the driver reports `max_b_picture_l0_reference_count`,
  `max_l1_reference_count`, and `B_FRAME_IN_L0_LIST`/`B_FRAME_IN_L1_LIST`
  as nonzero, so the hardware supports them; we've chosen not to use
  them for latency reasons (`tuning_mode = LOW_LATENCY`). That's a valid
  trade-off for WebRTC/MoQ but it's a quality knob we've turned off.
- **The H.265 DPB count is fixed at `max_dpb_slots + 1 = 5`.**
  For I/P-only with one reference this is correct. With B-frames and/or
  temporal sub-layers we need at least 6-9 slots.

### 4. H.265 syntax switches in the SPS / PPS — tools we've left off

H.265 has roughly two dozen SPS/PPS flags that the driver either accepts
or rejects per-encode. We currently enable a handful and leave the rest
at zero. Driver caps (`std_syntax_flags` bitfield) enumerate which
`*_SET` / `*_UNSET` values the driver accepts; everything the driver
reports as supported is fair game.

Enabled today:
- SPS: `sps_temporal_id_nesting_flag`, `amp_enabled_flag`,
  `sample_adaptive_offset_enabled_flag`, `sps_sub_layer_ordering_info_present_flag`,
  `conformance_window_flag` (when the encode extent exceeds the requested
  dimensions).
- PPS: `cabac_init_present_flag`, `transform_skip_enabled_flag`,
  `cu_qp_delta_enabled_flag` (non-CQP only), `pps_loop_filter_across_slices_enabled_flag`,
  `deblocking_filter_control_present_flag`.
- Slice segment header: `slice_sao_luma_flag`, `slice_sao_chroma_flag`,
  `cu_chroma_qp_offset_enabled_flag`, `deblocking_filter_override_flag`.

**Explicitly disabled (quality loss):**
- `strong_intra_smoothing_enabled_flag = 0` — I-frame prediction
  smoothing. Small gain on natural content, usually enabled.
- `sps_temporal_mvp_enabled_flag = 0` — temporal MV prediction.
  Significant inter-frame PSNR gain when B-frames or long GOPs are used.
  Turned off is surprising for a streaming encoder.

**Not set / not surfaced, but supported by the cap flags:**
- `weighted_pred_flag` / `weighted_bipred_flag` — weighted prediction
  for fades and cross-dissolves.
- `entropy_coding_sync_enabled_flag` — Wavefront Parallel Processing
  (WPP). No PSNR effect by itself, but enables parallel entropy coding
  on decoders.
- `sign_data_hiding_enabled_flag` — small bitrate win from transform
  coefficient sign hiding.
- `constrained_intra_pred_flag` — error-resilience trade-off; off by
  default is correct for low-loss pipelines.
- `scaling_list_data_present_flag` — custom quantization matrices.
  H.265 default flat matrices are fine for most content.
- `pcm_enabled_flag` — PCM block escape. Rarely useful.

A proper H.265 quality configuration almost certainly turns on
`sps_temporal_mvp_enabled_flag` and `strong_intra_smoothing_enabled_flag`
at minimum.

## The `tuning_mode` knob (fifth axis, almost)

`VkVideoEncodeUsageInfoKHR::tuning_mode` is pinned to `LOW_LATENCY` at
`encode/session.rs:107`. Alternatives:
- `DEFAULT` — no bias
- `HIGH_QUALITY` — bias toward PSNR at the cost of latency
- `ULTRA_LOW_LATENCY` — tightest latency bound
- `LOSSLESS` — only valid with specific QP configurations

`LOW_LATENCY` is correct for WebRTC/MoQ but it informs the driver that
lookahead, complex rate control, and multi-frame analysis are disabled.
For a non-streaming "record to MP4" use case, `HIGH_QUALITY` would
unlock multi-frame analysis on NVIDIA's driver (the driver docs
explicitly tie this to `tuning_mode`). This is a product-shape question:
we may want the tuning mode to be a derived property of a caller-supplied
use-case enum rather than hardcoded.

## Current vs. correct summary

| Knob | Current H.265 setting | Likely correct | Action |
|---|---|---|---|
| `VkVideoEncodeQualityLevelInfoKHR::quality_level` | 0 (pinned, `max=1`) | 0 (driver only exposes one level) | Keep; rename public field to `effort_level` and document it is not the H.265 quality knob |
| Profile | Main 8-bit / Main-10 (auto) | Auto-select from bit depth | OK |
| Tier / level | Main tier, auto-derived | OK | OK |
| Rate control default | CQP 18 | VBR @ target bitrate (streaming) or explicit CQP opt-in (benchmarks) | Change default `Preset` rate-control shape |
| `VkVideoEncodeH265RateControlInfoKHR` | not chained | chain with GOP + HRD | Add |
| `VkVideoEncodeH265RateControlLayerInfoKHR` | not chained | chain with useMinQp/MaxQp | Add |
| `init_qp_minus26` (PPS) | 0, hardcoded | re-verify vs. driver caps | Audit |
| B-frames | 0 | ≥ 1 for non-`LOW_LATENCY` | Gate on use case |
| `sps_temporal_mvp_enabled_flag` | 0 | 1 | Flip |
| `strong_intra_smoothing_enabled_flag` | 0 | 1 | Flip |
| `weighted_pred_flag` | 0 | product-dependent | Decide |
| `entropy_coding_sync_enabled_flag` (WPP) | 0 | 1 for parallel decoders | Decide |
| `sign_data_hiding_enabled_flag` | 0 | 1 | Flip |
| `tuning_mode` | `LOW_LATENCY` (hardcoded) | driven by use case | Add a config knob |

## Question list for interactive review

Before any implementation lands, resolve these with Jonathan:

1. **Scope of #330 follow-up implementation.** Is this one PR that fixes
   every "Flip" row above, or does each flip get its own ticket with a
   PSNR-before/after? The existing PSNR rig (#305) would make a
   per-flip sweep tractable, but a single PR with all-on vs. all-off is
   shorter.

2. **Which reviewer-recollected fix was "the proper one"?** The most
   likely candidates based on this audit are:
   - `sps_temporal_mvp_enabled_flag = 1`
   - Chaining `VkVideoEncodeH265RateControlInfoKHR` / `...LayerInfoKHR`
   - Re-verifying the hardcoded `init_qp_minus26 = 0` against the
     driver's `INIT_QP_MINUS26` cap flag
   - Switching the default rate control mode away from CQP
   None of these are recorded in commit history for this branch;
   reviewer may be remembering work done on a parallel branch or an
   NVIDIA sample patch. Jonathan: does any of the above match the memory?
   If not, where should I look?

3. **`quality_level` naming.** The public `SimpleEncoderConfig::quality_level`
   maps 1:1 to a Vulkan *effort* knob that does not move PSNR at fixed
   CQP. Keeping the name invites future confusion. Rename to
   `effort_level` (or `gpu_effort`) and leave `quality_level` as a
   deprecated alias for one release, or hard-rename?

4. **Default rate-control mode.** `Preset::Medium → CQP 18` was chosen
   for reproducible benchmarking. For a streaming product, VBR at a
   target bitrate is usually the expected default. Should the presets
   be split into `Preset::BenchmarkCqp18` (explicit) and a bitrate-first
   default like `Preset::Streaming { target_bitrate: 6_000_000 }`?

5. **Tuning mode.** Should `tuning_mode` become a config-level enum
   (`Usage::Streaming → LOW_LATENCY`, `Usage::Recording → HIGH_QUALITY`,
   `Usage::Realtime → ULTRA_LOW_LATENCY`)? Keeping it hardcoded to
   `LOW_LATENCY` is correct for today's WebRTC/MoQ use cases but
   prevents any offline-recording pathway from getting the better
   quality NVIDIA offers in `HIGH_QUALITY` mode.

6. **B-frames gating.** Is `num_b_frames > 0` ever acceptable in this
   codebase, or is low-latency a hard invariant? If B-frames are
   acceptable for recording use cases, they should follow the same
   use-case enum as `tuning_mode`.

7. **`complex_pattern` PSNR floor (from #306).** #306 left it as "CQP
   18 + synthetic high-complexity content = 29.5 dB floor, orthogonal
   to `quality_level`." Is that still the read, or does Jonathan want
   the floor investigated as part of this H.265 work? The most
   obvious candidates are the two disabled SPS flags above plus a
   lower default QP; any of them would move the floor.

8. **Validation plan.** Before merging the implementation that follows
   this research, what's the pass bar?
   - PSNR delta per flipped flag via `e2e_fixture_psnr.sh` (existing
     infrastructure)
   - Bitrate-at-equal-PSNR measurement (needs new tooling)
   - Subjective PNG inspection only (fastest, least precise)
   Pick one.

## References

- #306 — PR #329, "expose encoder quality_level with real-time default"
- #305 — fixture-based PSNR rig (`libs/streamlib/tests/fixtures/e2e_fixture_psnr.sh`)
- `libs/vulkan-video/src/encode/session.rs` — session creation, SPS/PPS build
- `libs/vulkan-video/src/encode/submit.rs` — per-frame slice header, rate control attachment
- `libs/vulkan-video/src/vk_video_encoder/vk_encoder_config_h265.rs` — `LEVEL_LIMITS_H265`, profile/level/tier auto-selection
- `libs/vulkan-video/src/bin/h265_caps.rs` — full capability dumper (run against the target GPU before any decision)
- ITU-T H.265 (08/2023), Annex A (profiles/levels), Section 7.4 (SPS/PPS semantics), Section 8.5 (inter prediction) — canonical reference for the SPS/PPS flags enumerated above
- Vulkan 1.4 spec §49 "Video Coding" — `VkVideoEncodeH265*` structure semantics and which fields the driver may override

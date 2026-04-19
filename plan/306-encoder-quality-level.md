---
whoami: amos
name: Expose encoder quality_level with real-time default
status: completed
description: SimpleEncoderConfig doesn't expose quality_level. Default picks "requested_quality=5" clamped to driver max (H.264→3, H.265→0 on NVIDIA RTX 3090), so H.265 silently runs at the driver's worst quality. Add a configurable knob with a real-time-tuned default.
github_issue: 306
adapters:
  github: builtin
dependencies:
  - "down:Fixture-based PSNR rig for encoder/decoder roundtrips"
---

@github:tatolab/streamlib#306

## Branch

Create `feat/encoder-quality-level` from `main`.

## Steps

1. Add `quality_level: Option<u32>` to `SimpleEncoderConfig` (`libs/vulkan-video/src/encode/config.rs`). `None` = library default.
2. Pick a per-codec default tuned for real-time / low-latency:
   - Consult `VkVideoEncodeCapabilitiesKHR::maxQualityLevels` per codec.
   - Use the #305 PSNR rig to find the lowest quality level that still hits Y ≥ 30 dB on the natural reference image — that's the real-time default.
   - Stay consistent with the existing `VkVideoEncodeUsageInfoKHR::tuning_mode = LOW_LATENCY` intent.
   - Document the chosen defaults explicitly; do not silently pick driver zero for H.265.
3. Replace the hardcoded `requested_quality = 5` in `libs/vulkan-video/src/encode/session.rs` with the configured / defaulted value. Keep the caps-clamp only as a safety floor.
4. Keep every `VkVideoEncodeUsageInfoKHR` chain in sync — `encode/session.rs`, `encode/staging.rs`, and `rgb_to_nv12.rs` all push their own. Divergence re-triggers `VUID-vkCmdEncodeVideoKHR-pEncodeInfo-08206` (see #300).
5. Plumb the knob through streamlib: add `quality_level: Option<u32>` to `H264EncoderProcessor::Config` and `H265EncoderProcessor::Config`.
6. Log the final effective quality at INFO so operators can see actual vs requested.

## Verification

- `vulkan-video-roundtrip h264 /dev/video0 15` and `h265 /dev/video0 15` with default config: no gross H.265 banding/artifacts on natural scenes per PNG Read-tool inspection. Describe image content per the [test-report template](../docs/testing.md#standardized-test-output-template).
- Explicit `quality_level = Some(max)` visibly higher quality, slower encode.
- Explicit `quality_level = Some(0)` reproduces today's worst-quality H.265 — confirms the knob is wired.
- #305 PSNR rig at default quality: Y PSNR ≥ 30 dB on the natural reference.
- Send PNGs for at least one codec at default and max quality to the user's Telegram for visual sign-off.

## References

- PR #301 retest — observed H.265 banding at default quality: https://github.com/tatolab/streamlib/pull/301#issuecomment-4274680105
- #300 — keep all three `VkVideoEncodeUsageInfoKHR` chains in sync.
- #305 — PSNR rig provides the numeric floor for the default.

---
whoami: amos
name: Encoder src picture profile mismatch
status: in_review
description: Chain VkVideoEncodeUsageInfoKHR into the RGB→NV12 converter's source image profile so srcPictureResource.imageViewBinding is compatible with the session's video profile, silencing VUID-vkCmdEncodeVideoKHR-pEncodeInfo-08206.
github_issue: 300
adapters:
  github: builtin
---

@github:tatolab/streamlib#300

## Branch

Create `fix/encode-src-profile-mismatch` from `main`.

## Steps

1. In `libs/vulkan-video/src/rgb_to_nv12.rs` (image create info near line 178):
   - Build `let mut encode_usage = vk::VideoEncodeUsageInfoKHR::builder().tuning_mode(vk::VideoEncodeTuningModeKHR::LOW_LATENCY);`
   - `profile_info = profile_info.push_next(&mut encode_usage);` before the codec-specific `push_next`.
   - Mirror every `VideoEncodeUsageInfoKHR` field that `encode/session.rs:106` sets on the session profile so the two chains are identical.
2. If `SimpleEncoderConfig` ever exposes configurable tuning/usage, thread a single source of truth through both the session profile (`encode/session.rs`) and the source image profile (`rgb_to_nv12.rs`, `encode/staging.rs`).
3. Confirm `encode/staging.rs:284` (other encode-src path) still matches — should already include `LOW_LATENCY`.

## Verification

- `VK_LOADER_LAYERS_ENABLE=*validation*` vivid H.264 roundtrip (5s, `/dev/video2`): zero `VUID-vkCmdEncodeVideoKHR-pEncodeInfo-08206` instances.
- Same for H.265.
- End-to-end still produces a valid bitstream (`frames_encoded` > 0, `frames_decoded` = `frames_encoded`).

---
whoami: amos
name: '@github:tatolab/streamlib#251'
adapters:
  github: builtin
description: Remove Vulkan Video and FFmpeg Codec Code — Strip all custom encoder/decoder code from streamlib in preparation for nvpro-vulkan-video integration. Branch refactor/remove-vulkan-video-code from main.
github_issue: 251
blocks:
- '@github:tatolab/streamlib#250'
---

@github:tatolab/streamlib#251

## Branch

Create `refactor/remove-vulkan-video-code` from `main` (after #249 + #250 merge).

## What to Delete

See the full list in the GitHub issue. Key deletions:
- All `vulkan/rhi/ffmpeg_vulkan_*` files
- All `vulkan/rhi/vulkan_video_*` files
- `vulkan/rhi/decoder_utils/` directory
- Codec-specific RHI shaders (bgra_to_nv12, dpb_to_bgra, ycbcr_to_bgra)
- Custom processor files (vulkan_h264_encoder/decoder, bitstream_writer, video_frame_writer)
- Codec schemas and generated configs
- `linux/ffmpeg/` directory
- FFmpeg dependencies from Cargo.toml
- Plan files: 207, 233, vulkan-video-decoder, vulkan-video-encoder-validation-fixes

## What to Update

Module declarations, lib.rs exports, streamlib.yaml, embedded_schemas.rs — remove references to deleted files.

## Note

The h264_encoder.rs and h264_decoder.rs processors in linux/processors/ will be temporarily stubbed or removed. They get rewritten as thin wrappers in #254.

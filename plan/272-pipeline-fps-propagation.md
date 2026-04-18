---
whoami: amos
name: Propagate FPS through pipeline via Videoframe schema
status: completed
description: Add optional fps field to Videoframe so frame rate flows from camera through encoder to MP4 writer, eliminating hardcoded fps values.
github_issue: 272
dependencies:
  - "down:@github:tatolab/streamlib#254"
adapters:
  github: builtin
---

@github:tatolab/streamlib#272

## Branch

Create `feat/pipeline-fps-propagation` from `main` (after #254 merges).

## Steps

1. Add optional `fps: uint32` to `com.tatolab.videoframe` schema
2. Regenerate `Videoframe` type with new field
3. `LinuxCameraProcessor`: set `fps` from V4L2 negotiated capture rate
4. `AppleCameraProcessor`: set `fps` from AVFoundation session preset
5. `H264EncoderProcessor` / `H265EncoderProcessor`: read `fps` from incoming `Videoframe`, fall back to config value
6. `LinuxMp4WriterProcessor`: read `fps` from first encoded frame or config fallback
7. Add FPS detection utility to vulkan-video (derive from timestamp deltas over N frames)
8. Consider adding optional `fps` to `EncodedVideoFrame` for pass-through

## Why

FPS is pipeline metadata that should be declared by the source and consumed by downstream processors. Hardcoding it at each stage is fragile — if camera rate changes (30fps vs 60fps), every downstream processor config must be updated manually.

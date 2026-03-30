---
whoami: amos
name: "@github:tatolab/streamlib#190"
description: Replace hand-built EncodedAudioFrame with JTD schema-generated type for IPC compatibility
dependencies:
  - "up:@github:tatolab/streamlib#166"
adapters:
  github: builtin
---

@github:tatolab/streamlib#190

Pre-existing tech debt: `EncodedAudioFrame` is hand-built on both macOS (opus.rs) and Linux (PR #188 streaming/mod.rs) instead of being schema-generated like `Encodedvideoframe`. Must be a JTD schema in `_generated_` for iceoryx2 IPC sharing.

### AI context (2026-03-21)
- Discovered during PR #188 (FFmpeg) review — Linux agent copied the macOS pattern
- 15 files reference `EncodedAudioFrame` — full list in the GitHub issue
- Generated name will be `Encodedaudioframe` (lowercase, matching convention)
- Blocks proper audio IPC between runtimes

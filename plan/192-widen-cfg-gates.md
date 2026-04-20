---
whoami: amos
name: '@github:tatolab/streamlib#192'
description: Linux — widen macOS-only cfg gates for runtime, codecs, and telemetry
adapters:
  github: builtin
blocked_by:
- '@github:tatolab/streamlib#166'
blocks:
- '@github:tatolab/streamlib#164'
- '@github:tatolab/streamlib#165'
- '@github:tatolab/streamlib#167'
---

@github:tatolab/streamlib#192

Systematic audit: many `#[cfg(any(target_os = "macos", target_os = "ios"))]` gates in runtime, codec wrappers, and streaming need to include Linux now that implementations exist. Without this, `StreamRuntime::start()` on Linux skips telemetry, surface store, and codec wrappers don't create FFmpeg implementations.

### AI context (2026-03-21)
- ~30 cfg gates identified across runtime.rs, video_encoder.rs, video_decoder.rs, mp4_muxer.rs, streaming/mod.rs
- Some are legitimately Apple-only (IOSurface, Metal texture, NSApplication)
- Key ones to widen: telemetry init, surface store, audio clock, codec wrappers
- Blocks end-to-end runtime on Linux even though all pieces exist

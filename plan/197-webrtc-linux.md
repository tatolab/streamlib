---
whoami: amos
name: '@github:tatolab/streamlib#197'
description: Linux — widen WebRTC WHIP/WHEP and RTP to cross-platform
adapters:
  github: builtin
blocked_by:
- '@github:tatolab/streamlib#166'
blocks:
- '@github:tatolab/streamlib#192'
---

@github:tatolab/streamlib#197

WebRTC WHIP/WHEP processors and RTP NAL parsing are macOS-only despite using cross-platform webrtc-rs. Blocks all Linux streaming.

### AI context (2026-03-22)
- Discovered by parity audit — surprise finding
- webrtc-rs is fully cross-platform, gates are unnecessary
- NAL parsing may depend on `apple::videotoolbox` — needs cross-platform alternative
- High-impact: blocks camera→WebRTC streaming pipeline on Linux

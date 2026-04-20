---
whoami: amos
name: "Fix cross-platform examples broken on Linux"
status: in_review
description: 'Stale/broken examples that don''t compile on Linux (camera-audio-recorder, microphone-reverb-speaker, screen-recorder, camera-python-display, webrtc-cloudflare-stream, grayscale-plugin). Mostly macOS-only code without cfg gates; webrtc-cloudflare-stream has stale port names from #207. Not caused by #322 but surfaced during review.'
github_issue: 358
adapters:
  github: builtin
---

@github:tatolab/streamlib#358

See the GitHub issue for full context.

## Priority

medium

## Parent

#322 / #319 umbrella (for capability-split-related) or infrastructure.

---
whoami: amos
name: iceoryx2 Slice API Migration
status: pending
description: Replace fixed-size FramePayload with iceoryx2 slice-based publish_subscribe for variable-length payloads. Branch feat/iceoryx2-slice-api from main.
github_issue: 250
dependencies:
  - "down:@github:tatolab/streamlib#249"
adapters:
  github: builtin
---

@github:tatolab/streamlib#250

## Branch

Create `feat/iceoryx2-slice-api` from `main` (after #249 merges).

## Source Commit

Reference commit `00fa18d` on `feat/233-ffmpeg-vulkan-codecs` for the exact diff. This commit is pure IPC infrastructure — no Vulkan code involved.

## Wire Format

```
[FrameHeader: 204 bytes][data: N bytes]
```

FrameHeader: port_key (64B) + schema_name (128B) + timestamp_ns (8B) + len (4B) = 204 bytes.
Data: msgpack-serialized frame (variable length, allocated in iceoryx2 shared memory).

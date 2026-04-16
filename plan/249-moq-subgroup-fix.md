---
whoami: amos
name: MoQ Subgroup Keyframe Fix
status: completed
description: "Bump moq-transport to 0.14.1 (includes LargestObject filter fix) and apply per-GOP subgroup creation. Branch fix/moq-subgroup-keyframe from main."
github_issue: 249
dependencies: []
adapters:
  github: builtin
---

@github:tatolab/streamlib#249

## Branch

Create `fix/moq-subgroup-keyframe` from `main`.

## Changes

### 1. Bump moq-transport to 0.14.1

The `FilterType::LargestObject` fix is already upstream in 0.14.1
(commit [englishm/moq-rs@9373bc2](https://github.com/englishm/moq-rs/commit/9373bc2)).
No tatolab fork needed — the tatolab/moq-rs fork has been deleted.

- Root `Cargo.toml`: change `moq-transport = "0.14"` → `"0.14.1"`
- Root `Cargo.toml`: remove `[patch.crates-io] moq-transport = { path = "vendor/moq-transport" }`
- Delete `vendor/moq-transport/` directory entirely

### 2. Per-GOP subgroup creation

From commit `43f45b5` on `feat/233-vulkan-video-decoder`:
- `core/streaming/moq_session.rs` — create new subgroup on keyframe

**WARNING**: Commit `ddbb7c5` REVERTS this fix in `moq_session.rs`. Do NOT take that regression.

### 3. Encoder keyframe config

From commit `43f45b5`:
- `core/processors/h264_encoder.rs` — `keyframe_interval_seconds` config field
- `_generated_/com_streamlib_h264_encoder_config.rs` — generated config with new field
- `schemas/com.streamlib.h264_encoder.config@1.0.0.yaml` — schema update
- `examples/moq-roundtrip/src/main.rs` — example uses new config

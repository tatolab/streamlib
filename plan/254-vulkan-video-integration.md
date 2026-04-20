---
whoami: amos
name: '@github:tatolab/streamlib#254'
adapters:
  github: builtin
description: Integrate nvpro-vulkan-video as libs/vulkan-video — Copy nvpro-vulkan-video crate into libs/vulkan-video, create thin processor wrappers for H.264/H.265 encode/decode. Branch feat/vulkan-video-integration from main.
github_issue: 254
blocks:
- '@github:tatolab/streamlib#253'
---

@github:tatolab/streamlib#254

## Branch

Create `feat/vulkan-video-integration` from `main` (after #253 merges).

## Steps

See `libs/vulkan-video/MIGRATION_PLAN.md` for full details.

1. Copy `~/Repositories/tatolab/nvpro-vulkan-video/` → `libs/vulkan-video/`
2. Adjust `Cargo.toml` to use workspace dependency coordinates
3. Register workspace member in root `Cargo.toml`
4. Add as dependency in `libs/streamlib/Cargo.toml`
5. Rewrite `linux/processors/h264_encoder.rs` — thin wrapper around `SimpleEncoder`
6. Rewrite `linux/processors/h264_decoder.rs` — thin wrapper around `SimpleDecoder`
7. Add H.265 encoder/decoder processors
8. Integration test: encode/decode roundtrip on RTX 3090

## Codec Recommendation

Use **H.265** for initial integration (66.83 dB encode, 45.79 dB decode — production quality).
H.264 quality fixes are in progress in the nvpro-vulkan-video repo but not blocking.

## Public API

```rust
// Encoder
let mut enc = SimpleEncoder::new(SimpleEncoderConfig {
    width: 1920, height: 1080, fps: 30,
    codec: Codec::H265, preset: Preset::Medium,
    streaming: true, idr_interval_secs: 2,
    ..Default::default()
})?;
let packets = enc.encode_image(rgba_image_view, Some(timestamp_ns))?;

// Decoder
let mut dec = SimpleDecoder::new(SimpleDecoderConfig {
    codec: Codec::H265, ..Default::default()
})?;
let frames = dec.feed(&h265_bytes)?;
```

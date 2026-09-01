# The NV12→RGBA converter silently rescales when its extent isn't the DPB's

## Symptom

Decoded video looks right — correct colours, correct orientation, correct
extent, no validation errors, no log warnings — but PSNR against the source
collapses on detailed content while flat content scores unchanged:

```text
  decoded sample                   Y(dB)     U(dB)     V(dB)   verdict
  complex_pattern__0               26.96     28.25     23.60   FAIL     <- was 48.82
  gradient_horizontal__0           46.51       inf       inf   PASS     <- was 53.36
  gradient_vertical__0             56.01       inf       inf   PASS     <- was 55.03
  solid_red__0                     48.13     48.13       inf   PASS     <- unchanged
```

The tell is the **axis selectivity**: content that varies along one axis
degrades, content that varies along the other does not, and flat content is
untouched. Above, the degraded references are the ones with vertical detail
(`gradient_horizontal` ramps top-to-bottom despite its name), and the
untouched one ramps left-to-right. That pattern is a resample along the
degraded axis, not a colour-conversion or plane-layout bug.

## Root cause

`shaders/nv12_to_rgb.comp` samples the DPB through a `sampler2D` with
**normalized** coordinates derived from a push-constant `resolution` that is
the *converter's own* extent:

```glsl
vec2 uv = (vec2(coord) + 0.5) / vec2(registers.resolution);
vec4 color = texture(nv12Input, uv);
```

Normalized coordinates address the whole source image regardless of its size.
So the converter does not read the DPB texel-for-texel — it maps the DPB's
full extent onto its own. When the two agree the mapping is the identity and
the sampler's filtering never fires. When they disagree it is a **bilinear
resample**, which is invisible in every gate except a per-pixel one.

Both codecs pad the coded picture up to a block size — H.264 to the 16-sample
macroblock, H.265 to the CTU — so a 1920x1080 stream is coded at 1920x1088.
It is exactly this pad that tempts you to size the converter to the *displayed*
extent, which is the one thing it must not be.

## The constraint

**The converter's extent must equal the extent of the image it samples** — the
value handed to `start_video_sequence`, which is what the DPB images are
created at. Not the conformance-windowed extent, not a block-aligned rounding
of the caps, not "what the consumer wants".

The conformance window belongs in the **readback's copy region**, which takes
an offset and an extent and does an exact texel copy:

```rust
let copy_region = vk::BufferImageCopy {
    image_offset: vk::Offset3D { x: window.origin_x as i32, y: window.origin_y as i32, z: 0 },
    image_extent: vk::Extent3D { width: window.width, height: window.height, depth: 1 },
    ..
};
```

That is the difference between cropping a picture and resizing it.

## Why nothing caught it

- No Vulkan error: sampling a differently-sized image through a sampler is
  legal and is what samplers are for.
- No extent mismatch downstream: the published frame is the size the consumer
  expects, because the resample delivered that size.
- Flat and low-frequency content scores unchanged, so a gate built on solid
  colours or a single smooth gradient passes.

Only a per-pixel metric against a **detailed** reference sees it. Keep at
least one high-frequency reference in any codec PSNR set for exactly this
class of defect — and treat "one axis degraded, the other not" as a rescale
until proven otherwise.

## Where this lives

`runtime/streamlib-engine/src/vulkan/video/nv12_to_rgb.rs` (the converter and
its push constant), `shaders/nv12_to_rgb.comp` (the normalized sample), and
the two construction sites in
`runtime/streamlib-engine/src/vulkan/video/decode/session.rs`, which both take
the extent their own `start_video_sequence` call was handed. The window is
applied in `decode/mod.rs`'s pending-frame drain.

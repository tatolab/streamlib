# NV12 → BGRA shader fixture

Bit-exact regression lock for `vulkan/rhi/shaders/nv12_to_bgra.comp`.

| File | Contents |
| --- | --- |
| `input_nv12_full_range_64x32.raw` | 3072 bytes — 64×32 NV12 full-range, deterministic varying Y/U/V (Y plane then bi-planar UV) |
| `expected_bgra_64x32.png` | RGBA8 PNG snapshot of the shader's BGRA output for that input |

The accompanying test `nv12_to_bgra_matches_committed_png_fixture` runs the
shader against the input and asserts byte equality (no tolerance) with the
PNG. Any change to the shader, its dispatch path, or the
`VulkanFormatConverter` plumbing will fail the test.

## Regenerating the fixture

When a shader change is intentional, regenerate the fixture:

```bash
cargo test -p streamlib --lib \
    vulkan::rhi::vulkan_format_converter::tests::regenerate_nv12_to_bgra_fixture \
    -- --ignored --nocapture
```

Both fixture files are written to this directory by the regenerator. Commit
the result alongside the shader/host change.

## Why a snapshot, not an external ground truth

The fixture's job in this repo is *regression locking*: catching unintended
drift in the shader's output. An external tool (e.g. ffmpeg) using a
slightly different BT.601 matrix would force a permanent ±N tolerance and
hide small, real changes. Snapshotting our own shader output keeps the
comparison bit-exact and pushes any deliberate math changes through the
fixture-regeneration workflow above.

The CPU-reference test
(`nv12_full_range_to_bgra_matches_cpu_reference`) is the complementary
correctness check — it independently re-implements the BT.601 full-range
matrix in Rust and verifies the GPU agrees within ±1 per channel.

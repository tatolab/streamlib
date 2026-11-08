// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// One test-pattern frame: SMPTE bars, the potato sprite over them, the caption
// panel over both. Every channel is blended as a whole byte, so every device
// writes the same bytes for the same push constants.

[[vk::binding(0, 0)]] Sampler2D potato_sprite_texture;
[[vk::binding(1, 0)]] Sampler2D caption_panel_texture;
[[vk::binding(2, 0)]] [format("rgba8")] RWTexture2D<float4> test_pattern_frame;

struct TestPatternFramePushConstants {
    uint frame_width;
    uint frame_height;
    int potato_sprite_left;
    int potato_sprite_top;
    uint potato_sprite_width;
    uint potato_sprite_height;
    int caption_panel_left;
    int caption_panel_top;
    uint caption_panel_width;
    uint caption_panel_height;
};
[[vk::push_constant]] ConstantBuffer<TestPatternFramePushConstants> placement;

static const uint SMPTE_BAR_COUNT = 8u;
static const uint4 SMPTE_BAR_COLORS_RGBA[SMPTE_BAR_COUNT] = {
    uint4(255u, 255u, 255u, 255u), // white
    uint4(255u, 255u, 0u, 255u),   // yellow
    uint4(0u, 255u, 255u, 255u),   // cyan
    uint4(0u, 255u, 0u, 255u),     // green
    uint4(255u, 0u, 255u, 255u),   // magenta
    uint4(255u, 0u, 0u, 255u),     // red
    uint4(0u, 0u, 255u, 255u),     // blue
    uint4(0u, 0u, 0u, 255u)        // black
};

// An `rgba8_unorm` texel holds `byte / 255` exactly enough that rounding
// recovers the byte.
uint4 texel_bytes(Sampler2D overlay_texture, int2 within_overlay) {
    return uint4(round(overlay_texture.Load(int3(within_overlay, 0)) * 255.0));
}

// `above` composited over an opaque `below`, rounded to the nearest byte.
uint4 composite_over_opaque(uint4 above, uint4 below) {
    if (above.a == 0u) {
        return below;
    }
    if (above.a == 255u) {
        return above;
    }
    uint3 blended = (above.rgb * above.a + below.rgb * (255u - above.a) + 127u) / 255u;
    return uint4(blended, 255u);
}

// The overlay byte quadruple covering frame pixel `at`, or a transparent one
// where the overlay placed at `top_left` does not cover it.
uint4 overlay_bytes_at(Sampler2D overlay_texture, int2 at, int2 top_left, uint2 overlay_extent) {
    int2 within_overlay = at - top_left;
    if (any(within_overlay < int2(0)) || any(uint2(within_overlay) >= overlay_extent)) {
        return uint4(0u);
    }
    return texel_bytes(overlay_texture, within_overlay);
}

[numthreads(16, 16, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint2 at = invocation.xy;
    if (at.x >= placement.frame_width || at.y >= placement.frame_height) {
        return;
    }

    uint bar = min((at.x * SMPTE_BAR_COUNT) / placement.frame_width, SMPTE_BAR_COUNT - 1u);
    uint4 pixel = SMPTE_BAR_COLORS_RGBA[bar];

    pixel = composite_over_opaque(
        overlay_bytes_at(
            potato_sprite_texture,
            int2(at),
            int2(placement.potato_sprite_left, placement.potato_sprite_top),
            uint2(placement.potato_sprite_width, placement.potato_sprite_height)),
        pixel);
    pixel = composite_over_opaque(
        overlay_bytes_at(
            caption_panel_texture,
            int2(at),
            int2(placement.caption_panel_left, placement.caption_panel_top),
            uint2(placement.caption_panel_width, placement.caption_panel_height)),
        pixel);

    test_pattern_frame[at] = float4(pixel) / 255.0;
}

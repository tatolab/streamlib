// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Three-layer Porter-Duff "over" compositor with animated picture-in-picture
// chrome. Every source is premultiplied: the overlay generator hands over a
// premultiplied skia canvas and the skeleton pass writes premultiplied, so
// `src.rgb + dst.rgb * (1 - src.a)` is the whole blend.
//
// Layer-size contract: the video and overlay layers are sampled at the same
// screen UV as the output and must match its extent. The pose layer may be any
// size — it is bilinearly resampled into the PiP rect by the hardware sampler.

#version 450

layout(location = 0) in vec2 screen_uv;
layout(location = 0) out vec4 composited_colour;

layout(set = 0, binding = 0) uniform sampler2D video_from_upstream;
layout(set = 0, binding = 1) uniform sampler2D overlay_from_neon_source;
layout(set = 0, binding = 2) uniform sampler2D pose_from_skeleton_overlay;

layout(push_constant, std430) uniform BreakingNewsCompositePushConstants {
    vec2 frame_extent_in_pixels;
    // Bit 0 the video layer, bit 1 the overlay layer, bit 2 the pose layer.
    uint present_layer_mask;
    // 0.0 fully off-screen right, 1.0 docked.
    float pip_slide_progress;
} pc;

const uint VIDEO_LAYER_BIT = 1u;
const uint OVERLAY_LAYER_BIT = 2u;
const uint POSE_LAYER_BIT = 4u;

const vec4 CYBER_CYAN = vec4(0.0, 0.94, 1.0, 1.0);
const vec4 CYBER_YELLOW = vec4(0.988, 0.933, 0.039, 1.0);
const vec4 CYBER_WHITE = vec4(1.0, 1.0, 1.0, 1.0);
const vec4 CYBER_DARK = vec4(0.06, 0.06, 0.08, 0.95);

// PiP geometry, as a fraction of the screen.
const float PIP_WIDTH = 0.28;
const float PIP_HEIGHT = 0.35;
const float PIP_MARGIN = 0.02;
const float PIP_BORDER = 0.004;
const float TITLE_BAR_HEIGHT = 0.045;

bool inside(vec2 uv, vec2 low_corner, vec2 high_corner) {
    return uv.x >= low_corner.x && uv.x <= high_corner.x
        && uv.y >= low_corner.y && uv.y <= high_corner.y;
}

vec4 over(vec4 source, vec4 destination) {
    return vec4(
        source.rgb + destination.rgb * (1.0 - source.a),
        source.a + destination.a * (1.0 - source.a)
    );
}

vec4 draw_pip_frame(vec2 uv, float slide_progress, vec4 base) {
    float slide_offset = (1.0 - slide_progress) * (PIP_WIDTH + PIP_MARGIN + 0.1);

    float pip_left = 1.0 - PIP_MARGIN - PIP_WIDTH + slide_offset;
    float pip_right = 1.0 - PIP_MARGIN + slide_offset;
    float pip_top = PIP_MARGIN;
    float pip_bottom = PIP_MARGIN + PIP_HEIGHT;

    float title_top = pip_bottom;
    float title_bottom = pip_bottom + TITLE_BAR_HEIGHT;

    float frame_left = pip_left - PIP_BORDER;
    float frame_right = pip_right + PIP_BORDER;
    float frame_top = pip_top - PIP_BORDER;
    float frame_bottom = title_bottom + PIP_BORDER;

    if (!inside(uv, vec2(frame_left, frame_top), vec2(frame_right, frame_bottom))) {
        return base;
    }

    bool inside_outer_border = inside(uv,
        vec2(frame_left + PIP_BORDER, frame_top + PIP_BORDER),
        vec2(frame_right - PIP_BORDER, frame_bottom - PIP_BORDER));
    if (!inside_outer_border) {
        return CYBER_CYAN;
    }

    float inner_border = PIP_BORDER * 0.5;
    bool inside_inner_border = inside(uv,
        vec2(frame_left + PIP_BORDER + inner_border, frame_top + PIP_BORDER + inner_border),
        vec2(frame_right - PIP_BORDER - inner_border, frame_bottom - PIP_BORDER - inner_border));
    if (!inside_inner_border) {
        return CYBER_WHITE;
    }

    if (inside(uv, vec2(pip_left, title_top), vec2(pip_right, title_bottom))) {
        vec4 title_bar = CYBER_YELLOW;
        title_bar.a = 0.95;
        if (fract(uv.y * 200.0) < 0.1) {
            title_bar.rgb *= 0.9;
        }
        return title_bar;
    }

    if (!inside(uv, vec2(pip_left, pip_top), vec2(pip_right, pip_bottom))) {
        return base;
    }

    vec2 pip_uv = vec2(
        (uv.x - pip_left) / (pip_right - pip_left),
        (uv.y - pip_top) / (pip_bottom - pip_top)
    );
    vec4 result = over(texture(pose_from_skeleton_overlay, pip_uv), CYBER_DARK);

    const float corner_length = 0.015;
    const float techmark_thickness = 0.002;
    float corner_x = corner_length / PIP_WIDTH;
    float corner_y = corner_length / PIP_HEIGHT;
    float thickness_x = techmark_thickness / PIP_WIDTH;
    float thickness_y = techmark_thickness / PIP_HEIGHT;

    bool on_a_corner_techmark =
        (pip_uv.x < corner_x && pip_uv.y < thickness_y) || (pip_uv.x < thickness_x && pip_uv.y < corner_y)
        || (pip_uv.x > 1.0 - corner_x && pip_uv.y < thickness_y) || (pip_uv.x > 1.0 - thickness_x && pip_uv.y < corner_y)
        || (pip_uv.x < corner_x && pip_uv.y > 1.0 - thickness_y) || (pip_uv.x < thickness_x && pip_uv.y > 1.0 - corner_y)
        || (pip_uv.x > 1.0 - corner_x && pip_uv.y > 1.0 - thickness_y) || (pip_uv.x > 1.0 - thickness_x && pip_uv.y > 1.0 - corner_y);
    return on_a_corner_techmark ? CYBER_CYAN : result;
}

void main() {
    // The dark-blue stand-in keeps the window showing something on cold start,
    // before the first camera frame has made it down the chain.
    vec4 result = (pc.present_layer_mask & VIDEO_LAYER_BIT) != 0u
        ? texture(video_from_upstream, screen_uv)
        : vec4(0.05, 0.05, 0.12, 1.0);

    if ((pc.present_layer_mask & OVERLAY_LAYER_BIT) != 0u) {
        result = over(texture(overlay_from_neon_source, screen_uv), result);
    }
    if ((pc.present_layer_mask & POSE_LAYER_BIT) != 0u && pc.pip_slide_progress > 0.0) {
        result = draw_pip_frame(screen_uv, pc.pip_slide_progress, result);
    }

    composited_colour = result;
}

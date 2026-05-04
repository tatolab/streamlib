// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// 4-layer Porter-Duff "over" compositor with animated PiP frame chrome.
//
// Inputs are sampled textures bound at descriptor-set 0 — the hardware
// sampler handles tiled-format access and (for the PiP layer) bilinear
// filtering, replacing the manual byte unpack + nearest-neighbor
// `sample_layer_pixel` / hand-rolled bilinear `sample_pip_bilinear`
// helpers from the pre-RHI compute shader. Output goes to the bound
// color attachment via `out vec4 outColor`.
//
// Layer-size contract: video, lower_third, and watermark must match the
// output's dimensions exactly (sampled at the same screen UV); the PiP
// layer may be any size and is bilinearly downsampled into the PiP
// rect.

#version 450

layout(location = 0) in vec2 inUV;
layout(location = 0) out vec4 outColor;

layout(set = 0, binding = 0) uniform sampler2D videoTex;
layout(set = 0, binding = 1) uniform sampler2D lowerThirdTex;
layout(set = 0, binding = 2) uniform sampler2D watermarkTex;
layout(set = 0, binding = 3) uniform sampler2D pipTex;

layout(push_constant) uniform PushConstants {
    uint width;
    uint height;
    uint pip_width;
    uint pip_height;
    uint flags;            // bit 0 has_video, bit 1 has_lower_third,
                           // bit 2 has_watermark, bit 3 has_pip
    float pip_slide_progress;
} pc;

// Cyberpunk palette — matches the macOS Metal kernel's chrome.
const vec4 CYBER_CYAN   = vec4(0.0,   0.94,  1.0,   1.0);
const vec4 CYBER_YELLOW = vec4(0.988, 0.933, 0.039, 1.0);
const vec4 CYBER_WHITE  = vec4(1.0,   1.0,   1.0,   1.0);
const vec4 CYBER_DARK   = vec4(0.06,  0.06,  0.08,  0.95);

// PiP geometry (UV space, fraction of screen).
const float PIP_WIDTH        = 0.28;
const float PIP_HEIGHT       = 0.35;
const float PIP_MARGIN       = 0.02;
const float PIP_BORDER       = 0.004;
const float TITLE_BAR_HEIGHT = 0.045;

bool in_rect(vec2 uv, vec2 lo, vec2 hi) {
    return uv.x >= lo.x && uv.x <= hi.x && uv.y >= lo.y && uv.y <= hi.y;
}

// Cyberpunk N54 News PiP frame (border + title bar + content + corner techmarks).
// Slides in from the right; `slide_progress` 0 → fully off-screen, 1 → docked.
vec4 draw_pip_frame(vec2 uv, float slide_progress, vec4 base) {
    float slide_offset = (1.0 - slide_progress) * (PIP_WIDTH + PIP_MARGIN + 0.1);

    float pip_left   = 1.0 - PIP_MARGIN - PIP_WIDTH + slide_offset;
    float pip_right  = 1.0 - PIP_MARGIN + slide_offset;
    float pip_top    = PIP_MARGIN;
    float pip_bottom = PIP_MARGIN + PIP_HEIGHT;

    float title_top    = pip_bottom;
    float title_bottom = pip_bottom + TITLE_BAR_HEIGHT;

    float frame_left   = pip_left   - PIP_BORDER;
    float frame_right  = pip_right  + PIP_BORDER;
    float frame_top    = pip_top    - PIP_BORDER;
    float frame_bottom = title_bottom + PIP_BORDER;

    if (!in_rect(uv, vec2(frame_left, frame_top), vec2(frame_right, frame_bottom))) {
        return base;
    }

    bool outer_inner = in_rect(uv,
        vec2(frame_left + PIP_BORDER, frame_top + PIP_BORDER),
        vec2(frame_right - PIP_BORDER, frame_bottom - PIP_BORDER));
    if (!outer_inner) {
        return CYBER_CYAN;
    }

    float inner_border = PIP_BORDER * 0.5;
    bool inner_inner = in_rect(uv,
        vec2(frame_left + PIP_BORDER + inner_border, frame_top + PIP_BORDER + inner_border),
        vec2(frame_right - PIP_BORDER - inner_border, frame_bottom - PIP_BORDER - inner_border));
    if (!inner_inner) {
        return CYBER_WHITE;
    }

    if (in_rect(uv, vec2(pip_left, title_top), vec2(pip_right, title_bottom))) {
        vec4 title = CYBER_YELLOW;
        title.a = 0.95;
        float scan = fract(uv.y * 200.0);
        if (scan < 0.1) {
            title.rgb *= 0.9;
        }
        return title;
    }

    if (in_rect(uv, vec2(pip_left, pip_top), vec2(pip_right, pip_bottom))) {
        vec4 result = CYBER_DARK;

        vec2 pip_uv = vec2(
            (uv.x - pip_left) / (pip_right - pip_left),
            (uv.y - pip_top)  / (pip_bottom - pip_top)
        );

        // Hardware-bilinear sample of the PiP source — replaces the
        // hand-rolled bilinear from the pre-RHI compute shader.
        vec4 pip_color = texture(pipTex, pip_uv);
        result.rgb = pip_color.rgb + result.rgb * (1.0 - pip_color.a);
        result.a   = pip_color.a   + result.a   * (1.0 - pip_color.a);

        const float corner_size  = 0.015;
        const float tech_thick   = 0.002;
        float cs_x = corner_size / PIP_WIDTH;
        float cs_y = corner_size / PIP_HEIGHT;
        float tt_x = tech_thick  / PIP_WIDTH;
        float tt_y = tech_thick  / PIP_HEIGHT;

        if ((pip_uv.x < cs_x && pip_uv.y < tt_y) || (pip_uv.x < tt_x && pip_uv.y < cs_y)) {
            result = CYBER_CYAN;
        }
        if ((pip_uv.x > 1.0 - cs_x && pip_uv.y < tt_y) || (pip_uv.x > 1.0 - tt_x && pip_uv.y < cs_y)) {
            result = CYBER_CYAN;
        }
        if ((pip_uv.x < cs_x && pip_uv.y > 1.0 - tt_y) || (pip_uv.x < tt_x && pip_uv.y > 1.0 - cs_y)) {
            result = CYBER_CYAN;
        }
        if ((pip_uv.x > 1.0 - cs_x && pip_uv.y > 1.0 - tt_y) || (pip_uv.x > 1.0 - tt_x && pip_uv.y > 1.0 - cs_y)) {
            result = CYBER_CYAN;
        }
        return result;
    }

    return base;
}

void main() {
    bool has_video        = (pc.flags & 1u) != 0u;
    bool has_lower_third  = (pc.flags & 2u) != 0u;
    bool has_watermark    = (pc.flags & 4u) != 0u;
    bool has_pip          = (pc.flags & 8u) != 0u;

    // Base layer: video, or the dark-blue fallback before any camera
    // frame has arrived (matches the macOS Metal kernel's pre-frame
    // placeholder so the swapchain shows something on cold start).
    vec4 result = has_video
        ? texture(videoTex, inUV)
        : vec4(0.05, 0.05, 0.12, 1.0);

    if (has_lower_third) {
        vec4 src = texture(lowerThirdTex, inUV);
        result.rgb = src.rgb + result.rgb * (1.0 - src.a);
        result.a   = src.a   + result.a   * (1.0 - src.a);
    }
    if (has_watermark) {
        vec4 src = texture(watermarkTex, inUV);
        result.rgb = src.rgb + result.rgb * (1.0 - src.a);
        result.a   = src.a   + result.a   * (1.0 - src.a);
    }
    if (has_pip && pc.pip_slide_progress > 0.0) {
        result = draw_pip_frame(inUV, pc.pip_slide_progress, result);
    }

    outColor = result;
}

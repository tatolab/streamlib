// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

// Three-layer Porter-Duff "over" compositor with animated picture-in-picture
// chrome, plus the finishing pass that makes the UI belong to the same broken
// screen as the video: a faint interlace, a light global vignette, and an
// intermittent full-frame glitch burst that displaces EVERYTHING — video and
// interface alike, the way 2077's HUD malfunctions with the world.
//
// Every source is premultiplied: the overlay generator hands over a
// premultiplied skia canvas and the avatar's stage arrives opaque, so
// `src.rgb + dst.rgb * (1 - src.a)` is the whole blend.
//
// Layer-size contract: the video and overlay layers are sampled at the same
// screen UV as the output and must match its extent. The avatar layer may be any
// size — it is bilinearly resampled into the PiP rect by the hardware sampler.

struct FullscreenTriangleVertexToFragment {
    float4 clip_position : SV_Position;
    float2 screen_uv : SCREEN_UV;
};

[[vk::binding(0, 0)]] Sampler2D video_from_upstream;
[[vk::binding(1, 0)]] Sampler2D overlay_from_neon_source;
[[vk::binding(2, 0)]] Sampler2D avatar_from_pose_scene;

struct BreakingNewsCompositePushConstants {
    float2 frame_extent_in_pixels;
    // Bit 0 the video layer, bit 1 the overlay layer, bit 2 the avatar layer.
    uint present_layer_mask;
    // 0.0 fully off-screen right, 1.0 docked; the easing may overshoot past 1.
    float pip_slide_progress;
    float elapsed_seconds;
};
[[vk::push_constant]] ConstantBuffer<BreakingNewsCompositePushConstants> pc;

static const uint VIDEO_LAYER_BIT = 1u;
static const uint OVERLAY_LAYER_BIT = 2u;
static const uint POSE_LAYER_BIT = 4u;

static const float4 CYBER_CYAN = float4(0.0, 0.94, 1.0, 1.0);
static const float4 CYBER_YELLOW = float4(0.988, 0.933, 0.039, 1.0);
static const float4 CYBER_WHITE = float4(1.0, 1.0, 1.0, 1.0);
static const float4 CYBER_DARK = float4(0.06, 0.06, 0.08, 0.95);

// PiP geometry, as a fraction of the screen.
static const float PIP_WIDTH = 0.28;
static const float PIP_HEIGHT = 0.35;
static const float PIP_MARGIN = 0.02;
static const float PIP_BORDER = 0.004;
static const float TITLE_BAR_HEIGHT = 0.045;

// The glitch burst cadence: one window per period, its start jittered inside,
// nothing before the UI has finished sliding in.
static const float GLITCH_PERIOD_SECONDS = 4.7;
static const float GLITCH_BURST_SECONDS = 0.22;
static const float GLITCH_QUIET_LEAD_SECONDS = 2.5;

float hash11(float p) {
    p = frac(p * 0.1031);
    p *= p + 33.33;
    p *= p + p;
    return frac(p);
}

bool inside(float2 uv, float2 low_corner, float2 high_corner) {
    return uv.x >= low_corner.x && uv.x <= high_corner.x
        && uv.y >= low_corner.y && uv.y <= high_corner.y;
}

float4 over(float4 source, float4 destination) {
    return float4(
        source.rgb + destination.rgb * (1.0 - source.a),
        source.a + destination.a * (1.0 - source.a)
    );
}

// 1.0 inside this moment's glitch burst, 0.0 outside it.
float glitch_burst_now() {
    float period_index = floor(pc.elapsed_seconds / GLITCH_PERIOD_SECONDS);
    float burst_start = period_index * GLITCH_PERIOD_SECONDS
        + hash11(period_index + 17.0) * (GLITCH_PERIOD_SECONDS - GLITCH_BURST_SECONDS);
    float in_burst = step(burst_start, pc.elapsed_seconds)
        * step(pc.elapsed_seconds, burst_start + GLITCH_BURST_SECONDS);
    return in_burst * step(GLITCH_QUIET_LEAD_SECONDS, pc.elapsed_seconds);
}

// Horizontal slice displacement for the whole frame — UI included.
float2 glitch_displaced(float2 uv, float burst) {
    if (burst < 0.5) {
        return uv;
    }
    float burst_seed = floor(pc.elapsed_seconds * 24.0);
    float slice_index = floor(uv.y * 28.0);
    float slice_random = hash11(slice_index * 1.7 + burst_seed);
    // Only some slices tear; the rest hold, which is what reads as
    // malfunction rather than blur.
    float slice_tears = step(0.66, slice_random);
    float offset = (hash11(slice_index + burst_seed * 3.1) - 0.5) * 0.055;
    return float2(uv.x + offset * slice_tears, uv.y);
}

float4 draw_pip_frame(float2 uv, float slide_progress, float4 base) {
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

    if (!inside(uv, float2(frame_left, frame_top), float2(frame_right, frame_bottom))) {
        return base;
    }

    bool inside_outer_border = inside(uv,
        float2(frame_left + PIP_BORDER, frame_top + PIP_BORDER),
        float2(frame_right - PIP_BORDER, frame_bottom - PIP_BORDER));
    if (!inside_outer_border) {
        return CYBER_CYAN;
    }

    float inner_border = PIP_BORDER * 0.5;
    bool inside_inner_border = inside(uv,
        float2(frame_left + PIP_BORDER + inner_border, frame_top + PIP_BORDER + inner_border),
        float2(frame_right - PIP_BORDER - inner_border, frame_bottom - PIP_BORDER - inner_border));
    if (!inside_inner_border) {
        return CYBER_WHITE;
    }

    if (inside(uv, float2(pip_left, title_top), float2(pip_right, title_bottom))) {
        float4 title_bar = CYBER_YELLOW;
        title_bar.a = 0.95;
        if (frac(uv.y * 200.0) < 0.1) {
            title_bar.rgb *= 0.9;
        }
        // A brighter band sweeping the title bar, endlessly.
        float across_the_bar = (uv.x - pip_left) / max(pip_right - pip_left, 1e-6);
        float sweep = smoothstep(0.14, 0.0, abs(across_the_bar - frac(pc.elapsed_seconds * 0.4)));
        title_bar.rgb *= 1.0 + 0.22 * sweep;
        return title_bar;
    }

    if (!inside(uv, float2(pip_left, pip_top), float2(pip_right, pip_bottom))) {
        return base;
    }

    float2 pip_uv = float2(
        (uv.x - pip_left) / (pip_right - pip_left),
        (uv.y - pip_top) / (pip_bottom - pip_top)
    );

    // A faint cyan dot grid for the moments before the avatar's first frame
    // arrives — its stage covers this fully once it does.
    float4 pip_backdrop = CYBER_DARK;
    float2 grid_cell = frac(pip_uv * float2(36.0, 22.0)) - 0.5;
    float grid_dot = smoothstep(0.11, 0.05, length(grid_cell));
    pip_backdrop.rgb += CYBER_CYAN.rgb * 0.05 * grid_dot;

    float4 result = over(avatar_from_pose_scene.Sample(pip_uv), pip_backdrop);

    static const float corner_length = 0.015;
    static const float techmark_thickness = 0.002;
    float corner_x = corner_length / PIP_WIDTH;
    float corner_y = corner_length / PIP_HEIGHT;
    float thickness_x = techmark_thickness / PIP_WIDTH;
    float thickness_y = techmark_thickness / PIP_HEIGHT;

    bool on_a_corner_techmark =
        (pip_uv.x < corner_x && pip_uv.y < thickness_y) || (pip_uv.x < thickness_x && pip_uv.y < corner_y)
        || (pip_uv.x > 1.0 - corner_x && pip_uv.y < thickness_y) || (pip_uv.x > 1.0 - thickness_x && pip_uv.y < corner_y)
        || (pip_uv.x < corner_x && pip_uv.y > 1.0 - thickness_y) || (pip_uv.x < thickness_x && pip_uv.y > 1.0 - corner_y)
        || (pip_uv.x > 1.0 - corner_x && pip_uv.y > 1.0 - thickness_y) || (pip_uv.x > 1.0 - thickness_x && pip_uv.y > 1.0 - corner_y);
    if (on_a_corner_techmark) {
        // The techmarks breathe rather than sit still.
        float4 techmark = CYBER_CYAN;
        techmark.rgb *= 0.75 + 0.25 * sin(pc.elapsed_seconds * 3.0);
        return techmark;
    }
    return result;
}

float4 main(FullscreenTriangleVertexToFragment fragment) : SV_Target0 {
    float burst = glitch_burst_now();
    float2 uv = glitch_displaced(fragment.screen_uv, burst);

    // The dark-blue stand-in keeps the window showing something on cold start,
    // before the first camera frame has made it down the chain.
    float4 result;
    if ((pc.present_layer_mask & VIDEO_LAYER_BIT) != 0u) {
        result = video_from_upstream.Sample(uv);
        // The burst splits the video's channels; the UI layers over it stay
        // whole, so the interface reads as glitching WITH the feed, not as
        // three copies of itself.
        if (burst > 0.5) {
            float split = 0.0045;
            result.r = video_from_upstream.Sample(uv + float2(split, 0.0)).r;
            result.b = video_from_upstream.Sample(uv - float2(split, 0.0)).b;
        }
    } else {
        result = float4(0.05, 0.05, 0.12, 1.0);
    }

    if ((pc.present_layer_mask & OVERLAY_LAYER_BIT) != 0u) {
        result = over(overlay_from_neon_source.Sample(uv), result);
    }
    if ((pc.present_layer_mask & POSE_LAYER_BIT) != 0u && pc.pip_slide_progress > 0.0) {
        result = draw_pip_frame(uv, pc.pip_slide_progress, result);
    }

    // Cyan interference lines riding the burst, over everything.
    if (burst > 0.5) {
        float line_noise = hash11(floor(pc.elapsed_seconds * 30.0) * 7.0
            + floor(uv.y * pc.frame_extent_in_pixels.y));
        if (line_noise > 0.982) {
            result.rgb += float3(0.0, 0.25, 0.25);
        }
    }

    // The finishing pass: a faint interlace so the whole frame reads as one
    // screen, and a light vignette pulling the corners down under the HUD.
    float interlace = lerp(0.955, 1.0, step(1.0, fmod(fragment.clip_position.y, 2.0)));
    result.rgb *= interlace;
    float2 uv_from_centre = fragment.screen_uv * 2.0 - 1.0;
    result.rgb *= clamp(1.0 - dot(uv_from_centre, uv_from_centre) * 0.16, 0.78, 1.0);

    return result;
}

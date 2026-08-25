// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Draws a neon COCO-17 skeleton over a transparent background, from keypoints
// the processor detected and handed over in push constants.
//
// Bones are capsule distance fields and joints are discs, both with a soft
// glow falloff — a fragment stage can draw geometry no other way here, because
// no vertex or index buffer is reachable from a Python processor.
//
// WIRE CONTRACT — `pose_skeleton_overlay.py` packs this block byte for byte.
// Each keypoint is one `packUnorm2x16` of its frame-normalized (x, y), which
// is what keeps 17 of them plus the rest inside the 128 bytes of push-constant
// space Vulkan guarantees; a `vec2` array would need 136 for the keypoints
// alone. Bit i of `visible_keypoint_mask` is set when keypoint i cleared the
// detector's confidence floor.

#version 450

layout(location = 0) in vec2 screen_uv;
layout(location = 0) out vec4 skeleton_colour;

layout(push_constant, std430) uniform PoseSkeletonPushConstants {
    uint packed_keypoints[17];
    uint visible_keypoint_mask;
    vec2 frame_extent_in_pixels;
    float elapsed_seconds;
} pc;

const uint KEYPOINT_COUNT = 17u;
const uint BONE_COUNT = 19u;

// The COCO-17 skeleton, as ultralytics numbers it: 0 nose, 1/2 eyes, 3/4 ears,
// 5/6 shoulders, 7/8 elbows, 9/10 wrists, 11/12 hips, 13/14 knees, 15/16 ankles.
const uvec2 BONES[19] = uvec2[19](
    uvec2(15u, 13u), uvec2(13u, 11u), uvec2(16u, 14u), uvec2(14u, 12u),
    uvec2(11u, 12u), uvec2(5u, 11u),  uvec2(6u, 12u),  uvec2(5u, 6u),
    uvec2(5u, 7u),   uvec2(6u, 8u),   uvec2(7u, 9u),   uvec2(8u, 10u),
    uvec2(1u, 2u),   uvec2(0u, 1u),   uvec2(0u, 2u),   uvec2(1u, 3u),
    uvec2(2u, 4u),   uvec2(3u, 5u),   uvec2(4u, 6u)
);

const vec3 NEON_CYAN = vec3(0.0, 0.94, 1.0);
const vec3 NEON_MAGENTA = vec3(1.0, 0.11, 0.72);

bool keypoint_is_visible(uint keypoint_index) {
    return (pc.visible_keypoint_mask & (1u << keypoint_index)) != 0u;
}

vec2 keypoint_in_pixels(uint keypoint_index) {
    return unpackUnorm2x16(pc.packed_keypoints[keypoint_index]) * pc.frame_extent_in_pixels;
}

float distance_to_segment(vec2 point, vec2 segment_start, vec2 segment_end) {
    vec2 along = segment_end - segment_start;
    float length_squared = dot(along, along);
    if (length_squared < 1e-6) {
        return distance(point, segment_start);
    }
    float travel = clamp(dot(point - segment_start, along) / length_squared, 0.0, 1.0);
    return distance(point, segment_start + travel * along);
}

// One stroke's core plus its glow, as a 0..1 coverage.
float stroke_coverage(float distance_in_pixels, float core_radius, float glow_radius) {
    float core = 1.0 - smoothstep(core_radius - 1.0, core_radius + 1.0, distance_in_pixels);
    float glow = 1.0 - smoothstep(core_radius, glow_radius, distance_in_pixels);
    return clamp(core + glow * 0.45, 0.0, 1.0);
}

void main() {
    vec2 fragment_in_pixels = screen_uv * pc.frame_extent_in_pixels;

    // Strokes scale with the frame so the skeleton reads the same at any
    // capture resolution.
    float stroke_scale = pc.frame_extent_in_pixels.y / 1080.0;
    float pulse = 0.85 + 0.15 * sin(pc.elapsed_seconds * 4.0);

    float bone_coverage = 0.0;
    for (uint bone = 0u; bone < BONE_COUNT; bone++) {
        uint from_keypoint = BONES[bone].x;
        uint to_keypoint = BONES[bone].y;
        if (!keypoint_is_visible(from_keypoint) || !keypoint_is_visible(to_keypoint)) {
            continue;
        }
        float distance_in_pixels = distance_to_segment(
            fragment_in_pixels,
            keypoint_in_pixels(from_keypoint),
            keypoint_in_pixels(to_keypoint)
        );
        bone_coverage = max(
            bone_coverage,
            stroke_coverage(distance_in_pixels, 3.5 * stroke_scale, 11.0 * stroke_scale)
        );
    }

    float joint_coverage = 0.0;
    for (uint keypoint = 0u; keypoint < KEYPOINT_COUNT; keypoint++) {
        if (!keypoint_is_visible(keypoint)) {
            continue;
        }
        float distance_in_pixels = distance(fragment_in_pixels, keypoint_in_pixels(keypoint));
        joint_coverage = max(
            joint_coverage,
            stroke_coverage(distance_in_pixels, 6.0 * stroke_scale, 16.0 * stroke_scale)
        );
    }

    // Joints sit over bones, so the magenta wins where they overlap.
    vec3 colour = mix(NEON_CYAN, NEON_MAGENTA, joint_coverage);
    float coverage = max(bone_coverage, joint_coverage) * pulse;

    // Premultiplied against the coverage: the compositor blends this
    // source-over, and an unpremultiplied edge would fringe towards black.
    skeleton_colour = vec4(colour * coverage, coverage);
}

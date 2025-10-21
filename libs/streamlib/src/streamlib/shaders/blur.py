"""
Blur effect shaders for WebGPU.

These shaders implement various blur algorithms optimized for GPU execution.
All shaders follow the streamlib convention:
- Binding 0: Input texture (texture_2d<f32>)
- Binding 1: Output texture (texture_storage_2d<rgba8unorm, write>)
"""

# Simple box blur - fast but lower quality
BOX_BLUR_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    // Boundary check
    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    // 3x3 box blur kernel
    var color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    var samples = 0.0;

    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let sample_coord = coord + vec2<i32>(dx, dy);
            // Check bounds
            if (sample_coord.x >= 0 && sample_coord.x < i32(dims.x) &&
                sample_coord.y >= 0 && sample_coord.y < i32(dims.y)) {
                color += textureLoad(input_texture, sample_coord, 0);
                samples += 1.0;
            }
        }
    }

    // Average the samples
    color = color / samples;

    textureStore(output_texture, coord, color);
}
"""

# Gaussian blur - higher quality, separable implementation
GAUSSIAN_BLUR_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

// Gaussian weights for 5x5 kernel (sigma = 1.0)
// Precomputed for performance
const WEIGHTS: array<f32, 5> = array<f32, 5>(
    0.227027, 0.1945946, 0.1216216, 0.054054, 0.016216
);

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    // Boundary check
    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    var color = textureLoad(input_texture, coord, 0) * WEIGHTS[0];

    // Apply Gaussian blur horizontally and vertically
    for (var i = 1; i < 5; i++) {
        let offset = i;
        let weight = WEIGHTS[i];

        // Horizontal samples
        let left = coord - vec2<i32>(offset, 0);
        let right = coord + vec2<i32>(offset, 0);

        if (left.x >= 0) {
            color += textureLoad(input_texture, left, 0) * weight;
        }
        if (right.x < i32(dims.x)) {
            color += textureLoad(input_texture, right, 0) * weight;
        }

        // Vertical samples
        let up = coord - vec2<i32>(0, offset);
        let down = coord + vec2<i32>(0, offset);

        if (up.y >= 0) {
            color += textureLoad(input_texture, up, 0) * weight;
        }
        if (down.y < i32(dims.y)) {
            color += textureLoad(input_texture, down, 0) * weight;
        }
    }

    textureStore(output_texture, coord, color);
}
"""

# Default blur shader (alias for box blur for simplicity)
BLUR_SHADER = BOX_BLUR_SHADER